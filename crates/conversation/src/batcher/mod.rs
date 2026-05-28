//! Generic event-batcher trait + shell.
//!
//! `HarnessBatcher` (in `harness_batch.rs`) was the right shape but
//! the wrong type — it coupled debounce + drop-oldest + flush to
//! `HarnessEvent` specifically. A second producer (image-build
//! progress, eval-runner streaming, scorer worker output) would have
//! to fork the same machinery.
//!
//! This module factors out the producer-agnostic primitives:
//! - [`BatchableEvent`] — what every event must offer for batching
//!   (kind, single/bullet renderers, terminal flag).
//! - [`BatcherConfig`] — the debounce window + flush-trigger set
//! + drop-oldest cap, hoisted off `HarnessEvent`.
//! - [`Batcher`] — the generic shell. Owns the per-key queue map +
//!   debounce timer; defers persistence/SSE-fanout to a
//!   [`FlushSink`] callback the caller supplies.
//!
//! `HarnessBatcher` stays as wiring: it implements `FlushSink` to
//! persist `HarnessEvent`s onto the session and emit the synthetic
//! turn. A future second impl (e.g. `ImageBuildBatcher`) implements
//! `FlushSink` for its own event type and reuses `Batcher` unchanged.
//!
//! See `harness_batch.rs` for the production wiring. The plan-level
//! end-state is `pub type HarnessBatcher = Batcher<HarnessEvent>;`
//! plus `impl BatchableEvent for HarnessEvent` — that migration is
//! incremental; this module is the foundation.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// The contract every batcher event must offer. Three
/// renderers (single / bullet / kind) cover the format-events
/// patterns observed in `harness_batch.rs::format_*`. `is_terminal`
/// lets the shell decide whether the event triggers an immediate
/// flush (today the per-batcher `flush_on_event_kinds` set covers
/// this; new impls can prefer `is_terminal` for cleaner code).
pub trait BatchableEvent: Clone + Send + Sync + 'static {
    /// Event kind for flush-trigger matching (e.g. `task_completed`).
    fn kind(&self) -> &str;
    /// Single-event prose used when the batch holds exactly one
    /// event. The "I started X" line surfacing the lone event.
    fn render_single(&self) -> String;
    /// Per-event bullet used when the batch holds multiple events.
    /// One short verb-led line per call.
    fn render_bullet(&self) -> String;
    /// Whether this event represents a terminal state for its scope
    /// (task_completed/failed/blocked, execution_finished). Callers
    /// may flush immediately on terminal events.
    fn is_terminal(&self) -> bool {
        false
    }
}

/// Generic batcher config. Same field set as
/// `harness_batch::BatcherConfig` but with `max_events` parameterized
/// (the harness shell pinned it at 512 via const). New batchers can
/// pick a tighter cap when their producers emit faster.
#[derive(Debug, Clone)]
pub struct BatcherConfig {
    /// Quiet-period debounce window before the batcher flushes.
    pub window: Duration,
    /// Event kinds that bypass the debounce window and trigger an immediate flush.
    pub flush_on_event_kinds: BTreeSet<String>,
    /// Maximum events buffered per key before drop-oldest eviction.
    pub max_events: usize,
}

impl BatcherConfig {
    /// Create a config with the given debounce window and default limits.
    pub fn new(window: Duration) -> Self {
        Self {
            window,
            flush_on_event_kinds: BTreeSet::new(),
            max_events: 512,
        }
    }

    /// Set the event kinds that trigger an immediate flush.
    pub fn with_flush_kinds<I: IntoIterator<Item = String>>(mut self, kinds: I) -> Self {
        self.flush_on_event_kinds = kinds.into_iter().collect();
        self
    }

    /// Override the maximum buffered events per key (drop-oldest cap).
    pub fn with_max_events(mut self, cap: usize) -> Self {
        self.max_events = cap;
        self
    }
}

#[derive(Debug)]
pub(crate) struct BatchQueue<E> {
    pub events: VecDeque<E>,
    pub first_event_at: Instant,
    pub flush_pending: bool,
}

impl<E> Default for BatchQueue<E> {
    fn default() -> Self {
        Self {
            events: VecDeque::new(),
            first_event_at: Instant::now(),
            flush_pending: false,
        }
    }
}

/// Flush sink supplied by callers. The shell owns
/// queueing + timing; the caller owns "what does flushing mean for my
/// event type?" (write to session, post to chat, push a metric).
///
/// Keeping the sink async lets impls round-trip through the
/// `SessionStore` (or any other persistence layer) without blocking
/// the tokio runtime spawning the flush task.
#[async_trait::async_trait]
pub trait FlushSink<K, E>: Send + Sync + 'static
where
    K: Send + Sync + 'static,
    E: Send + Sync + 'static,
{
    /// Called by the batcher shell to flush a batch of events for `key`.
    async fn flush(&self, key: K, events: Vec<E>);
    /// Optional drop-oldest counter — implementations that don't
    /// surface it can leave the default no-op.
    async fn record_dropped(&self, _key: K, _dropped: u64) {}
}

/// Generic batcher shell. Producers enqueue keyed
/// events; the shell debounces, caps the queue with drop-oldest, and
/// fires the [`FlushSink`] either on the debounce window or
/// immediately when an event's `kind` is in `flush_on_event_kinds`.
///
/// Generic over the queue key (typically `SessionId`) and event type
/// (e.g. `HarnessEvent`). The shell doesn't care about the SSE
/// sink — that's the impl's responsibility.
pub struct Batcher<K, E, S>
where
    K: Eq + Hash + Copy + Send + Sync + 'static,
    E: BatchableEvent,
    S: FlushSink<K, E>,
{
    config: BatcherConfig,
    queues: Arc<Mutex<HashMap<K, BatchQueue<E>>>>,
    sink: Arc<S>,
}

impl<K, E, S> Clone for Batcher<K, E, S>
where
    K: Eq + Hash + Copy + Send + Sync + 'static,
    E: BatchableEvent,
    S: FlushSink<K, E>,
{
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            queues: self.queues.clone(),
            sink: self.sink.clone(),
        }
    }
}

impl<K, E, S> Batcher<K, E, S>
where
    K: Eq + Hash + Copy + Send + Sync + 'static,
    E: BatchableEvent,
    S: FlushSink<K, E>,
{
    /// Create a new batcher with the given config and flush sink.
    pub fn new(config: BatcherConfig, sink: Arc<S>) -> Self {
        Self {
            config,
            queues: Arc::new(Mutex::new(HashMap::new())),
            sink,
        }
    }

    /// Enqueue an event for the given key. Drops oldest events when
    /// the queue is at cap. Spawns a delayed flush on the first
    /// queued event; spawns an immediate flush on terminal-kind
    /// events.
    pub async fn enqueue(self: &Arc<Self>, key: K, event: E) {
        let is_immediate = self.config.flush_on_event_kinds.contains(event.kind());
        let (needs_delayed_flush, dropped) = {
            let mut queues = self.queues.lock().await;
            let queue = queues.entry(key).or_default();
            if queue.events.is_empty() {
                queue.first_event_at = Instant::now();
            }
            let mut dropped = 0u64;
            while queue.events.len() >= self.config.max_events {
                queue.events.pop_front();
                dropped += 1;
            }
            queue.events.push_back(event);
            let needs = if is_immediate {
                false
            } else if !queue.flush_pending {
                queue.flush_pending = true;
                true
            } else {
                false
            };
            (needs, dropped)
        };

        if dropped > 0 {
            self.sink.record_dropped(key, dropped).await;
        }

        if is_immediate {
            let this = self.clone();
            tokio::spawn(async move { this.flush(key).await });
        } else if needs_delayed_flush {
            let this = self.clone();
            let window = self.config.window;
            tokio::spawn(async move {
                tokio::time::sleep(window).await;
                this.flush(key).await;
            });
        }
    }

    /// Flush the queue for a key into the sink. Idempotent: a queue
    /// that's already been drained is a no-op.
    pub async fn flush(self: &Arc<Self>, key: K) {
        let events: Vec<E> = {
            let mut queues = self.queues.lock().await;
            match queues.remove(&key) {
                Some(q) => q.events.into_iter().collect(),
                None => return,
            }
        };
        if events.is_empty() {
            return;
        }
        self.sink.flush(key, events).await;
    }

    /// Flush every pending key. Used by shutdown paths and tests.
    ///
    /// Prefer [`Self::drain_now`] in new graceful-shutdown wiring; this
    /// method is kept for backward compat with existing call sites
    /// (server SIGTERM handler).
    pub async fn flush_pending(self: &Arc<Self>) {
        let keys: Vec<K> = {
            let queues = self.queues.lock().await;
            queues.keys().copied().collect()
        };
        for k in keys {
            self.flush(k).await;
        }
    }

    /// R3.3 — graceful-shutdown drain. Synchronously flushes every
    /// pending batch through the [`FlushSink`] so queued events
    /// aren't lost when the process exits mid-debounce.
    ///
    /// Equivalent to [`Self::flush_pending`] but returns a `Result`
    /// so a graceful-shutdown handler (e.g. server SIGTERM) can log
    /// drain failures without panicking. Since the underlying
    /// `flush` is infallible at the shell level, the success arm is
    /// currently always `Ok(())`; the `Result` is the
    /// forward-compatible shape for adding error reporting later
    /// (e.g. a deadline-exceeded variant if a sink hangs).
    pub async fn drain_now(self: &Arc<Self>) -> anyhow::Result<()> {
        self.flush_pending().await;
        Ok(())
    }

    /// Drop-context summary of unflushed state — number of pending
    /// keys and total events. Uses `try_lock` so callers in sync
    /// contexts (e.g. `Drop` impls) don't deadlock the runtime. Returns
    /// `Ok((n_keys, n_events))` or `Err(())` when the lock is contended.
    /// Wrappers use this to log a structured warning when their `Drop`
    /// observes pending work.
    #[allow(clippy::result_unit_err)]
    pub fn try_pending_summary(&self) -> Result<(usize, usize), ()> {
        match self.queues.try_lock() {
            Ok(queues) => Ok((queues.len(), queues.values().map(|q| q.events.len()).sum())),
            Err(_) => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct TestEvent {
        kind: String,
        body: String,
    }

    impl BatchableEvent for TestEvent {
        fn kind(&self) -> &str {
            &self.kind
        }
        fn render_single(&self) -> String {
            format!("[single] {}: {}", self.kind, self.body)
        }
        fn render_bullet(&self) -> String {
            format!("- {}: {}", self.kind, self.body)
        }
        fn is_terminal(&self) -> bool {
            self.kind == "done"
        }
    }

    struct CollectingSink {
        flushed: tokio::sync::Mutex<Vec<(u32, Vec<TestEvent>)>>,
        dropped: tokio::sync::Mutex<Vec<(u32, u64)>>,
    }

    impl CollectingSink {
        fn new() -> Self {
            Self {
                flushed: tokio::sync::Mutex::new(Vec::new()),
                dropped: tokio::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl FlushSink<u32, TestEvent> for CollectingSink {
        async fn flush(&self, key: u32, events: Vec<TestEvent>) {
            self.flushed.lock().await.push((key, events));
        }
        async fn record_dropped(&self, key: u32, dropped: u64) {
            self.dropped.lock().await.push((key, dropped));
        }
    }

    #[tokio::test]
    async fn debounced_flush_collects_all_queued_events() {
        let sink = Arc::new(CollectingSink::new());
        let cfg = BatcherConfig::new(Duration::from_millis(50));
        let batcher = Arc::new(Batcher::new(cfg, sink.clone()));
        for i in 0..3 {
            batcher
                .enqueue(
                    1,
                    TestEvent {
                        kind: "tick".into(),
                        body: format!("body{i}"),
                    },
                )
                .await;
        }
        // Wait for debounce flush.
        tokio::time::sleep(Duration::from_millis(120)).await;
        let flushed = sink.flushed.lock().await;
        assert_eq!(
            flushed.len(),
            1,
            "expected one flush, got {}",
            flushed.len()
        );
        assert_eq!(flushed[0].0, 1);
        assert_eq!(flushed[0].1.len(), 3, "all 3 events must be flushed");
    }

    #[tokio::test]
    async fn terminal_kind_in_flush_set_fires_immediately() {
        let sink = Arc::new(CollectingSink::new());
        let cfg =
            BatcherConfig::new(Duration::from_secs(60)).with_flush_kinds(["done".to_string()]);
        let batcher = Arc::new(Batcher::new(cfg, sink.clone()));
        batcher
            .enqueue(
                7,
                TestEvent {
                    kind: "tick".into(),
                    body: "warming".into(),
                },
            )
            .await;
        batcher
            .enqueue(
                7,
                TestEvent {
                    kind: "done".into(),
                    body: "ok".into(),
                },
            )
            .await;
        // Brief yield; the immediate flush is spawned, not awaited.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let flushed = sink.flushed.lock().await;
        assert_eq!(
            flushed.len(),
            1,
            "immediate flush on terminal kind, no second debounce flush"
        );
        let (key, events) = &flushed[0];
        assert_eq!(*key, 7);
        assert_eq!(events.len(), 2, "both events flushed together");
    }

    #[tokio::test]
    async fn drop_oldest_at_cap_reports_via_record_dropped() {
        let sink = Arc::new(CollectingSink::new());
        let cfg = BatcherConfig::new(Duration::from_millis(50)).with_max_events(2);
        let batcher = Arc::new(Batcher::new(cfg, sink.clone()));
        for i in 0..5 {
            batcher
                .enqueue(
                    42,
                    TestEvent {
                        kind: "tick".into(),
                        body: format!("e{i}"),
                    },
                )
                .await;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
        let dropped = sink.dropped.lock().await;
        let total: u64 = dropped.iter().map(|(_, n)| *n).sum();
        assert_eq!(
            total, 3,
            "5 events with cap 2 should report 3 drops, got {dropped:?}"
        );
        let flushed = sink.flushed.lock().await;
        let (_, events) = &flushed[0];
        assert_eq!(events.len(), 2, "queue settled at cap");
        assert_eq!(events[0].body, "e3");
        assert_eq!(events[1].body, "e4");
    }

    #[tokio::test]
    async fn flush_pending_drains_every_key() {
        let sink = Arc::new(CollectingSink::new());
        let cfg = BatcherConfig::new(Duration::from_secs(60));
        let batcher = Arc::new(Batcher::new(cfg, sink.clone()));
        for k in [1u32, 2, 3] {
            batcher
                .enqueue(
                    k,
                    TestEvent {
                        kind: "tick".into(),
                        body: format!("k{k}"),
                    },
                )
                .await;
        }
        batcher.flush_pending().await;
        let flushed = sink.flushed.lock().await;
        assert_eq!(flushed.len(), 3, "every key drained");
        let mut keys: Vec<u32> = flushed.iter().map(|(k, _)| *k).collect();
        keys.sort();
        assert_eq!(keys, vec![1, 2, 3]);
    }
}
