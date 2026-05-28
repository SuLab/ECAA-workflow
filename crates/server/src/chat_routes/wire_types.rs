//! JSON wire types for the chat API. Lives outside
//! `chat_routes/mod.rs` so the top-level module stays merge-only.
//! These types are the public contract between the chat server and
//! the UI / harness clients; changes here require coordination with
//! TypeScript (`ts-rs::TS` re-exports drive the browser-side type
//! definitions).

use ecaa_workflow_conversation::{SessionId, Turn};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One file under a completed task's `runtime/<task_id>/` artifact
/// directory. Returned from the per-task result endpoint and the
/// `task_completed_reviewable` SSE event so the UI can render
/// thumbnails / download links without another round-trip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Filename within the task artifact directory.
    pub name: String,
    /// Path relative to the session package root.
    pub relative_path: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// MIME type inferred from the file extension.
    pub mime_type: String,
}

/// Request body for `POST /api/chat/session` — create a new chat session.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateSessionRequest {
    /// When true, escalates the LLM model to Opus for higher-confidence intake.
    #[serde(default)]
    pub careful_mode: bool,
}

/// Response from `POST /api/chat/session`.
#[derive(Debug, Clone, Serialize)]
pub struct CreateSessionResponse {
    /// Newly minted session identifier.
    pub session_id: SessionId,
    /// Greeting turn produced by the conversation service on session start.
    pub greeting: Turn,
}

/// Request body for `POST /api/chat/session/from-intent` — create a session
/// pre-seeded with structured SME intent fields.
#[derive(Debug, Clone, Deserialize)]
pub struct StartSessionFromIntentRequest {
    /// Free-text research goal provided by the SME.
    pub goal: String,
    /// Modality identifier (e.g. `scrna`, `bulk_rnaseq`).
    pub modality: String,
    /// Organism name or taxonomy ID; empty string when not specified.
    #[serde(default)]
    pub organism: String,
    /// Desired output types or deliverables described by the SME.
    #[serde(default)]
    pub desired_outputs: String,
    /// Open questions or uncertainties the SME wants addressed.
    #[serde(default)]
    pub uncertainties: String,
    /// When true, escalates to Opus for higher-confidence intake.
    #[serde(default)]
    pub careful_mode: bool,
}

/// Request body for `POST /api/chat/session/:id/turn`.
#[derive(Debug, Clone, Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct SendTurnRequest {
    /// SME message text to append as a user turn.
    pub message: String,
    /// Optional client-generated user turn id. When present, the
    /// server uses this id for the persisted user Turn instead of
    /// minting its own. Lets the optimistic UI append + 60s
    /// reconciliation poll dedupe via turn_id (closes the
    /// dup-user-turn drift after long-lived sessions).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub user_turn_id: Option<String>,
}

/// Snapshot of a session's state machine position and progress counters.
/// Returned by `GET /api/chat/session/:id/state`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SessionStateSnapshot {
    /// Session identifier.
    #[ts(type = "string")]
    pub session_id: SessionId,
    /// Current state-machine state.
    pub state: ecaa_workflow_conversation::SessionState,
    /// Whether the SME has clicked the Confirm button.
    pub user_confirmed: bool,
    /// Timestamp of the most recent activity in this session.
    #[ts(type = "string")]
    pub last_activity: chrono::DateTime<chrono::Utc>,
    /// Total number of tasks in the emitted DAG; zero before emission.
    #[ts(type = "number")]
    pub task_count: usize,
    /// Aggregated task-state counts.
    pub progress: ProgressSummary,
    /// Absolute path to the emitted package, once `emit_package` has run.
    /// None while the session is still in Intake / ReadyToEmit. Exposed so
    /// test harnesses (and, eventually, UI components that link to the
    /// package) can read the server-assigned path rather than trying to
    /// route an `output_dir` through the LLM's prose — the `emit_package`
    /// tool's schema intentionally hides `output_dir`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(type = "string | null", optional)]
    pub emitted_package_path: Option<std::path::PathBuf>,
    /// operator-visible short name, populated by
    /// `POST /api/chat/session/:id/auto-title`. Absent until the
    /// Haiku-powered side call has fired for this session, or
    /// persistently absent when the feature flag is off. The UI title
    /// bar renders this when present and falls back to the short
    /// session id otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// parent session linkage when this session was branched
    /// from another via `branch_session`. Carries the parent's session
    /// id so the Plan tab header can render a "Branch of …" chip +
    /// Switch-to-parent link. Null on root sessions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    /// task ids currently in `blocked` state. Drives the
    /// "Needs your input" header chip + deep-link to the first blocked
    /// task in the Plan tab. Empty when nothing is blocked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_tasks: Vec<String>,
    /// Filesystem paths the path-hint extractor pulled out of SME
    /// intake prose. Drives the UI's "Detected inputs — register?"
    /// affordance and is mirrored to the LLM via the
    /// `get_session_state` tool. Empty when no validated hints exist
    /// (and elided from the JSON when empty so callers that don't
    /// know about this field stay happy).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_input_hints: Vec<ecaa_workflow_conversation::intake_path_hints::InputPathHint>,
}

/// Aggregated task-state counts for the `SessionStateSnapshot.progress` field.
#[derive(Debug, Clone, Serialize, Default, ts_rs::TS)]
#[ts(export)]
pub struct ProgressSummary {
    /// Number of tasks in `Completed` state.
    #[ts(type = "number")]
    pub completed: usize,
    /// Number of tasks in `Ready` state (prereqs satisfied, not yet dispatched).
    #[ts(type = "number")]
    pub ready: usize,
    /// Number of tasks in `Blocked` state awaiting SME input.
    #[ts(type = "number")]
    pub blocked: usize,
    /// Number of tasks not yet ready (awaiting upstream completion).
    #[ts(type = "number")]
    pub pending: usize,
}

/// Wrapper around `SsePayload` carrying a monotonic
/// per-session sequence number. Every broadcast goes through this
/// envelope so subscribers can drop out-of-order events. The wire
/// Shape is `{ "seq": N, "type": "...",... }` via `#[serde(flatten)]`.
///
/// The seq is minted at the broadcaster layer (see
/// `ChatAppState::next_sse_seq` + `BroadcastEventSink::fanout`) so
/// every subscriber sees the same seq for the same logical event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvelopedEvent {
    /// Monotonic per-session sequence number; starts at 1.
    pub seq: u64,
    /// Event payload flattened into the wire object.
    #[serde(flatten)]
    pub payload: SsePayload,
}

/// Server-sent event payload. Tagged by `"type"` field on the wire
/// (`#[serde(tag = "type", rename_all = "snake_case")]`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SsePayload {
    /// A tool call started in the current assistant turn.
    ToolCallStarted {
        /// Tool name as it appears in the closed vocabulary.
        tool_name: String,
        /// Human-readable status line for the UI status pill.
        status_line: String,
    },
    /// A tool call completed (succeeded or failed).
    ToolCallFinished {
        /// Name of the tool that finished.
        tool_name: String,
    },
    /// A streaming token chunk from the assistant's response.
    AssistantTokenDelta {
        /// Incremental text fragment.
        text: String,
    },
    /// The session state machine advanced to a new state.
    StateAdvanced {
        /// The state the session transitioned into.
        new_state: ecaa_workflow_conversation::SessionState,
    },
    /// A harness lifecycle event (task started/completed/failed/blocked/stalled).
    HarnessProgress {
        /// Event kind string (e.g. `"task_started"`, `"task_completed"`).
        kind: String,
        /// Task identifier this event pertains to.
        task_id: String,
        /// Human-readable status.
        status: String,
        /// Extended detail or progress line.
        detail: String,
        /// Remote executor metadata; present only for remote executor kinds.
        #[serde(skip_serializing_if = "Option::is_none")]
        remote: Option<RemoteExecutionInfoWire>,
    },
    /// An infrastructure-level error the server surfaced to the conversation.
    InfraError {
        /// Machine-readable reason code.
        reason: String,
        /// Human-readable copy the UI renders in the chat pane.
        user_copy: String,
    },
    /// Fires when an amendment package has just been re-emitted.
    PackageAmended {
        /// Session whose package was amended.
        #[serde(with = "uuid_as_string")]
        session_id: Uuid,
        /// Stage whose method was amended.
        amended_stage: String,
        /// Task IDs that were invalidated by the amendment.
        invalidated_tasks: Vec<String>,
        /// Absolute path to the newly-emitted package.
        package_path: String,
    },
    /// Fires alongside a `task_completed` HarnessProgress when the
    /// harness has reported artifacts for the completed task.
    TaskCompletedReviewable {
        /// Task that produced the artifacts.
        task_id: String,
        /// List of artifact files available for download.
        artifacts: Vec<ArtifactRef>,
    },
    /// Sizing-pilot started for the current session.
    HarnessSizingPilotStarted,
    /// Sizing-pilot completed; report contains per-atom timing and sizing data.
    HarnessSizingPilotComplete {
        /// Pilot report as a free-form JSON object.
        report: serde_json::Value,
    },
    /// Sizing-pilot was skipped.
    HarnessSizingPilotSkipped {
        /// Human-readable reason (e.g. `"ECAA_PILOT_ENABLED unset"`).
        reason: String,
    },
    /// Stall signal observed by the harness.
    HarnessStallDetected {
        /// Task that stalled.
        task_id: String,
        /// Stall signal payload from the stall monitor.
        signal: ecaa_workflow_core::blocker::StallSignalWire,
        /// Recommended operator action.
        suggested_action: ecaa_workflow_core::blocker::StallAction,
    },
    /// Optional follow-up when the stall monitor's projection suggests
    /// a resize would resolve the stall.
    HarnessResizeRecommended {
        /// Task triggering the resize recommendation.
        task_id: String,
        /// Current instance type.
        from_instance_type: String,
        /// Recommended instance type.
        to_instance_type: String,
    },
    /// Fires on emit completion when a cross-version diff was produced.
    HarnessVersionDiff {
        /// Cross-version diff report as a free-form JSON object.
        report: serde_json::Value,
    },
    /// Emitted when a subscriber falls behind the broadcast channel's
    /// ring buffer and the server drops events on its behalf.
    ResyncRequired {
        /// Number of events dropped from the ring buffer.
        dropped: u64,
    },
    /// Fires when the auto-fired dashboard summary side-call (Haiku) errors out.
    DashboardSummaryFailed {
        /// Task whose dashboard summary failed.
        task_id: String,
        /// Error reason.
        reason: String,
    },
    /// A new assistant or user turn was just committed.
    TurnAppended {
        /// The turn that was committed.
        turn: Turn,
    },
    /// Harness startup diagnostic reporting the selected executor.
    HarnessExecutorSelected {
        /// Executor name (e.g. `"local"`, `"aws"`, `"slurm"`).
        name: String,
        /// CPU core budget available to the executor.
        cpu_budget: u64,
        /// GPU unit budget available to the executor.
        gpu_budget: u64,
        /// Remote instance type, if applicable.
        instance_type: Option<String>,
        /// Harness binary version string.
        harness_version: String,
        /// Environment mode (e.g. `"local"`, `"remote"`).
        env_mode: String,
    },
    /// Final POST-health counters from the harness on session exit.
    HarnessProgressHealth {
        /// Total progress POST attempts.
        total_posts: u64,
        /// Number of failed POST attempts.
        failed_posts: u64,
        /// Total HTTP attempts including retries.
        total_attempts: u64,
        /// Last error message, empty when all posts succeeded.
        last_error: String,
        /// RFC 3339 timestamp of the last successful POST.
        last_success_at: String,
    },
    /// Verified orphan-reap sweep outcome.
    HarnessOrphansReaped {
        /// Number of candidate instances examined.
        candidate_count: u64,
        /// Number of instances confirmed orphaned and reaped.
        verified_count: u64,
        /// Instance IDs that could not be verified; left running.
        unverified_ids: Vec<String>,
        /// Reap policy applied (e.g. `"terminate"`).
        policy: String,
    },
    /// Per-task heartbeat stall detected.
    HarnessHeartbeatStalled {
        /// Task whose heartbeat has stalled.
        task_id: String,
        /// Seconds since the last heartbeat was received.
        age_secs: u64,
    },
    /// A new hypothesized-node proposal was accepted into the
    /// session-scoped registry. Fires once per `propose_hypothesized_node`
    /// tool acceptance.
    ProposalReceived {
        /// Identifier of the accepted proposal.
        proposal_id: ecaa_workflow_core::hypothesized_proposal::ProposalId,
        /// DAG node identifier assigned to the proposal.
        node_id: String,
    },
    /// A promotion gate (validator / sandbox) just produced
    /// an outcome on a proposal. `passed=false` means the proposal
    /// transitioned to `Blocked`.
    ProposalGateAdvanced {
        /// Proposal that advanced.
        proposal_id: ecaa_workflow_core::hypothesized_proposal::ProposalId,
        /// Which gate produced the outcome.
        gate: ecaa_workflow_core::hypothesized_proposal::GateName,
        /// True when the gate passed; false when it blocked.
        passed: bool,
    },
    /// SME promotion-authority signoff materialized the
    /// proposal into the executable DAG.
    ProposalPromoted {
        /// Proposal that was promoted.
        proposal_id: ecaa_workflow_core::hypothesized_proposal::ProposalId,
        /// DAG task node id created by promotion.
        task_node_id: String,
    },
    /// SME rejected the proposal. Terminal.
    ProposalRejected {
        /// Proposal that was rejected.
        proposal_id: ecaa_workflow_core::hypothesized_proposal::ProposalId,
        /// Optional SME-supplied reason for the rejection.
        rationale: Option<String>,
    },
    /// Stall signal received via the direct relay thread, bypassing the
    /// main harness loop. Fired when the harness is blocked inside an
    /// SSM call and can no longer drain the normal stall-monitor channel.
    StallSignalDirect {
        /// Task that triggered the stall signal.
        task_id: String,
        /// "cpu_starvation" | "memory_pressure" | "gpu_idle_during_training"
        /// | "runtime_over_expected"
        kind: String,
        /// Raw stall measurements from the stall monitor.
        measurements: serde_json::Value,
        /// "retry" | "resize" | "abort"
        suggested_action: String,
    },
    /// A git-provenance commit hook was dropped by the `GitHookPool`
    /// because the pool was saturated (all concurrent slots occupied)
    /// or the hook exceeded its per-hook wall-clock timeout. The
    /// recovery-point timeline has a gap. `trigger` is the operation
    /// name ("emit", "amend", "branch", …); `reason` is either
    /// `"pool_saturated"` or `"timeout_secs=<n>"`. Operators should
    /// run the git commit manually against the package directory to
    /// close the gap.
    ProvenanceCommitDropped {
        /// Operation that triggered the dropped hook (e.g. `"emit"`, `"amend"`).
        trigger: String,
        /// Why the hook was dropped (`"pool_saturated"` or `"timeout_secs=N"`).
        reason: String,
    },
}

mod uuid_as_string {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use uuid::Uuid;

    pub(super) fn serialize<S: Serializer>(id: &Uuid, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&id.to_string())
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Uuid, D::Error> {
        let s = String::deserialize(d)?;
        Uuid::parse_str(&s).map_err(serde::de::Error::custom)
    }
}

/// Harness-to-server progress POST body. Parsed from `POST /api/chat/session/:id/progress`.
#[derive(Debug, Clone, Deserialize)]
pub struct HarnessProgressEvent {
    /// Event kind string (e.g. `"task_started"`, `"task_completed"`, `"task_blocked"`).
    pub kind: String,
    /// Task identifier this event pertains to; empty for session-level events.
    pub task_id: String,
    /// Human-readable status.
    pub status: String,
    /// Extended detail or progress line.
    pub detail: String,
    /// Remote-backend executor metadata for `kind == "task_*"`.
    #[serde(default)]
    pub remote: Option<RemoteExecutionInfoWire>,
    /// Artifact list for `kind == "task_completed"`.
    #[serde(default)]
    pub artifacts: Option<Vec<ArtifactRef>>,
    /// Stall signal for `kind == "task_stalled"`.
    #[serde(default)]
    pub stall_signal: Option<ecaa_workflow_core::blocker::StallSignalWire>,
    /// Suggested recovery action for `kind == "task_stalled"`.
    #[serde(default)]
    pub suggested_action: Option<ecaa_workflow_core::blocker::StallAction>,
    /// Pilot-lifecycle payload for `sizing_pilot_*` kinds.
    #[serde(default)]
    pub pilot_report: Option<serde_json::Value>,
    /// Cross-version-diff report for `kind == "cross_version_diff"`.
    #[serde(default)]
    pub cross_version_report: Option<serde_json::Value>,
    /// Resize recommendation for `kind == "resize_recommended"`.
    #[serde(default)]
    pub from_instance_type: Option<String>,
    /// Recommended target instance type for `kind == "resize_recommended"`.
    #[serde(default)]
    pub to_instance_type: Option<String>,
    /// Per-task agent token usage for `kind == "task_completed"`.
    #[serde(default)]
    pub agent_usage: Option<AgentUsageWire>,
    /// Executor info for `kind == "executor_selected"`.
    #[serde(default)]
    pub executor_info: Option<ExecutorInfoWire>,
    /// Health counters for `kind == "progress_client_health"`.
    #[serde(default)]
    pub client_health: Option<ProgressClientHealthWire>,
    /// Reap summary for `kind == "orphan_instances_reaped"`.
    #[serde(default)]
    pub orphan_reap: Option<OrphanReapWire>,
    /// Heartbeat-age for `kind == "heartbeat_stalled"`.
    #[serde(default)]
    pub heartbeat_age_secs: Option<u64>,
    /// RFC 3339 harness clock timestamp. Present only on the first POST
    /// per session (harness stamps it once after the AtomicBool handshake).
    /// When present the server echoes `X-Server-Now` in the response so
    /// the harness can measure host-vs-server clock skew (§9.1).
    #[serde(default)]
    pub client_now: Option<String>,
}

/// Mirror of the harness-side `ExecutorInfoWire`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ExecutorInfoWire {
    /// Executor name (e.g. `"local"`, `"aws"`, `"slurm"`).
    pub name: String,
    /// CPU core budget available to the executor.
    pub cpu_budget: u64,
    /// GPU unit budget available to the executor.
    pub gpu_budget: u64,
    /// Remote instance type, if applicable.
    #[serde(default)]
    pub instance_type: Option<String>,
    /// Harness binary version string.
    pub harness_version: String,
    /// Environment mode string.
    pub env_mode: String,
}

/// Mirror of the harness-side `ProgressClientHealthWire`.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ProgressClientHealthWire {
    /// Total progress POST attempts (successful + failed).
    pub total_posts: u64,
    /// Number of POSTs that received a non-2xx response.
    pub failed_posts: u64,
    /// Total HTTP attempts including per-post retries.
    pub total_attempts: u64,
    /// Last error message; empty string when all posts succeeded.
    #[serde(default)]
    pub last_error: String,
    /// RFC 3339 timestamp of the last successful POST.
    #[serde(default)]
    pub last_success_at: String,
}

/// Mirror of the harness-side `OrphanReapWire`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OrphanReapWire {
    /// Number of candidate orphan instances examined.
    pub candidate_count: u64,
    /// Number of instances confirmed orphaned and terminated.
    pub verified_count: u64,
    /// Instance IDs that could not be verified; left running.
    #[serde(default)]
    pub unverified_ids: Vec<String>,
    /// Reap policy that was applied (e.g. `"terminate"`).
    pub policy: String,
}

/// Wire shape for the `agent_usage` field on a HarnessProgressEvent.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentUsageWire {
    /// Model ID used by the agent (e.g. `"claude-sonnet-4-6"`).
    pub model: String,
    /// Input token count billed for this task.
    pub input_tokens: u64,
    /// Output token count billed for this task.
    pub output_tokens: u64,
    /// Tokens read from the prompt cache.
    #[serde(default)]
    pub cache_read_tokens: u64,
    /// Tokens written into the prompt cache.
    #[serde(default)]
    pub cache_creation_tokens: u64,
}

/// Wire shape for the `remote` field on a HarnessProgressEvent.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemoteExecutionInfoWire {
    /// Backend name (e.g. `"aws"`, `"slurm"`).
    pub backend: String,
    /// Cloud/cluster instance identifier.
    pub instance_id: String,
    /// Instance type string (e.g. `"r6i.4xlarge"`).
    pub instance_type: String,
}

/// Optional body for the SME-driven checkpoint endpoints (`/confirm`,
/// `/reject`, `/unblock`, `/branch`). When the client supplies a
/// `rationale`, the string is attached to the corresponding
/// `DecisionRecord`. Absent body is treated as no rationale.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CheckpointDecisionRequest {
    /// SME rationale text attached to the resulting `DecisionRecord`.
    #[serde(default)]
    pub rationale: Option<String>,
    /// Blocker resolution value (for `/unblock` with a resolution field).
    #[serde(default)]
    pub resolution: Option<String>,
    /// Per-stage SME review gate.
    #[serde(default)]
    pub stage: Option<String>,
    /// Optional `SessionMode` from the Confirmation card.
    #[serde(default)]
    pub mode: Option<ecaa_workflow_core::session_mode::SessionMode>,
    /// Optional `CheckpointMode` from the Confirmation card.
    #[serde(default)]
    pub checkpoint_mode: Option<ecaa_workflow_core::checkpoint_mode::CheckpointMode>,
    /// Task boundary for task-scoped branch (M1.3). When set, the child
    /// DAG is snapshotted with this task reset to Ready and its transitive
    /// successors reset to Pending. Only consumed by the `/branch` endpoint.
    #[serde(default)]
    pub task_id: Option<String>,
}

/// Request body for `POST /api/chat/session/:id/start-execution`.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct StartExecutionRequest {
    /// Path to the agent script the harness should invoke per task.
    #[serde(default)]
    pub agent_path: Option<String>,
    /// Maximum harness iterations before the harness self-terminates.
    #[serde(default)]
    pub max_iterations: Option<u32>,
}

/// Response from `GET /api/chat/session/:id/execution`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub struct ExecutionStatusResponse {
    /// OS process ID of the harness subprocess.
    pub pid: u32,
    /// Timestamp when the harness was launched.
    #[ts(type = "string")]
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Absolute path to the package directory the harness is executing against.
    #[ts(type = "string")]
    pub package_dir: std::path::PathBuf,
    /// Full agent command string passed to the harness.
    pub agent_command: String,
    /// One of `running` | `pausing` | `paused` | `stopping` | `exited`.
    pub status: String,
    /// Exit code when `status == "exited"`; absent while the process is live.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub exit_code: Option<i32>,
    /// Timestamp when the harness was last paused; absent when not paused.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub paused_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Timestamp when a stop was requested; absent when not stopping.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub stop_requested_at: Option<chrono::DateTime<chrono::Utc>>,
    /// POSIX process group ID used by `/execution/kill` to terminate the harness tree.
    pub pgid: u32,
}

/// Map a `SessionState` variant to its canonical string discriminant.
/// Used by callers that need a stable string key without depending on
/// serde's tag representation.
pub(crate) fn session_state_kind(
    state: &ecaa_workflow_conversation::SessionState,
) -> &'static str {
    use ecaa_workflow_conversation::SessionState as S;
    match state {
        S::Greeting => "greeting",
        S::Intake => "intake",
        S::IntakeFollowup => "intake_followup",
        S::PendingConfirmation { .. } => "pending_confirmation",
        S::ReadyToEmit => "ready_to_emit",
        S::Emitting => "emitting",
        S::Emitted => "emitted",
        S::Amending { .. } => "amending",
        S::Blocked { .. } => "blocked",
        _ => "unknown",
    }
}
