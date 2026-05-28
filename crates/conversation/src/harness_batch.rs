//! Server-side batching for harness progress events.
//!
//! When the harness is wired to a chat session, individual task progress
//! events would create a torrent of micro-turns. The batcher buckets events
//! by session and, after a quiet window (default 10s), flushes them as a
//! single deterministic assistant turn appended to the conversation.
//!
//! Per `memory:llm_as_ux_shim_pattern` the batcher does NOT call the LLM —
//! it formats events into plain prose so the cost is bounded and the surface
//! stays predictable. A future iteration can swap in an LLM-mediated turn
//! generator if the deterministic version reads as too mechanical.
//!
//! The queue + debounce + drop-oldest machinery lives in
//! the generic [`crate::batcher::Batcher`] shell. This module is the
//! `HarnessEvent` impl on top: it owns the `HarnessFlushSink` that
//! persists the synthetic turn + fans out the `turn_appended` SSE
//! event.
//!
//! The `BatcherConfig` re-exported from this module remains the
//! HarnessBatcher-flavored config (no `max_events` parameter — the
//! 512-event cap is the production fleet-wide constant `HARNESS_BATCH_MAX_EVENTS`).
//! Internally it's translated to a `crate::batcher::BatcherConfig`
//! on construction.

use crate::batcher::{Batcher as GenericBatcher, BatcherConfig as GenericBatcherConfig, FlushSink};
use crate::metrics::MetricsStore;
use crate::persistence::SessionStore;
use crate::session::{AssistantIntent, HarnessEvent, SessionId, Turn};
use chrono::Utc;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

/// cap on the per-session in-window queue. A runaway agent
/// firing thousands of events before the 10s flush completes would
/// otherwise balloon memory; drop-oldest keeps the newest events
/// (which are the most interesting for terminal-state reconstruction)
/// and increments a counter the Metrics tab surfaces.
pub const HARNESS_BATCH_MAX_EVENTS: usize = 512;

/// Configuration for the `HarnessBatcher` debounce behaviour.
#[derive(Debug, Clone)]
pub struct BatcherConfig {
    /// Quiet-period window before the batcher flushes a session's events.
    pub window: Duration,
    /// event `kind` strings that trigger an immediate flush
    /// rather than waiting for the full `window`. Defaults cover the
    /// terminal events an SME wants to see right away: a task
    /// finishing, failing, or the whole execution completing. Started
    /// events still ride out the debounce window so a burst of
    /// fan-outs collapses into one turn.
    pub flush_on_event_kinds: BTreeSet<String>,
}

impl Default for BatcherConfig {
    fn default() -> Self {
        let mut flush_on_event_kinds = BTreeSet::new();
        flush_on_event_kinds.insert("task_completed".into());
        flush_on_event_kinds.insert("task_failed".into());
        flush_on_event_kinds.insert("execution_finished".into());
        Self {
            window: Duration::from_secs(default_window_secs()),
            flush_on_event_kinds,
        }
    }
}

/// Default debounce window in seconds. Read out so it can be reused by
/// `from_env()` and by docs-as-contract gates.
pub const DEFAULT_HARNESS_BATCH_WINDOW_SECS: u64 = 10;

const fn default_window_secs() -> u64 {
    DEFAULT_HARNESS_BATCH_WINDOW_SECS
}

/// Pure parser separated from the env-read so it can be
/// unit-tested without mutating process environment. Returns `None`
/// (caller falls back to default) for empty / zero / out-of-range /
/// non-numeric inputs; returns `Some(Duration)` for sensible overrides.
pub(crate) fn parse_window_value(raw: &str) -> Option<Duration> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed.parse::<u64>() {
        Ok(0) => None,
        Ok(secs) if secs > 600 => None,
        Ok(secs) => Some(Duration::from_secs(secs)),
        Err(_) => None,
    }
}

impl BatcherConfig {
    /// Read `ECAA_HARNESS_BATCH_WINDOW_SECS` from the
    /// environment and construct a `BatcherConfig` whose debounce window
    /// reflects it. Out-of-range or non-numeric values fall back to the
    /// default and emit a tracing warning so misconfiguration surfaces
    /// without breaking the server boot path.
    ///
    /// - Unset / empty → 10s default.
    /// - 0 → tracing warning (a 0s window collapses every event into its
    ///   own turn — almost never what an operator wants); falls back to
    ///   default.
    /// - \> 600 → tracing warning (debounces longer than 10 minutes hide
    ///   blockers from the SME for too long); falls back to default.
    /// - Otherwise → that value, in seconds.
    pub fn from_env() -> Self {
        let mut cfg = Self::default();
        if let Ok(raw) = std::env::var("ECAA_HARNESS_BATCH_WINDOW_SECS") {
            match parse_window_value(&raw) {
                Some(window) => cfg.window = window,
                None if !raw.trim().is_empty() => {
                    tracing::warn!(
                        target: "harness_batch",
                        raw = %raw,
                        "ECAA_HARNESS_BATCH_WINDOW_SECS={} rejected (must be 1..=600); \
                         falling back to {}s default",
                        raw,
                        DEFAULT_HARNESS_BATCH_WINDOW_SECS
                    );
                }
                None => {}
            }
        }
        cfg
    }

    /// Lift a HarnessBatcher-flavored config to the generic
    /// shell config — the only difference is the (constant) `max_events`
    /// cap, which the generic config parameterizes.
    fn into_generic(self) -> GenericBatcherConfig {
        GenericBatcherConfig::new(self.window)
            .with_flush_kinds(self.flush_on_event_kinds)
            .with_max_events(HARNESS_BATCH_MAX_EVENTS)
    }
}

/// Persistence + SSE-fanout sink for harness events. Built by
/// `HarnessBatcher::build_sink()` after the (optional) `with_metrics` /
/// `with_event_sink` builders have populated the corresponding fields.
/// The generic `Batcher` shell calls `flush()` once per debounce
/// window or terminal-kind event; `record_dropped()` bumps the
/// per-session `batch_dropped_events` metric when drop-oldest fires.
struct HarnessFlushSink {
    store: SessionStore,
    metrics: Option<MetricsStore>,
    event_sink: Option<Arc<dyn crate::service::ServiceEventSink>>,
}

#[async_trait::async_trait]
impl FlushSink<SessionId, HarnessEvent> for HarnessFlushSink {
    async fn flush(&self, session_id: SessionId, events: Vec<HarnessEvent>) {
        if events.is_empty() {
            return;
        }
        let summary = format_events(&events);
        let turn = Turn {
            turn_id: uuid::Uuid::new_v4(),
            role: crate::session::TurnRole::Assistant,
            content: summary,
            intent: Some(AssistantIntent::PostEmission),
            tool_calls: vec![],
            quick_replies: vec![],
            confirmation_card: None,
            timestamp: Utc::now(),
        };

        let _ = self
            .store
            .update(session_id, |s| {
                s.harness_events.extend(events.iter().cloned());
                Arc::make_mut(&mut s.conversation).push(turn.clone());
                Ok(())
            })
            .await;

        // emit turn_appended so the UI appends the synthetic
        // harness-summary turn locally instead of waiting for the 60 s
        // transcript poll. Fires after the persistence round-trip so
        // the turn is durable before the event goes out.
        if let Some(sink) = &self.event_sink {
            sink.turn_appended(session_id, &turn);
        }
    }

    async fn record_dropped(&self, session_id: SessionId, dropped: u64) {
        if let Some(metrics) = &self.metrics {
            metrics.record_batch_dropped(session_id, dropped).await;
        }
    }
}

/// Thin wrapper that preserves the `HarnessBatcher` API while
/// delegating queue + debounce + drop-oldest to the generic
/// `Batcher<SessionId, HarnessEvent, HarnessFlushSink>` shell.
///
/// Construction is two-phase: the public `new` + `with_metrics` +
/// `with_event_sink` builders accumulate config + sink wiring on a
/// pre-Arc value; the first `enqueue` (or `flush*`) call lazily
/// initializes the inner generic batcher via the held `OnceLock`.
/// This preserves the legacy builder pattern (`HarnessBatcher::new(s, c).with_metrics(m)`)
/// without requiring callers to thread the optionals through the
/// constructor.
#[derive(Clone)]
pub struct HarnessBatcher {
    config: BatcherConfig,
    store: SessionStore,
    /// optional metrics store for recording drop-oldest events.
    /// Tests that don't care about drop telemetry can leave this None.
    metrics: Option<MetricsStore>,
    /// optional event sink so the synthetic turn `flush`
    /// appends can ride the `turn_appended` SSE channel alongside
    /// tool-loop turns. Without this, harness-driven turns would
    /// only surface on the UI's 60 s transcript poll.
    event_sink: Option<Arc<dyn crate::service::ServiceEventSink>>,
    /// Lazily-initialised generic shell. Constructed on the first
    /// `enqueue`/`flush*` call so the builder methods can still mutate
    /// `metrics` / `event_sink` between `HarnessBatcher::new(...)` and
    /// `Arc::new(...)`.
    inner: Arc<std::sync::OnceLock<Arc<GenericBatcher<SessionId, HarnessEvent, HarnessFlushSink>>>>,
}

impl HarnessBatcher {
    /// Create a new batcher with the given session store and config.
    pub fn new(store: SessionStore, config: BatcherConfig) -> Self {
        Self {
            config,
            store,
            metrics: None,
            event_sink: None,
            inner: Arc::new(std::sync::OnceLock::new()),
        }
    }

    /// Create a batcher using the default debounce window from env/config.
    pub fn with_default_window(store: SessionStore) -> Self {
        Self::new(store, BatcherConfig::default())
    }

    /// wire a ServiceEventSink so synthetic turns appended by
    /// `flush` surface over SSE as `turn_appended` events. The server
    /// passes in the same sink used by the ConversationService.
    pub fn with_event_sink(mut self, sink: Arc<dyn crate::service::ServiceEventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// wire a MetricsStore so queue-overflow drops bump the
    /// `batch_dropped_events` counter on SessionMetrics. Builder-style
    /// so tests and the production constructor stay terse.
    pub fn with_metrics(mut self, metrics: MetricsStore) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Initialise (idempotent) and return the generic shell. Threads
    /// the wrapper's accumulated config + optional metrics + optional
    /// SSE sink into a `HarnessFlushSink` and constructs the
    /// `Batcher<SessionId, HarnessEvent, ...>`.
    fn shell(&self) -> Arc<GenericBatcher<SessionId, HarnessEvent, HarnessFlushSink>> {
        self.inner
            .get_or_init(|| {
                let sink = HarnessFlushSink {
                    store: self.store.clone(),
                    metrics: self.metrics.clone(),
                    event_sink: self.event_sink.clone(),
                };
                Arc::new(GenericBatcher::new(
                    self.config.clone().into_generic(),
                    Arc::new(sink),
                ))
            })
            .clone()
    }

    /// Enqueue an event for the given session. If this is the first event in
    /// the bucket, a flush task is spawned to drain it after the configured
    /// window.
    ///
    /// if the event's `kind` is in `config.flush_on_event_kinds`
    /// (task_completed / task_failed / execution_finished by default), a
    /// flush task is spawned immediately so the SME doesn't wait the full
    /// 10s window on terminal events. Any delayed flush already pending
    /// remains a no-op when the queue has been drained.
    pub async fn enqueue(self: Arc<Self>, session_id: SessionId, event: HarnessEvent) {
        let shell = self.shell();
        shell.enqueue(session_id, event).await
    }

    /// Flush the accumulated events for a session into a synthetic assistant
    /// turn. Idempotent if the queue is empty.
    pub async fn flush(self: Arc<Self>, session_id: SessionId) {
        let shell = self.shell();
        shell.flush(session_id).await
    }

    /// Flush every pending session into the session store.
    ///
    /// Callers that own process shutdown must invoke this explicitly.
    /// `Drop` is log-only because it cannot safely perform async
    /// persistence when a Tokio runtime is already shutting down.
    pub async fn flush_pending(self: Arc<Self>) {
        let shell = self.shell();
        shell.flush_pending().await
    }
}

impl Drop for HarnessBatcher {
    fn drop(&mut self) {
        if let Some(shell) = self.inner.get() {
            match shell.try_pending_summary() {
                Ok((pending_sessions, pending_events)) => {
                    if pending_events > 0 {
                        tracing::warn!(
                            pending_sessions,
                            pending_events,
                            "HarnessBatcher dropped with pending events; call flush_pending() during graceful shutdown"
                        );
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        "HarnessBatcher dropped while its queue lock was held; call flush_pending() before drop for durability"
                    );
                }
            }
        }
    }
}

fn format_events(events: &[HarnessEvent]) -> String {
    if events.len() == 1 {
        let e = &events[0];
        return format_single(e);
    }
    let mut out = String::from("Quick update on the running analysis:\n");
    // Collapse contiguous duplicates by (kind, task_id, detail). Heartbeat
    // ticks across overlapping stall watchers can post the same trio many
    // times inside one batcher window — without this guard the synthetic
    // turn renders six identical "• heartbeat_age: heartbeat_age_secs=29"
    // bullets in a row, which is noise the SME can't act on. Distinct
    // events (task_started followed by task_completed for the same task,
    // or the same kind for different tasks) are preserved unchanged.
    let mut prev: Option<(&str, &str, &str)> = None;
    for e in events {
        let key = (e.kind.as_str(), e.task_id.as_str(), e.detail.as_str());
        if prev == Some(key) {
            continue;
        }
        out.push_str("• ");
        out.push_str(&format_event_bullet(e));
        out.push('\n');
        prev = Some(key);
    }
    out
}

/// `HarnessEvent` ⇒ `BatchableEvent`. The trait gives the
/// generic `Batcher<HarnessEvent>` shell access to the same
/// kind/single/bullet formatters this module's `format_*` helpers
/// expose internally. The shell itself doesn't call these renderers
/// directly (it leaves rendering to `HarnessFlushSink::flush` so
/// the singular-vs-plural decision can see the whole batch); the impl
/// is preserved so future Batcher consumers that DO want
/// per-event rendering (image-build progress, eval-runner output) can
/// reuse the same trait surface.
impl crate::batcher::BatchableEvent for HarnessEvent {
    fn kind(&self) -> &str {
        self.kind.as_str()
    }
    fn render_single(&self) -> String {
        format_single(self)
    }
    fn render_bullet(&self) -> String {
        format_event_bullet(self)
    }
    fn is_terminal(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "task_completed" | "task_failed" | "task_blocked" | "execution_finished"
        )
    }
}

fn format_single(e: &HarnessEvent) -> String {
    let detail = crate::sme_text::sanitize_for_sme(&e.detail);
    let body = match e.kind.as_str() {
        "task_started" => format!("Started: {}.", detail),
        "task_completed" => format!("Finished: {}.", detail),
        "task_failed" => format!("That step ran into trouble: {}.", detail),
        "task_blocked" => format!("That step is paused: {}.", detail),
        "execution_finished" => {
            "All steps are complete. Let me know what you'd like to do next.".into()
        }
        _ => format!("{}: {}", e.kind, detail),
    };
    append_remote_tag(body, e)
}

fn format_event_bullet(e: &HarnessEvent) -> String {
    let verb = match e.kind.as_str() {
        "task_started" => "started",
        "task_completed" => "finished",
        "task_failed" => "failed",
        "task_blocked" => "paused",
        "execution_finished" => "done",
        _ => e.kind.as_str(),
    };
    let detail = crate::sme_text::sanitize_for_sme(&e.detail);
    let body = if detail.is_empty() {
        verb.to_string()
    } else {
        format!("{}: {}", verb, detail)
    };
    append_remote_tag(body, e)
}

/// When the harness reports a remote
/// backend (AWS, GCP, …) the batcher surfaces the backend + instance
/// type alongside the prose so the SME sees the compute shape without
/// having to open the Jobs tab. Local-mode events (no `remote` field)
/// are unchanged so existing fixtures stay byte-identical.
fn append_remote_tag(body: String, e: &HarnessEvent) -> String {
    match &e.remote {
        Some(info) => format!("{} [{} · {}]", body, info.backend, info.instance_type),
        None => body,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::SessionStore;
    use crate::session::Session;

    /// Return the tempdir's RAII guard alongside the store so
    /// the caller binds it for the duration of the test. The store
    /// keeps a path reference into the tempdir; the `Arc<TempDir>`
    /// keeps the dir alive until every clone is dropped.
    async fn make_store() -> (SessionStore, std::sync::Arc<tempfile::TempDir>) {
        let dir = std::sync::Arc::new(tempfile::tempdir().unwrap());
        let store = SessionStore::open(dir.path()).await.unwrap();
        (store, dir)
    }

    fn ev(kind: &str, task_id: &str, detail: &str) -> HarnessEvent {
        HarnessEvent {
            kind: kind.into(),
            task_id: task_id.into(),
            status: "running".into(),
            detail: detail.into(),
            remote: None,
            timestamp: Utc::now(),
        }
    }

    fn ev_aws(kind: &str, task_id: &str, detail: &str, instance_type: &str) -> HarnessEvent {
        HarnessEvent {
            kind: kind.into(),
            task_id: task_id.into(),
            status: "running".into(),
            detail: detail.into(),
            remote: Some(crate::session::RemoteExecutionInfo {
                backend: "aws".into(),
                instance_id: "i-deadbeef".into(),
                instance_type: instance_type.into(),
            }),
            timestamp: Utc::now(),
        }
    }

    #[test]
    fn format_single_local_has_no_tag() {
        let e = ev("task_started", "alignment", "STAR run");
        assert_eq!(format_single(&e), "Started: STAR run.");
    }

    /// `parse_window_value` is the pure parser; the env
    /// read in `from_env` adds nothing beyond `std::env::var` on top of
    /// it. Testing the parser directly avoids the cross-test mutation
    /// of process env (and the unsafe-block discipline `unsafe_code =
    /// "deny"` enforces in this crate).
    #[test]
    fn parse_window_value_honors_override_and_falls_back_on_bad_values() {
        // override
        assert_eq!(
            parse_window_value("25"),
            Some(Duration::from_secs(25)),
            "valid value should pass through"
        );
        // empty
        assert_eq!(parse_window_value(""), None, "empty falls back");
        // zero
        assert_eq!(parse_window_value("0"), None, "0 falls back");
        // ceiling
        assert_eq!(parse_window_value("9999"), None, ">600 falls back");
        // garbage
        assert_eq!(
            parse_window_value("not-a-number"),
            None,
            "garbage falls back"
        );
        // boundary
        assert_eq!(
            parse_window_value("600"),
            Some(Duration::from_secs(600)),
            "ceiling itself is allowed"
        );
    }

    #[test]
    fn format_single_remote_tags_backend_and_instance_type() {
        let e = ev_aws("task_started", "alignment", "STAR run", "r6i.4xlarge");
        assert_eq!(format_single(&e), "Started: STAR run. [aws · r6i.4xlarge]");
    }

    #[test]
    fn format_event_bullet_remote_tags_backend_and_instance_type() {
        let e = ev_aws("task_completed", "clustering", "Leiden 0.8", "r6i.8xlarge");
        assert_eq!(
            format_event_bullet(&e),
            "finished: Leiden 0.8 [aws · r6i.8xlarge]"
        );
    }

    #[test]
    fn format_events_collapses_contiguous_duplicate_heartbeats() {
        // Six identical heartbeat_age ticks (same task_id, same detail)
        // arriving inside one batcher window must render as a single
        // bullet, not six. Two distinct task_started events for different
        // tasks are kept separately, so the dedup only fires on true
        // contiguous duplicates.
        let events = vec![
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("heartbeat_age", "discover_diff", "heartbeat_age_secs=29"),
            ev("task_started", "qc", "QC: counting reads"),
            ev("task_started", "norm", "Normalization: VST"),
        ];
        let out = format_events(&events);
        // The bullet renders as `• heartbeat_age: heartbeat_age_secs=29`,
        // so check the bullet prefix instead of the literal kind (the kind
        // string appears twice per bullet: in the verb and in the detail).
        assert_eq!(
            out.matches("• heartbeat_age:").count(),
            1,
            "rendered: {out}"
        );
        assert!(
            out.contains("• started: QC: counting reads"),
            "rendered: {out}"
        );
        assert!(
            out.contains("• started: Normalization: VST"),
            "rendered: {out}"
        );
    }

    #[test]
    fn format_events_preserves_distinct_events_for_different_tasks() {
        // Distinct task_ids with the same kind must each render their own
        // bullet — dedup keys on (kind, task_id, detail) so we don't lose
        // separate per-task progress.
        let events = vec![
            ev("task_completed", "qc", "QC done"),
            ev("task_completed", "norm", "Norm done"),
            ev("task_completed", "de", "DE done"),
        ];
        let out = format_events(&events);
        assert_eq!(out.matches("• finished:").count(), 3, "rendered: {out}");
    }

    #[tokio::test]
    async fn flush_appends_a_synthetic_assistant_turn() {
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let batcher = Arc::new(HarnessBatcher::new(
            store.clone(),
            BatcherConfig {
                window: Duration::from_millis(50),
                ..BatcherConfig::default()
            },
        ));
        batcher
            .clone()
            .enqueue(session.id, ev("task_started", "qc", "QC: counting reads"))
            .await;
        batcher
            .clone()
            .enqueue(session.id, ev("task_completed", "qc", "QC: counting reads"))
            .await;

        // Wait for the flush task
        tokio::time::sleep(Duration::from_millis(150)).await;

        let s = store.get(session.id).await.unwrap();
        assert!(
            !s.harness_events.is_empty(),
            "harness_events should be populated after flush"
        );
        let last_turn = s
            .conversation
            .last()
            .expect("conversation should have a turn");
        assert_eq!(last_turn.role, crate::session::TurnRole::Assistant);
        assert!(last_turn.content.contains("Quick update"));
    }

    #[tokio::test]
    async fn single_event_flush_uses_singular_phrasing() {
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let batcher = Arc::new(HarnessBatcher::new(
            store.clone(),
            BatcherConfig {
                window: Duration::from_millis(40),
                ..BatcherConfig::default()
            },
        ));
        batcher
            .clone()
            .enqueue(session.id, ev("execution_finished", "", ""))
            .await;

        tokio::time::sleep(Duration::from_millis(120)).await;

        let s = store.get(session.id).await.unwrap();
        let last_turn = s.conversation.last().unwrap();
        assert!(last_turn.content.contains("All steps are complete"));
    }

    #[tokio::test]
    async fn empty_flush_is_idempotent() {
        let (store, _env) = make_store().await;
        let batcher = Arc::new(HarnessBatcher::with_default_window(store));
        // Should not panic.
        batcher.flush(uuid::Uuid::new_v4()).await;
    }

    #[tokio::test]
    async fn task_completed_flushes_without_waiting_for_window() {
        // terminal events shouldn't wait the full 10s debounce
        // window. A `task_completed` enqueue must surface as an assistant
        // turn well before the window elapses.
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let batcher = Arc::new(HarnessBatcher::new(
            store.clone(),
            BatcherConfig {
                window: Duration::from_secs(10),
                ..BatcherConfig::default()
            },
        ));
        batcher
            .clone()
            .enqueue(session.id, ev("task_completed", "qc", "counting reads"))
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;

        let s = store.get(session.id).await.unwrap();
        let last_turn = s
            .conversation
            .last()
            .expect("immediate-flush event should have produced a turn");
        assert_eq!(last_turn.role, crate::session::TurnRole::Assistant);
        assert!(last_turn.content.contains("Finished"));
    }

    #[tokio::test]
    async fn task_started_still_waits_the_full_window() {
        // non-terminal events continue to ride out the debounce
        // window so a fan-out burst collapses into one turn.
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let batcher = Arc::new(HarnessBatcher::new(
            store.clone(),
            BatcherConfig {
                window: Duration::from_millis(500),
                ..BatcherConfig::default()
            },
        ));
        batcher
            .clone()
            .enqueue(session.id, ev("task_started", "qc", "counting reads"))
            .await;

        tokio::time::sleep(Duration::from_millis(100)).await;
        let early = store.get(session.id).await.unwrap();
        assert!(
            early.conversation.is_empty(),
            "task_started should not flush within 100ms of a 500ms window"
        );

        tokio::time::sleep(Duration::from_millis(500)).await;
        let late = store.get(session.id).await.unwrap();
        assert!(
            !late.conversation.is_empty(),
            "task_started should flush after the window elapses"
        );
    }

    #[tokio::test]
    async fn overflow_drops_oldest_and_records_metrics_counter() {
        // plan spec: "Enqueue 1000 events in one window; assert
        // oldest dropped, counter == 488, flushed turn still rendered."
        // 1000 events with HARNESS_BATCH_MAX_EVENTS=512 → 488 dropped.
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let metrics = MetricsStore::new();
        let batcher = Arc::new(
            HarnessBatcher::new(
                store.clone(),
                BatcherConfig {
                    window: Duration::from_millis(50),
                    ..BatcherConfig::default()
                },
            )
            .with_metrics(metrics.clone()),
        );

        // Seed a turn counter so snapshot() returns Some. Without this
        // MetricsStore::snapshot returns None for sessions that only
        // have drop counters set.
        metrics
            .record_turn(
                session.id,
                Duration::from_millis(1),
                0,
                0,
                0,
                0,
                0,
                crate::model_policy::ModelId::Sonnet46,
            )
            .await;

        const BURST: usize = 1000;
        let expected_dropped = (BURST - HARNESS_BATCH_MAX_EVENTS) as u64;
        assert_eq!(expected_dropped, 488, "plan spec sanity check");

        for i in 0..BURST {
            batcher
                .clone()
                .enqueue(
                    session.id,
                    ev("task_started", &format!("t{}", i), &format!("step {}", i)),
                )
                .await;
        }

        // Wait past the window (plus slack for the flush task to run).
        tokio::time::sleep(Duration::from_millis(200)).await;

        let snap = metrics.snapshot(session.id).await.unwrap();
        assert_eq!(snap.batch_dropped_events, expected_dropped);

        let s = store.get(session.id).await.unwrap();
        // The synthetic turn was rendered (plan: "flushed turn still rendered").
        let last = s.conversation.last().expect("flushed turn");
        assert!(last.content.contains(&format!("step {}", BURST - 1)));
        // And the oldest (step 0) was dropped — not in the rendered turn.
        assert!(
            !last.content.contains("step 0\n") && !last.content.ends_with("step 0"),
            "oldest event must have been dropped"
        );
        // The flushed queue contained the retained tail only.
        assert_eq!(s.harness_events.len(), HARNESS_BATCH_MAX_EVENTS);
    }

    // SME-copy linter mirror. Feeds jargon-rich agent-provided detail
    // strings through format_single / format_event_bullet and asserts
    // the resulting SME-visible prose contains none of the forbidden
    // tokens the UI linter also checks.
    #[test]
    fn sme_copy_linter_catches_leaks_from_format_single() {
        let forbidden = [
            r"discover_",
            r"validate_",
            r"harness",
            r"executor",
            r"Jobs tab",
            r"State tab",
            r"tool_call",
            r"runtime/",
            r"results/tables/",
        ];
        // Jargon-rich detail the agent might write — every forbidden
        // token shows up. A clean sanitizer rewrites all of them.
        let e = HarnessEvent {
            kind: "task_completed".into(),
            task_id: "discover_normalization".into(),
            status: "completed".into(),
            detail: "discover_normalization emitted \
                runtime/outputs/discover_normalization/decision.json; \
                the harness picked vst after a validate_qc pass. See \
                the Jobs tab for tool_call history in \
                results/tables/norm_scores.tsv."
                .into(),
            remote: None,
            timestamp: Utc::now(),
        };
        let out = format_single(&e);
        for token in &forbidden {
            assert!(
                !out.contains(token),
                "format_single output contains forbidden token {:?}: {:?}",
                token,
                out
            );
        }
    }

    #[test]
    fn sme_copy_linter_catches_leaks_from_format_event_bullet() {
        let forbidden = ["discover_", "harness", "runtime/", "tool_call"];
        let e = HarnessEvent {
            kind: "task_started".into(),
            task_id: "validate_integration".into(),
            status: "running".into(),
            detail: "the harness routed discover_batch_correction to \
                runtime/outputs/discover_batch_correction via tool_call"
                .into(),
            remote: None,
            timestamp: Utc::now(),
        };
        let out = format_event_bullet(&e);
        for token in &forbidden {
            assert!(
                !out.contains(token),
                "format_event_bullet output contains forbidden token {:?}: {:?}",
                token,
                out
            );
        }
    }

    #[tokio::test]
    async fn burst_within_window_collapses_to_one_turn() {
        let (store, _env) = make_store().await;
        let session = Session::new(false);
        store.save(&session).await.unwrap();

        let batcher = Arc::new(HarnessBatcher::new(
            store.clone(),
            BatcherConfig {
                window: Duration::from_millis(100),
                ..BatcherConfig::default()
            },
        ));
        for i in 0..5 {
            batcher
                .clone()
                .enqueue(
                    session.id,
                    ev("task_completed", &format!("t{}", i), &format!("step {}", i)),
                )
                .await;
        }

        tokio::time::sleep(Duration::from_millis(220)).await;

        let s = store.get(session.id).await.unwrap();
        // One synthetic turn should be appended even though five events came in
        let synthetic_turns: Vec<_> = s
            .conversation
            .iter()
            .filter(|t| {
                t.role == crate::session::TurnRole::Assistant
                    && t.intent == Some(AssistantIntent::PostEmission)
            })
            .collect();
        assert_eq!(synthetic_turns.len(), 1);
        assert_eq!(s.harness_events.len(), 5);
    }
}
