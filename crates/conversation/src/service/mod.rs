//! Public conversation service: starts sessions and drives the tool-use
//! loop on the user's behalf.
//!
//! Split across submodules by concern (plan §S5.9 modularity cap):
//!
//! - `tool_loop` — the tool-use dispatch loop (cap 10, soft-landing at 7) + auto-append
//! - `retry` — `ServiceError` + the retry-exhausted fallback turn
//! - `greeting` — the static greeting-turn constructor
//! - `transitions` — deterministic state-transition methods (confirm/reject/unblock/branch/amend/rerun/inject_infra_error)
//! - `send_turn` — the per-turn lifecycle (`send_turn`, `metrics_snapshot`, `get_session`, `maybe_auto_title`, `project_remaining_cost`)
//!
//! `mod.rs` is a thin shell holding the struct + new() + sink wiring +
//! cross-module accessor helpers. All non-trivial behavior lives in the
//! submodules above.

mod greeting;
mod retry;
mod send_turn;
mod tool_loop;
mod transitions;

use crate::anthropic::LlmBackend;
use crate::metrics::MetricsStore;
use crate::persistence::SessionStore;
use crate::session::{Session, SessionId};
use dashmap::{DashMap, DashSet};
use scripps_workflow_core::llm_availability::LlmAvailability;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

pub use greeting::greeting_turn;
pub use retry::ServiceError;
pub use tool_loop::{SOFT_LANDING_ITERATION, TOOL_LOOP_CAP, TOOL_PILL_THRESHOLD};
pub use transitions::{AmendResult, AutoEmitOutcome};

/// Optional event sink the service uses to publish tool-call boundaries
/// to a downstream SSE broadcaster. The server passes one in to surface
/// `ToolCallStarted` / `ToolCallFinished` to the UI's status pill.
pub trait ServiceEventSink: Send + Sync {
    /// Notify that a tool call has started.
    fn tool_call_started(&self, session_id: SessionId, tool_name: &str, status_line: &str);
    /// Notify that a tool call has finished.
    fn tool_call_finished(&self, session_id: SessionId, tool_name: &str);
    /// Broadcast an incremental assistant text delta (streaming).
    fn assistant_token_delta(&self, session_id: SessionId, text: &str);
    /// Notify that the session advanced to a new state.
    fn state_advanced(&self, session_id: SessionId, new_state: &crate::session::SessionState);
    /// the LLM fired StartExecution after the SME said
    /// "yes, start it" in chat. The server sink spawns the harness
    /// subprocess and starts tracking it. Default implementation is a
    /// no-op so existing tests/servers don't need to implement this.
    fn execution_requested(
        &self,
        _session_id: SessionId,
        _agent_path: Option<String>,
        _max_iterations: Option<u32>,
    ) {
    }
    /// a new `Turn` was just committed to the session. The
    /// server sink forwards this to the SSE broadcaster as a
    /// `TurnAppended` event so UIs can append the turn locally
    /// instead of polling `getChatTranscript` every 15 s. Default
    /// no-op for tests/mocks that don't wire the sink.
    fn turn_appended(&self, _session_id: SessionId, _turn: &crate::session::Turn) {}
    /// the `rerun_task` tool has just reset a completed task
    /// back to Pending/Ready, so any cached artifact listing for
    /// that (session, task) is stale. Server wires this to
    /// `ChatAppState::invalidate_artifact_cache_for_task`. The
    /// dir-mtime invalidator on the cache already catches rerun
    /// writes; this hook is a belt-and-suspenders so the stale
    /// entry is dropped even before the agent rewrites artifacts.
    fn task_reset(&self, _session_id: SessionId, _task_id: &str) {}
    /// `propose_hypothesized_node` just inserted a new
    /// proposal into the session-scoped registry. Server forwards
    /// this to `SsePayload::ProposalReceived` so the UI can mount the
    /// progress card.
    ///
    /// Default no-op so CLI / test paths without an SSE broadcaster
    /// don't need to implement it.
    fn proposal_received(
        &self,
        _session_id: SessionId,
        _proposal_id: &scripps_workflow_core::hypothesized_proposal::ProposalId,
        _node_id: &str,
    ) {
    }
    /// The proposal gate runner just produced a
    /// `GateOutcome`. Fires once per gate that actually ran (validator
    /// and/or sandbox); a `passed=false` outcome means the proposal
    /// transitioned to `Blocked`. Server forwards to
    /// `SsePayload::ProposalGateAdvanced`.
    ///
    /// Default no-op so CLI / test paths without an SSE broadcaster
    /// don't need to implement it.
    fn proposal_gate_advanced(
        &self,
        _session_id: SessionId,
        _proposal_id: &scripps_workflow_core::hypothesized_proposal::ProposalId,
        _gate: scripps_workflow_core::hypothesized_proposal::GateName,
        _passed: bool,
    ) {
    }
}

/// Registry of per-session async mutexes that serialize `send_turn`
/// against concurrent invocations for the SAME session. The persistence
/// layer's sync closure mutex only covers `store.update` writes; the
/// tool loop itself (including async file I/O in `emit_package`) runs
/// on a cloned local session OUTSIDE that lock. Two concurrent
/// `send_turn` calls therefore can both enter the tool loop in
/// `ReadyToEmit`, both pass `try_transition(EmitPackageStart)` on their
/// local copies, and both call `copy_plotting_library` — producing the
/// ENOTEMPTY race. Holding a per-session mutex across the whole turn
/// fixes the root cause. Different sessions never contend on each other.
pub type SessionTurnMutex = Arc<AsyncMutex<()>>;
pub(crate) type SessionTurnMutexRegistry = Arc<DashMap<SessionId, SessionTurnMutex>>;

/// Main service object that owns the LLM backend, session store, metrics,
/// and per-session turn serialization mutexes.
pub struct ConversationService {
    llm: Arc<dyn LlmBackend>,
    store: SessionStore,
    config_dir: PathBuf,
    event_sink: Option<Arc<dyn ServiceEventSink>>,
    metrics: MetricsStore,
    /// Per-session turn mutex — serializes `send_turn` against
    /// concurrent calls for the same session while letting different
    /// sessions run in parallel.
    turn_locks: SessionTurnMutexRegistry,
    /// v3 P10 — per-session cached `LlmAvailability`. Detected at session
    /// start from the process env; refreshed when a live Anthropic call
    /// observes a transient `Unavailable` error so the UI can re-mount
    /// the structured-form fallback without a server restart. Lock-free
    /// `DashMap` because reads/writes are O(1) snapshot/clone operations
    /// and the per-session lookup is on the hot path of every turn.
    availability: Arc<DashMap<SessionId, LlmAvailability>>,
    /// Per-session in-flight claim set for the Haiku auto-title side-call.
    /// `maybe_auto_title` calls `insert` and bails when the SessionId was
    /// already present — making the claim ATOMIC with the spawn decision.
    /// Without this, two concurrent `send_turn` calls both observe
    /// `session.title.is_none()` and both spawn a Haiku call (the second
    /// is pure waste — only one title persists). The spawned task removes
    /// the SessionId via `AutoTitleInFlightGuard` on completion.
    pub(crate) auto_title_in_flight: Arc<DashSet<SessionId>>,
}

impl ConversationService {
    /// Construct a new service with the given backend, session store, and config directory.
    pub fn new(llm: Arc<dyn LlmBackend>, store: SessionStore, config_dir: PathBuf) -> Self {
        // MetricsStore persists sidecar *.metrics.json files in the
        // session store dir so the UI's Metrics tab survives a
        // server restart mid-session. Before this, MetricsStore was
        // in-memory only and /metrics returned 404 after any restart.
        let metrics_dir = store.dir().to_path_buf();
        let turn_locks: SessionTurnMutexRegistry = Arc::new(DashMap::new());
        let availability: Arc<DashMap<SessionId, LlmAvailability>> = Arc::new(DashMap::new());
        let auto_title_in_flight: Arc<DashSet<SessionId>> = Arc::new(DashSet::new());
        let turn_locks_for_hook = Arc::clone(&turn_locks);
        let availability_for_hook = Arc::clone(&availability);
        let auto_title_for_hook = Arc::clone(&auto_title_in_flight);
        store.set_prune_hook(move |id| {
            turn_locks_for_hook.remove(&id);
            availability_for_hook.remove(&id);
            auto_title_for_hook.remove(&id);
        });
        Self {
            llm,
            store,
            config_dir,
            event_sink: None,
            metrics: MetricsStore::new().with_persist_dir(metrics_dir),
            turn_locks,
            availability,
            auto_title_in_flight,
        }
    }

    /// Acquire (or create) the per-session turn mutex. Held across the
    /// full `send_turn` body so concurrent invocations for the same
    /// session serialize properly — preventing the race where two
    /// local-session copies both reach `emit_package`.
    fn session_turn_lock(&self, id: SessionId) -> SessionTurnMutex {
        self.turn_locks
            .entry(id)
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .value()
            .clone()
    }

    /// Attach an event sink for SSE broadcasting.
    pub fn with_event_sink(mut self, sink: Arc<dyn ServiceEventSink>) -> Self {
        self.event_sink = Some(sink);
        self
    }

    /// Create a fresh session and return its id plus the static greeting.
    pub async fn start_session(
        &self,
        careful_mode: bool,
    ) -> Result<(SessionId, crate::session::Turn), ServiceError> {
        let mut session = Session::new(careful_mode);
        let greeting = greeting_turn();
        Arc::make_mut(&mut session.conversation).push(greeting.clone());
        // v3 P10 — snapshot the env-detected availability into the
        // per-session cache so the UI's `GET /llm-availability` returns
        // the same answer the conversation service will act on for the
        // rest of the session's lifecycle.
        let id = session.id;
        self.store
            .save(&session)
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;
        self.set_llm_availability(id, LlmAvailability::detect_from_env());
        Ok((id, greeting))
    }

    /// Create a fresh session and deterministically apply the first
    /// intake prose without invoking the LLM. Used by the structured
    /// intake fallback when chat is disabled or unavailable.
    pub async fn start_session_from_prose(
        &self,
        careful_mode: bool,
        prose: String,
    ) -> Result<(SessionId, crate::session::Turn), ServiceError> {
        let prose = prose.trim().to_string();
        if prose.is_empty() {
            return Err(ServiceError::Internal(
                "structured intake prose is empty".to_string(),
            ));
        }

        let (id, greeting) = self.start_session(careful_mode).await?;
        let config_dir = self.config_dir.clone();
        let store = self.store_handle();
        store
            .update(id, |session| {
                let mut next = session.clone();
                Arc::make_mut(&mut next.conversation)
                    .push(crate::session::Turn::user(prose.clone()));
                next.last_activity = chrono::Utc::now();
                next.try_transition(crate::session::StateTrigger::AppendProse)
                    .map_err(anyhow::Error::from)?;

                let result = crate::tools::append_intake_prose(&mut next, &prose, &config_dir);
                if result.is_error {
                    let reason = serde_json::from_value::<crate::errors::ToolError>(result.content)
                        .map(|err| err.short_reason())
                        .unwrap_or_else(|_| "append_intake_prose failed".to_string());
                    return Err(anyhow::anyhow!(reason));
                }

                *session = next;
                Ok(())
            })
            .await
            .map_err(|e| ServiceError::Internal(e.to_string()))?;

        Ok((id, greeting))
    }

    /// Snapshot every persisted session. Used by the title-bar
    /// "Recent ▼" dropdown so SMEs can jump back into a workflow they
    /// navigated away from. Caller sorts/filters as needed.
    pub async fn iter_sessions(&self) -> Vec<Session> {
        self.store.iter_sessions().await
    }

    /// Listing-grade metadata projection of
    /// `iter_sessions`. Backed by the persistence-layer in-memory
    /// cache so the title-bar "Recent ▼" dropdown and SessionTree
    /// children query don't pay the cost of a full disk scan +
    /// Session-shape deserialize per request.
    pub async fn iter_session_metadata(&self) -> Vec<crate::persistence::SessionMetadata> {
        self.store.iter_session_metadata().await
    }

    /// list every session whose lineage points at
    /// `parent_id`. Order is by `created_at` ascending so the
    /// SessionTree UI can render the children chronologically.
    pub async fn children_of(&self, parent_id: SessionId) -> Vec<Session> {
        let mut out = self.store.iter_sessions().await;
        out.retain(|s| {
            s.lineage
                .as_ref()
                .map(|l| l.parent_session_id == parent_id)
                .unwrap_or(false)
        });
        out.sort_by_key(|s| s.created_at);
        out
    }

    /// Metadata-only `children_of`. Returns the
    /// projection the `/sessions?parent=` endpoint actually renders
    /// without paying for full Session deserialization per child.
    pub async fn children_of_metadata(
        &self,
        parent_id: SessionId,
    ) -> Vec<crate::persistence::SessionMetadata> {
        let mut out = self.store.iter_session_metadata().await;
        out.retain(|m| m.parent_session_id == Some(parent_id));
        out.sort_by_key(|m| m.created_at);
        out
    }

    /// Expose the underlying SessionStore for runner / batcher integration
    /// tests that need to share state with a `HarnessBatcher`. Not exposed
    /// to external callers because production code constructs both pieces
    /// at the server layer (see `chat_routes::ChatAppState::new`).
    #[doc(hidden)]
    /// Expose the MetricsStore so chat_routes can record per-task
    /// instance_seconds when the harness reports a remote executor on
    /// a progress event.
    pub fn metrics(&self) -> &crate::metrics::MetricsStore {
        &self.metrics
    }

    /// Return a clone of the session store handle.
    pub fn store_handle(&self) -> SessionStore {
        self.store.clone()
    }

    /// Expose the underlying LlmBackend for scorer calls made by
    /// `chat_routes::sessions::score_session`. The scorer shares the
    /// same backend as the chat tool loop so `MockLlmBackend` in tests
    /// and `AnthropicClient` in production flow through one pipe.
    pub fn llm_for_scoring(&self) -> Arc<dyn LlmBackend> {
        self.llm.clone()
    }

    // Accessors used by submodules (`send_turn.rs`, `tool_loop.rs`,
    // `transitions.rs`) so they can read private state on the service
    // struct without requiring `mod.rs` to expose the fields. Kept
    // `pub(super)` so external callers don't get to reach in.
    pub(super) fn llm(&self) -> &Arc<dyn LlmBackend> {
        &self.llm
    }
    pub(super) fn event_sink(&self) -> &Option<Arc<dyn ServiceEventSink>> {
        &self.event_sink
    }
    pub(super) fn config_dir(&self) -> &PathBuf {
        &self.config_dir
    }
    /// `send_turn.rs` reads the metrics store via this
    /// accessor instead of the private field.
    pub(super) fn metrics_store(&self) -> &MetricsStore {
        &self.metrics
    }
    /// `send_turn.rs` acquires the per-session turn
    /// mutex through this thin wrapper around the private
    /// `session_turn_lock` so it doesn't have to know about the
    /// internal registry shape.
    pub(super) fn session_turn_lock_handle(&self, id: SessionId) -> SessionTurnMutex {
        self.session_turn_lock(id)
    }

    /// v3 P10 — return the cached `LlmAvailability` for the given session.
    /// If nothing is cached (the session was created before this field
    /// was wired or it has not yet exchanged a turn), fall back to a
    /// fresh env-driven detection so callers still get a useful answer.
    pub fn llm_availability(&self, session_id: SessionId) -> LlmAvailability {
        if let Some(av) = self.availability.get(&session_id) {
            return av.value().clone();
        }
        LlmAvailability::detect_from_env()
    }

    /// v3 P10 — overwrite the cached availability for a session. Called
    /// by `start_session` (initial detection) and by the tool loop when
    /// an Anthropic call observes a transient `Unavailable` so the UI's
    /// next `/llm-availability` poll surfaces the change.
    pub fn set_llm_availability(&self, session_id: SessionId, availability: LlmAvailability) {
        self.availability.insert(session_id, availability);
    }

    #[doc(hidden)]
    pub fn turn_locks_for_test(&self) -> SessionTurnMutexRegistry {
        Arc::clone(&self.turn_locks)
    }

    #[doc(hidden)]
    pub fn availability_for_test(&self) -> Arc<DashMap<SessionId, LlmAvailability>> {
        Arc::clone(&self.availability)
    }

    #[doc(hidden)]
    pub fn session_turn_lock_for_test(&self, id: SessionId) -> SessionTurnMutex {
        self.session_turn_lock(id)
    }
}

#[cfg(test)]
mod tests;
