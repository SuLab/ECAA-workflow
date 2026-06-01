//! Bridge from `ServiceEventSink` callbacks into the per-session SSE
//! broadcasters.
//!
//! Service callbacks are sync; the fanout body delegates to
//! `tokio::spawn` which then reads the broadcasters `DashMap` lock-free
//! so a parallel writer (lazy-init of a fresh subscriber's channel) no
//! longer drops events on the floor. Token deltas in particular cannot
//! recover from drops; the contention window is per-shard and on the
//! order of microseconds. Owns a weak-ish reference to the rest of
//! `ChatAppState` so `execution_requested` (fired by the StartExecution
//! tool) can spawn the harness. Extracted from `chat_routes/mod.rs` so
//! the top-level module stays merge-only.
//!
//! Every fanout mints a monotonic per-session seq via
//! the shared `sse_seq` `DashMap` so the wire-format `EnvelopedEvent`
//! lets subscribers drop out-of-order deliveries. The seq is minted
//! INSIDE the spawned task immediately before `tx.send`, so seq
//! assignment order equals send order even when a synchronous
//! `ChatAppState::broadcast` races a spawned fanout for the same
//! session — the per-session `AtomicU64::fetch_add` serializes them on
//! whatever executor actually performs the send.

use super::{execution, ChatAppState, EnvelopedEvent, SsePayload};
use dashmap::DashMap;
use ecaa_workflow_conversation::{ServiceEventSink, SessionId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Mint the next monotonic per-session seq from the shared `sse_seq`
/// map. Per-session seqs start at 1 and increment by 1 on each call.
/// `Relaxed` ordering: program-order causality on a per-session atomic
/// is sufficient — cross-session ordering doesn't matter to clients,
/// and `Relaxed` avoids the cross-core cache-line flush `SeqCst` would
/// impose on the SSE hot path. Shared by `BroadcastEventSink::fanout`
/// (mint-at-send), `ChatAppState::spawn_fanout` (mint-at-send), and
/// `ChatAppState::next_sse_seq` (used by the synchronous `broadcast`
/// and the events-stream subscribe path) so all paths share one
/// entry-init rule.
pub(super) fn mint(sse_seq: &DashMap<SessionId, Arc<AtomicU64>>, id: SessionId) -> u64 {
    let entry = sse_seq
        .entry(id)
        .or_insert_with(|| Arc::new(AtomicU64::new(0)))
        .clone();
    entry.fetch_add(1, Ordering::Relaxed) + 1
}

pub(super) struct BroadcastEventSink {
    pub broadcasters: Arc<DashMap<SessionId, broadcast::Sender<EnvelopedEvent>>>,
    /// Shared with `ChatAppState::sse_seq` so the seq
    /// space is consistent across REST-driven broadcasts and
    /// service-driven fanouts.
    pub sse_seq: Arc<DashMap<SessionId, Arc<AtomicU64>>>,
    /// Fanout-success counter. Used by the
    /// `sse_fanout_no_drops` regression test to assert events make it
    /// to the broadcast channel even under subscribe contention.
    pub sse_sent_count: Arc<AtomicU64>,
    /// Bounded permit set shared with `ChatAppState::sse_fanout_sem` so
    /// the sink path can't enqueue unbounded tokio tasks when subscribers
    /// are slow.
    pub sse_fanout_sem: Arc<tokio::sync::Semaphore>,
    /// Counter for fanout drops due to semaphore saturation; shared with
    /// `ChatAppState::sse_fanout_dropped_count`.
    pub sse_fanout_dropped_count: Arc<AtomicU64>,
    pub app: Arc<std::sync::OnceLock<ChatAppState>>,
}

impl BroadcastEventSink {
    fn fanout(&self, id: SessionId, payload: SsePayload) {
        // Wraps the payload in an `EnvelopedEvent` with a per-session
        // monotonic seq and increments the success counter on send.
        // Reads the broadcasters `DashMap` lock-free at the shard level,
        // so contention with a parallel lazy-init writer is microseconds
        // per shard rather than a global write-lock window. Bounded by
        // a 128-permit semaphore so a slow subscriber can't grow the
        // tokio task queue without limit.
        let permit = match self.sse_fanout_sem.clone().try_acquire_owned() {
            Ok(p) => p,
            Err(_) => {
                self.sse_fanout_dropped_count
                    .fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    target: "sse_fanout",
                    session_id = %id,
                    "SSE fanout dropped — semaphore saturated"
                );
                return;
            }
        };
        let broadcasters = self.broadcasters.clone();
        let counter = self.sse_sent_count.clone();
        let sse_seq = self.sse_seq.clone();
        tokio::spawn(async move {
            let _permit = permit; // held across send for the full task
            if let Some(tx) = broadcasters.get(&id).map(|e| e.value().clone()) {
                // Mint at send time so seq order == send order even when
                // a synchronous `ChatAppState::broadcast` races this
                // spawned fanout. The per-session atomic serializes the
                // two paths on whichever executor actually sends first.
                let seq = mint(&sse_seq, id);
                if tx.send(EnvelopedEvent { seq, payload }).is_ok() {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }
}

impl ServiceEventSink for BroadcastEventSink {
    fn tool_call_started(&self, id: SessionId, tool_name: &str, status_line: &str) {
        self.fanout(
            id,
            SsePayload::ToolCallStarted {
                tool_name: tool_name.into(),
                status_line: status_line.into(),
            },
        );
    }

    fn tool_call_finished(&self, id: SessionId, tool_name: &str) {
        self.fanout(
            id,
            SsePayload::ToolCallFinished {
                tool_name: tool_name.into(),
            },
        );
    }

    fn assistant_token_delta(&self, id: SessionId, text: &str) {
        self.fanout(id, SsePayload::AssistantTokenDelta { text: text.into() });
    }

    fn state_advanced(&self, id: SessionId, new_state: &ecaa_workflow_conversation::SessionState) {
        self.fanout(
            id,
            SsePayload::StateAdvanced {
                new_state: new_state.clone(),
            },
        );
    }

    fn turn_appended(&self, id: SessionId, turn: &ecaa_workflow_conversation::Turn) {
        // Fan the new turn out to SSE subscribers so the UI can append
        // it locally. The 60s transcript poll is still the
        // reconciliation fallback.
        self.fanout(id, SsePayload::TurnAppended { turn: turn.clone() });
    }

    fn task_reset(&self, _id: SessionId, task_id: &str) {
        // `rerun_task` just reset a completed task; drop any cached
        // artifact listings for this task across all sessions so the
        // next get_task_result scans fresh files.
        let Some(app) = self.app.get().cloned() else {
            return;
        };
        let task_id = task_id.to_string();
        tokio::spawn(async move {
            app.invalidate_artifact_cache_for_task(&task_id).await;
        });
    }

    fn proposal_received(
        &self,
        id: SessionId,
        proposal_id: &ecaa_workflow_core::hypothesized_proposal::ProposalId,
        node_id: &str,
    ) {
        self.fanout(
            id,
            SsePayload::ProposalReceived {
                proposal_id: proposal_id.clone(),
                node_id: node_id.to_string(),
            },
        );
    }

    fn proposal_gate_advanced(
        &self,
        id: SessionId,
        proposal_id: &ecaa_workflow_core::hypothesized_proposal::ProposalId,
        gate: ecaa_workflow_core::hypothesized_proposal::GateName,
        passed: bool,
    ) {
        self.fanout(
            id,
            SsePayload::ProposalGateAdvanced {
                proposal_id: proposal_id.clone(),
                gate,
                passed,
            },
        );
    }

    /// Chat-driven execution start. The StartExecution tool fires this
    /// from the service's dispatch loop. We spawn the harness in a
    /// detached tokio task so dispatch isn't blocked.
    fn execution_requested(
        &self,
        session_id: SessionId,
        agent_path: Option<String>,
        max_iterations: Option<u32>,
    ) {
        let Some(app) = self.app.get().cloned() else {
            // Demoted from
            // `eprintln!`. Even though this line doesn't print a
            // session id, it sits on the execution-spawn hot path so
            // keeping it on the tracing pipeline lets operators
            // correlate it with the surrounding span fields.
            tracing::warn!(
                ?session_id,
                "execution_requested: sink not bound to app state"
            );
            return;
        };
        tokio::spawn(async move {
            // Demoted from
            // `eprintln!` so session-id correlation lives in the
            // structured tracing pipeline rather than the raw stderr
            // stream. The level reflects severity — success is debug,
            // already-running / already-starting are info (operationally
            // expected), spawn errors are warn.
            // `start_execution` is a fresh first run — honour the configured
            // method-source authority (no rerun/amend freeze; plan Task 5.2).
            match execution::spawn_harness_for_session(
                &app,
                session_id,
                agent_path,
                max_iterations,
                false,
            )
            .await
            {
                Ok(handle) => {
                    tracing::debug!(
                        ?session_id,
                        pid = handle.pid,
                        "execution_requested: spawned harness"
                    );
                }
                Err(execution::SpawnHarnessError::AlreadyRunning { pid }) => {
                    tracing::info!(?session_id, pid, "execution_requested: already running");
                }
                Err(execution::SpawnHarnessError::AlreadyStarting) => {
                    tracing::info!(?session_id, "execution_requested: already starting");
                }
                Err(e) => {
                    let reason = match e {
                        execution::SpawnHarnessError::SessionNotFound => {
                            "session not found".to_string()
                        }
                        execution::SpawnHarnessError::NotEmitted => "not emitted".to_string(),
                        execution::SpawnHarnessError::SpawnFailed(err) => err.to_string(),
                        execution::SpawnHarnessError::SentinelCleanup(err) => {
                            format!("sentinel cleanup failed: {err}")
                        }
                        execution::SpawnHarnessError::NoPid => "no pid".to_string(),
                        execution::SpawnHarnessError::AlreadyRunning { .. } => unreachable!(),
                        execution::SpawnHarnessError::AlreadyStarting => unreachable!(),
                    };
                    tracing::warn!(
                        ?session_id,
                        reason = %reason,
                        "execution_requested: spawn failed"
                    );
                }
            }
        });
    }
}
