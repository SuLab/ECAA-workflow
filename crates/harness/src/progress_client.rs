//! Sync HTTP client that posts harness execution events back to the
//! conversation server so they can be turned into assistant turns.
//!
//! Per `memory:feedback_simplicity` the harness stays sync — we use `ureq`
//! (blocking) instead of pulling tokio in. Posts are best-effort: a failure
//! does not stall execution. The harness path used by `make ivd-execute`
//! never sets `--session-id` so this code is a no-op for the deterministic
//! CI path.

#![allow(unreachable_pub)]
use ecaa_workflow_core::blocker::{BlockerKind, StallAction, StallSignalWire};
use ecaa_workflow_core::clock::{Clock, WallClock};
use ecaa_workflow_core::dag::TaskState;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Current on-disk schema version for [`HarnessProgressEvent`].
///
/// Stays `pub` (not `pub(crate)`) so integration tests under
/// `crates/harness/tests/` — which compile as separate crates and see
/// only the public lib surface — can pin the wire-format version they
/// produce in their event fixtures.
pub fn harness_progress_event_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_harness_progress_event_version()
}

fn default_harness_progress_event_schema_version() -> semver::Version {
    harness_progress_event_schema_version()
}

/// Current on-disk schema version for [`OrphanReapWire`].
pub(crate) fn orphan_reap_wire_schema_version() -> semver::Version {
    ecaa_workflow_core::migration::current_orphan_reap_wire_version()
}

fn default_orphan_reap_wire_schema_version() -> semver::Version {
    orphan_reap_wire_schema_version()
}

/// Agent-side LLM usage captured during task execution, mirrored to the
/// server so the session's `/api/chat/session/:id/metrics` endpoint can
/// surface `agent_cost_usd` alongside the chat-side `total_cost_usd`.
/// Populated by the harness executor after the agent process exits —
/// typically by reading a JSON file the agent script writes to
/// `runtime/outputs/<task_id>/agent-usage.json`.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct AgentUsageWire {
    /// `ModelId::api_id()` string (e.g. "claude-sonnet-4-6"). The server
    /// translates this into a `ModelId` for pricing lookup.
    pub model: String,
    /// Prompt tokens charged by the Anthropic API for this task invocation.
    pub input_tokens: u64,
    /// Completion tokens charged by the Anthropic API for this task invocation.
    pub output_tokens: u64,
    #[serde(default)]
    /// Tokens served from prompt cache, reducing effective input cost.
    pub cache_read_tokens: u64,
    #[serde(default)]
    /// Tokens written into prompt cache during this invocation.
    pub cache_creation_tokens: u64,
}

/// Snapshot of the executor selected at harness startup. Posted on the
/// `executor_selected` progress event so the UI Progress tab can render
/// the backend header immediately instead of inferring it from the
/// first `task_started` event.
/// §1.5.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct ExecutorInfoWire {
    /// `Executor::name()` — "local", "aws", "slurm", "mock".
    pub name: String,
    /// Declared concurrent CPU-task budget.
    pub cpu_budget: u64,
    /// Declared concurrent GPU-task budget.
    pub gpu_budget: u64,
    /// Backend-native instance shape (e.g. "m5.4xlarge") when the
    /// executor knows it up-front (AWS with a fixed pre-provisioned
    /// instance). Absent for local / mock / sized-at-pilot runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_type: Option<String>,
    /// Harness crate version (`CARGO_PKG_VERSION`). Lets the Progress
    /// tab surface harness-upgrade mismatches between UI and harness.
    pub harness_version: String,
    /// Value of `SWFC_EXECUTOR_MODE` at harness startup ("local" if
    /// unset). Matches `name` in happy paths; differs only if the
    /// factory silently substitutes (it doesn't today, but cheap to
    /// record).
    pub env_mode: String,
}

/// ProgressClient rolling health counters. Exposed so the UI Performance
/// tab can surface "Progress events lost" without scanning the health
/// sidecar.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct ProgressClientHealthWire {
    /// Total semantic events submitted (not attempts — one per `post()`
    /// call regardless of retries).
    pub total_posts: u64,
    /// Events that failed every retry and were dropped.
    pub failed_posts: u64,
    /// Total HTTP attempts across all events (counts retries).
    pub total_attempts: u64,
    /// Last error string when non-empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_error: String,
    /// ISO 8601 timestamp of last successful POST, or empty when none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_success_at: String,
}

/// Mirror of the server-side `HarnessProgressEvent` shape. Optional
/// fields carry structured payloads for pilot and stall events; they
/// serialize with `skip_serializing_if = Option::is_none` so the wire
/// shape stays backward-compatible with older servers.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct HarnessProgressEvent {
    /// On-disk/wire schema version. `#[serde(default)]` lets older
    /// servers that never wrote this field deserialize with `0.1.0`.
    /// The `schema_version_serde` adapter accepts both legacy `u64`
    /// values and canonical SemVer strings; always writes canonical SemVer.
    #[serde(
        default = "default_harness_progress_event_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    pub schema_version: semver::Version,
    /// Event category string (e.g. "task_started", "task_completed", "heartbeat_stalled").
    pub kind: String,
    /// Task identifier this event pertains to.
    pub task_id: String,
    /// Short machine-readable status (e.g. "running", "completed", "failed").
    pub status: String,
    /// Human-readable detail string for the UI Progress tab.
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Structured stall-signal payload; present only on `kind == "stall_detected"`.
    pub stall_signal: Option<StallSignalWire>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Suggested remediation action accompanying a stall signal.
    pub suggested_action: Option<StallAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Pilot sizing report; present only on `kind == "pilot_completed"`.
    pub pilot_report: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Cross-version concordance report; present only on `kind == "cross_version_diff"`.
    pub cross_version_report: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Original instance type before a resize; present only on resize events.
    pub from_instance_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    /// Target instance type after a resize; present only on resize events.
    pub to_instance_type: Option<String>,
    /// Agent-side LLM usage. Populated on `task_completed` when the
    /// agent wrote `runtime/outputs/<task_id>/agent-usage.json`; absent
    /// when agent instrumentation is unavailable. Older servers that
    /// don't know the field ignore it (serde flattens on the wire).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_usage: Option<AgentUsageWire>,
    /// Populated on `kind == "executor_selected"` with the backend
    /// snapshot. Older servers ignore it silently (serde flattens on
    /// the wire). See §1.5 of
    ///
    #[serde(skip_serializing_if = "Option::is_none")]
    pub executor_info: Option<ExecutorInfoWire>,
    /// Populated on `kind == "progress_client_health"` with rolling
    /// POST health counters.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_health: Option<ProgressClientHealthWire>,
    /// Populated on `kind == "orphan_instances_reaped"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orphan_reap: Option<OrphanReapWire>,
    /// Populated on `kind == "heartbeat_stalled"` with the age of the
    /// stalest task's heartbeat file, in seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_age_secs: Option<u64>,
    /// Populated on `kind == "cost_guard_passed"` with per-provision and
    /// cumulative spend figures. Lets the UI render a budget bar without
    /// parsing the human-readable `detail` string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_guard: Option<CostGuardSnapshot>,
    /// RFC 3339 harness clock timestamp. Sent only on the first POST per
    /// session so the server can echo `X-Server-Now` and the harness can
    /// detect host-vs-server clock skew. Absent on subsequent POSTs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_now: Option<String>,
}

/// Typed payload for a `cost_guard_passed` progress event. Carries
/// enough detail for the UI to render a budget bar and for operators
/// to understand both the per-provision spend and the session-level
/// cumulative burn rate without parsing the `detail` string.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct CostGuardSnapshot {
    /// Estimated spend in USD for the current provision (per-provision ceiling check).
    pub estimated_usd: f64,
    /// Per-provision ceiling value in USD (`SWFC_AWS_COST_CEILING_USD`).
    pub ceiling_usd: f64,
    /// Cumulative spend in USD across all provisions this run so far
    /// (read from the persisted sidecar after `record_provision`).
    pub cumulative_usd: f64,
    /// Run-total ceiling in USD (`SWFC_AWS_RUN_TOTAL_CEILING_USD`,
    /// default $100). The UI can render `cumulative_usd / total_ceiling_usd`
    /// as a budget bar.
    pub total_ceiling_usd: f64,
}

/// Verification outcome for an AWS orphan reap sweep.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq)]
pub struct OrphanReapWire {
    /// On-disk/wire schema version. `#[serde(default)]` keeps older
    /// payloads that predate this field deserializing as `0.1.0`.
    #[serde(
        default = "default_orphan_reap_wire_schema_version",
        with = "ecaa_workflow_core::migration::schema_version_serde"
    )]
    pub schema_version: semver::Version,
    /// Total candidates the reaper tried to terminate.
    pub candidate_count: u64,
    /// Candidates whose subsequent `describe-instances` reported
    /// `terminated` or `shutting-down` within the verification window.
    pub verified_count: u64,
    /// Candidate instance ids that did not converge to terminated
    /// within the verification window.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverified_ids: Vec<String>,
    /// Policy at reap time ("warn" | "reap" | "dry-run" | "none").
    pub policy: String,
    /// P1-158 — per-id failures returned by the AWS API itself when
    /// the batched `terminate-instances` call reported
    /// `UnsuccessfulItems`. Each pair is `(instance_id, reason)`.
    /// Distinct from `unverified_ids`: a failure here means AWS
    /// refused the request (e.g. instance no longer exists); an
    /// unverified id means AWS accepted the request but the
    /// instance hasn't converged to `terminated` yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminate_failures: Vec<(String, String)>,
    /// P1-226 — ids confirmed terminated by the convergence poll.
    /// The harness startup forwards these as the WAL recovery's
    /// `instance_denylist`. Surfaced on the wire too so an operator
    /// log scrape can audit the kill list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified_ids: Vec<String>,
}

impl HarnessProgressEvent {
    /// Build a plain kind/task_id/status/detail event with no
    /// pilot/stall payloads attached. Matches the legacy shape.
    fn bare(kind: &str, task_id: &str, status: &str, detail: &str) -> Self {
        Self {
            schema_version: harness_progress_event_schema_version(),
            kind: kind.into(),
            task_id: task_id.into(),
            status: status.into(),
            detail: detail.into(),
            stall_signal: None,
            suggested_action: None,
            pilot_report: None,
            cross_version_report: None,
            from_instance_type: None,
            to_instance_type: None,
            agent_usage: None,
            executor_info: None,
            client_health: None,
            orphan_reap: None,
            heartbeat_age_secs: None,
            cost_guard: None,
            client_now: None,
        }
    }
}

/// Security remediation recover
/// from a poisoned mutex by returning the inner guard regardless of
/// whether a previous panic poisoned the lock. The sender thread's
/// health-counter writes must never abort just because some earlier
/// panic-then-catch left the mutex in the poisoned state; the
/// counters themselves are CRDT-ish (monotonic + last-write-wins)
/// so a stale observation is preferable to a process abort.
fn lock_or_recover<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Rolling POST health counters. Wrapped in an `Arc<Mutex<..>>` on the
/// client so the dedicated sender thread can update from each drained
/// job while the harness main thread reads via `health_snapshot()` /
/// `health_loss_ratio()` without serializing against in-flight network
/// IO.
#[derive(Debug, Default, Clone)]
struct HealthCounters {
    total_posts: u64,
    failed_posts: u64,
    total_attempts: u64,
    last_error: String,
    last_success_at: String,
}

/// One unit of work for the sender thread. the
/// harness main thread enqueues these into a bounded mpsc and returns
/// immediately; the sender thread drains and does the retried HTTP
/// POST without blocking the main loop.
///
/// `HarnessProgressEvent` is ~560 bytes due to its many optional
/// payload fields (`pilot_report`, `cross_version_report`, etc.);
/// we box it to keep the enum compact (so the mpsc buffer stays
/// roughly `capacity * sizeof(SenderJob)` ≈ 256 * 32 bytes instead
/// of 256 * 560 bytes).
#[allow(dead_code)] // SetTaskState variant constructed conditionally; warn on private re-scope.
enum SenderJob {
    /// `POST /api/chat/session/:id/progress` carrying a fully-built
    /// `HarnessProgressEvent`. Covers `task_started`, `task_completed`,
    /// `task_blocked`, `task_failed`, `task_stalled`, sizing-pilot
    /// events, executor-selected, heartbeat-stalled, etc.
    Progress(Box<HarnessProgressEvent>),
    /// `POST /api/chat/session/:id/task/:task_id/state` for the
    /// 17.2.3 task-state mirror endpoint.
    SetTaskState {
        task_id: String,
        state: ecaa_workflow_core::dag::TaskState,
    },
}

/// Sends structured harness progress events to the chat server.
///
/// Internally uses a bounded `mpsc::sync_channel` to a dedicated sender thread
/// so `post()` never blocks the harness main loop. On queue overflow events are
/// dropped and counted in `events_dropped`.
#[allow(dead_code)] // Some fields used only by conditional code paths; pub(crate) scope exposed them as unused.
pub struct ProgressClient {
    /// Bounded mpsc to the sender thread. Capacity 256 — `try_send`
    /// drops on overflow rather than blocking the harness main thread.
    /// We'd rather lose a `task_completed` event than stall the loop.
    /// Wrapped in `Option` so `Drop` can take ownership and explicitly
    /// drop the sender, which closes the channel and lets the sender
    /// thread exit cleanly before we join it.
    tx: Option<std::sync::mpsc::SyncSender<SenderJob>>,
    /// Sender-thread join handle, kept inside an `Option` so `Drop`
    /// can take ownership for the bounded join on shutdown.
    join_handle: Option<std::thread::JoinHandle<()>>,
    /// Shared health counters — read by the main thread (snapshot /
    /// loss ratio / sidecar flush), written by the sender thread on
    /// each drained job. Shared via `Arc<Mutex<..>>` so both threads
    /// see a consistent view.
    health: std::sync::Arc<std::sync::Mutex<HealthCounters>>,
    /// Package directory (set by `with_package_dir`) — the sender
    /// thread reads this when deciding whether to write the rolling
    /// `runtime/harness-health.json` sidecar. `Arc<Mutex<..>>` so
    /// `with_package_dir` after `new()` propagates without restarting
    /// the sender thread.
    package_dir: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    /// Dedicated sync `ureq::Agent` for the main-thread-only
    /// `is_session_pausing_dispatch` GET. We keep it separate from
    /// the sender thread's agent so a bounded-channel deadlock can't
    /// occur (the dispatch-gate call would otherwise have to wait
    /// for sender-thread capacity behind every queued POST). The
    /// dispatch-gate path uses sync ureq on a separate path from the
    /// bounded sender to keep liveness independent of queue depth.
    dispatch_gate_agent: ureq::Agent,
    /// Base URL kept here for the main-thread-only
    /// `is_session_pausing_dispatch` call (the sender thread has its
    /// own owned copy).
    base_url: String,
    /// Session id kept for the same reason as `base_url`.
    session_id: String,
    /// Optional bearer token attached to every outbound HTTP call.
    /// Captured once at construction from `SWFC_SERVER_AUTH_TOKEN`.
    /// The sender thread
    /// receives its own owned copy at spawn time so the field here
    /// is just a record for `is_session_pausing_dispatch` and the
    /// `auth_token()` accessor used by integration tests.
    auth_token: Option<String>,
    /// Injected clock for `last_success_at` health stamps. The sender
    /// thread receives a shared Arc clone so both the main thread and the
    /// sender use the same clock source. `WallClock` at production; tests
    /// substitute a `FrozenClock` to assert deterministic timestamps.
    /// Stored on the struct so future main-thread paths can re-clone it
    /// without re-plumbing the constructor; current consumers are only
    /// the sender thread (cloned in `new`).
    #[allow(dead_code)]
    clock: Arc<dyn Clock + Send + Sync>,
    /// Cumulative count of progress events dropped by `try_send`
    /// overflow (queue saturated) or sender-thread disconnection.
    /// Bumped from both `post()` and
    /// `set_task_state()`. The very first transition from 0→1 emits a
    /// `tracing::warn!` so operators see the saturation signal in
    /// structured log output without spamming the warn channel for
    /// every subsequent drop. Exposed via `events_dropped()` so
    /// integration tests + the Performance tab can observe the
    /// counter without scraping eprintln output.
    ///
    /// `Arc<AtomicU64>` (not just `AtomicU64`) so a future sender
    /// thread could share the counter without restructuring; kept
    /// behind `Arc` today for symmetry with `health` even though only
    /// the main thread mutates it.
    events_dropped: std::sync::Arc<AtomicU64>,
    /// Set once the first POST per session has been sent. The sender
    /// thread stamps `client_now` on the first event so the server can
    /// echo `X-Server-Now` and this client can measure host-vs-server
    /// clock skew (§9.1). Subsequent POSTs omit the field. Held here so
    /// the sender thread (cloned in `new`) and the main thread share one
    /// atomic; the field is intentionally main-thread-read-rarely today.
    #[allow(dead_code)]
    first_post_sent: std::sync::Arc<AtomicBool>,
    /// Detected clock skew from the first-POST handshake. Set by the
    /// sender thread after reading `X-Server-Now` on the first
    /// successful response. The harness main loop reads this via
    /// `clock_skew_blocker()` before each dispatch iteration and
    /// refuses to start if the skew exceeds the threshold.
    #[allow(dead_code)]
    clock_skew_blocker: std::sync::Arc<std::sync::Mutex<Option<BlockerKind>>>,
}

#[allow(dead_code)] // Some methods only invoked by conditional codepaths; pub(crate) scope exposed them as unused.
impl ProgressClient {
    /// Constructs a `ProgressClient` backed by the wall clock.
    pub fn new(session_id: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_clock(session_id, base_url, Arc::new(WallClock))
    }

    /// Constructor that accepts an injected clock. Used in tests to supply
    /// a `FrozenClock` so `last_success_at` health stamps are deterministic.
    /// Production code calls `new()` which supplies a `WallClock`.
    pub fn with_clock(
        session_id: impl Into<String>,
        base_url: impl Into<String>,
        clock: Arc<dyn Clock + Send + Sync>,
    ) -> Self {
        let session_id = session_id.into();
        let base_url = base_url.into();
        let health = std::sync::Arc::new(std::sync::Mutex::new(HealthCounters::default()));
        let package_dir = std::sync::Arc::new(std::sync::Mutex::new(None::<std::path::PathBuf>));

        // Security remediation capture the
        // bearer token once at startup so every outbound HTTP call
        // (sender thread POSTs + the main-thread dispatch-gate GET)
        // can attach the `Authorization: Bearer …` header that the
        // server's `auth_middleware` requires when bound non-loopback
        // or when the token is explicitly set.
        let auth_token = std::env::var("SWFC_SERVER_AUTH_TOKEN")
            .ok()
            .filter(|t| !t.is_empty());

        // Capacity 256 chosen so a WAL recovery
        // emitting up to ~256 `task_blocked` events back-to-back
        // queues without dropping; harness loops with thousands of
        // tasks would see drops which we log via `eprintln!`.
        let (tx, rx) = std::sync::mpsc::sync_channel::<SenderJob>(256);

        let first_post_sent = std::sync::Arc::new(AtomicBool::new(false));
        let clock_skew_blocker = std::sync::Arc::new(std::sync::Mutex::new(None::<BlockerKind>));

        let sender_session = session_id.clone();
        let sender_base = base_url.clone();
        let sender_health = health.clone();
        let sender_pkg = package_dir.clone();
        let sender_token = auth_token.clone();
        let sender_clock = clock.clone();
        let sender_first_post = first_post_sent.clone();
        let sender_skew = clock_skew_blocker.clone();
        let join_handle = std::thread::Builder::new()
            .name("progress-client-sender".into())
            .spawn(move || {
                sender_loop(
                    sender_session,
                    sender_base,
                    sender_token,
                    rx,
                    sender_health,
                    sender_pkg,
                    sender_clock,
                    sender_first_post,
                    sender_skew,
                );
            })
            .expect("spawn progress-client-sender");

        let dispatch_gate_agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build();

        Self {
            tx: Some(tx),
            join_handle: Some(join_handle),
            health,
            package_dir,
            dispatch_gate_agent,
            base_url,
            session_id,
            auth_token,
            clock,
            events_dropped: std::sync::Arc::new(AtomicU64::new(0)),
            first_post_sent,
            clock_skew_blocker,
        }
    }

    /// Total progress events dropped by `try_send` overflow (queue
    /// saturated) or sender-thread disconnection over
    /// the life of this `ProgressClient`. Exposed so integration tests
    /// can assert saturation behaviour without scraping log output, and
    /// so the UI Performance tab (future hookup) can surface a drop
    /// counter alongside the failed-POST counter without scanning the
    /// eprintln stream.
    ///
    /// Increments cover both `post()` and `set_task_state()` — anywhere
    /// the bounded mpsc rejects a job. Read-only accessor; the counter
    /// reset semantics match the rest of the harness lifecycle (per-
    /// process, cleared on harness restart).
    ///
    /// `#[allow(dead_code)]` — the harness `main.rs` does not call this
    /// yet (the existing `health_loss_ratio()` is the path used to gate
    /// exit code); the accessor is exposed for the saturation test and
    /// for a follow-up wiring into the harness-health sidecar.
    #[allow(dead_code)]
    pub fn events_dropped(&self) -> u64 {
        self.events_dropped.load(Ordering::Relaxed)
    }

    /// Read-only accessor for the captured auth token. Exposed for
    /// regression tests in `tests/progress_auth.rs`. Returns `None`
    /// when `SWFC_SERVER_AUTH_TOKEN` was unset or empty at construction
    /// time. The bin does not call this accessor; it's a test-only
    /// hook so the `dead_code` allow is intentional.
    #[allow(dead_code)]
    pub fn auth_token(&self) -> Option<&str> {
        self.auth_token.as_deref()
    }

    /// Returns a `BlockerKind::ClockSkew` when the sender thread detected
    /// that the host clock differs from the server clock by more than
    /// `SWFC_CLOCK_SKEW_THRESHOLD_SECS`. Returns `None` when the first
    /// POST hasn't completed yet or when the skew is within the threshold.
    /// Called by the harness main loop before each dispatch iteration.
    #[allow(dead_code)]
    pub(crate) fn clock_skew_blocker(&self) -> Option<BlockerKind> {
        lock_or_recover(&self.clock_skew_blocker).clone()
    }

    /// One-shot probe: GET `<base_url>/api/health` (or fall back to the
    /// session-state endpoint when health isn't available). Returns
    /// `true` when the server replied 401 — meaning auth is enforced
    /// and the harness must be started with `SWFC_SERVER_AUTH_TOKEN`
    /// in its env. Returns `false` on any other status (200, 404, …)
    /// AND on network errors so an unreachable server doesn't refuse
    /// to start the harness (the server might still be coming up).
    pub(crate) fn probe_auth_required(base_url: &str) -> bool {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(2))
            .timeout(Duration::from_secs(5))
            .build();
        // `/api/health` is the lightest endpoint we expect to exist;
        // historically the chat server does not serve one, so we
        // probe the chat router at a stable path and treat anything
        // other than 401 as "auth not enforced". `into_string` is
        // discarded; we care about the status code only.
        let url = format!("{}/api/health", base_url.trim_end_matches('/'));
        match agent.get(&url).call() {
            Ok(_) => false,
            Err(ureq::Error::Status(401, _)) => true,
            Err(_) => false,
        }
    }

    /// Attach a package directory so the sender thread can write a
    /// rolling `runtime/harness-health.json` sidecar on every 10th
    /// POST. Absent by default — the harness main sets it once it
    /// knows the package path. No-op when unset.
    pub(crate) fn with_package_dir(self, package_dir: impl Into<std::path::PathBuf>) -> Self {
        if let Ok(mut g) = self.package_dir.lock() {
            *g = Some(package_dir.into());
        }
        self
    }

    /// Read-only snapshot of current health counters for callers that
    /// want to embed them in an event or decide the harness exit code.
    pub(crate) fn health_snapshot(&self) -> ProgressClientHealthWire {
        let g = lock_or_recover(&self.health);
        ProgressClientHealthWire {
            total_posts: g.total_posts,
            failed_posts: g.failed_posts,
            total_attempts: g.total_attempts,
            last_error: g.last_error.clone(),
            last_success_at: g.last_success_at.clone(),
        }
    }

    /// Loss ratio (failed / total), 0.0 when no posts. Used by
    /// `harness::main` to decide the process exit code.
    pub(crate) fn health_loss_ratio(&self) -> f32 {
        let g = lock_or_recover(&self.health);
        if g.total_posts == 0 {
            0.0
        } else {
            g.failed_posts as f32 / g.total_posts as f32
        }
    }

    /// Enqueue a progress event for the sender thread to POST. Returns
    /// immediately — the public method preserves its `()` fire-and-forget
    /// contract from before §19.3. Overflow drops are logged but do not
    /// propagate to callers; we'd rather drop a `task_completed` event
    /// than block the harness main thread on a backed-up queue.
    pub fn post(&self, event: &HarnessProgressEvent) {
        let Some(tx) = self.tx.as_ref() else {
            // Drop in progress — channel taken. Should be rare; we
            // still bump the dropped counter for telemetry.
            self.note_dropped_send("ProgressClient dropping");
            return;
        };
        let job = SenderJob::Progress(Box::new(event.clone()));
        if let Err(e) = tx.try_send(job) {
            // Sender disconnected (thread panicked) or queue full.
            // Either way, count the drop and move on — the main thread
            // never stalls on the network.
            self.note_dropped_send(&format!("{}", e));
            eprintln!(
                "[progress] queue full or sender dead — dropping {} event for task {}: {}",
                event.kind, event.task_id, e
            );
        }
    }

    /// Increment the dropped-event counters under the bounded-queue
    /// fail-fast contract. Kept inline so the post/set_task_state hot
    /// paths share the same telemetry shape as the post-attempt
    /// failures recorded by the sender thread.
    ///
    /// Bumps the cumulative `events_dropped` atomic and emits a
    /// one-shot structured `tracing::warn!` on the
    /// 0→1 transition. We deliberately do NOT warn on every drop —
    /// saturation tends to manifest as bursts of hundreds of drops in
    /// a row, and warning per-event would drown the log. Subsequent
    /// drops still bump the counter and the existing `eprintln!` /
    /// health-counter paths, so a downstream consumer can still
    /// observe the rate.
    fn note_dropped_send(&self, err: &str) {
        let mut g = lock_or_recover(&self.health);
        g.total_posts += 1;
        g.failed_posts += 1;
        g.last_error = format!("queue-drop: {}", err);
        drop(g); // release the lock before the tracing call

        let prev = self.events_dropped.fetch_add(1, Ordering::Relaxed);
        if prev == 0 {
            tracing::warn!(
                session_id = %self.session_id,
                error = %err,
                "progress channel saturated; events dropping"
            );
        }
    }

    /// Write a one-shot snapshot of health counters into
    /// `<package>/runtime/harness-health.json`. Atomic via `.tmp` rename.
    /// Reads the current `package_dir` through the shared `Arc<Mutex<..>>`
    /// so both the main-thread `flush_health_sidecar` call and the
    /// sender-thread's per-10-post rolling write resolve the same dir.
    fn write_health_sidecar(
        package_dir: &std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
        snap: &ProgressClientHealthWire,
    ) -> std::io::Result<()> {
        let dir = {
            let g = package_dir
                .lock()
                .map_err(|e| std::io::Error::other(format!("package_dir mutex poisoned: {e}")))?;
            match g.as_ref() {
                Some(d) => d.clone(),
                None => return Ok(()),
            }
        };
        let runtime_dir = dir.join("runtime");
        let target = runtime_dir.join("harness-health.json");
        let body = serde_json::to_vec_pretty(snap).map_err(std::io::Error::other)?;
        ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&target, &body)?;
        Ok(())
    }

    /// Flush the health sidecar unconditionally. Called by the harness
    /// at loop exit so the final counters land on disk regardless of
    /// where the total_posts counter happened to stop.
    pub(crate) fn flush_health_sidecar(&self) {
        let snap = self.health_snapshot();
        let _ = Self::write_health_sidecar(&self.package_dir, &snap);
    }

    /// Consult the session-state snapshot so the parallel scheduler
    /// can gate new dispatches when the SME is mid-amend or the
    /// session is Blocked / PendingConfirmation. Returns:
    /// * `Ok(true)` — scheduler should pause new dispatches.
    /// * `Ok(false)` — scheduler should proceed.
    /// * `Err(_)` — network / parse failure. The caller MUST treat
    ///   this as "unknown — wait", NOT "proceed". Returning false on
    ///   any error would let the harness launch agents while the SME
    ///   was mid-amend with a server briefly unreachable; the caller
    ///   instead sleeps `SWFC_HARNESS_SETTLE_SECS` and retries on the
    ///   next iteration. This trades a bounded stall window for not
    ///   racing against a paused session.
    ///
    /// **Sync call deliberately.** This stays on `dispatch_gate_agent`
    /// (a separate `ureq::Agent` from the sender thread) so a backed-up
    /// progress queue can't deadlock the dispatch gate. Sync ureq runs
    /// on a path independent of the bounded sender.
    pub fn is_session_pausing_dispatch(&self) -> anyhow::Result<bool> {
        let url = format!(
            "{}/api/chat/session/{}/state",
            self.base_url.trim_end_matches('/'),
            self.session_id
        );
        let mut req = self.dispatch_gate_agent.get(&url);
        if let Some(tok) = self.auth_token.as_deref() {
            req = req.set("Authorization", &format!("Bearer {tok}"));
        }
        let response = req.call().map_err(|e| anyhow::anyhow!("GET {url}: {e}"))?;
        let text = response
            .into_string()
            .map_err(|e| anyhow::anyhow!("read body for {url}: {e}"))?;
        let v: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("parse JSON from {url}: {e}"))?;
        let kind = v
            .get("state")
            .and_then(|s| s.get("kind"))
            .and_then(|k| k.as_str())
            .unwrap_or("");
        Ok(matches!(
            kind,
            "blocked" | "amending" | "pending_confirmation"
        ))
    }

    /// When the session is in `Amending` state, return the
    /// `(target_stage, invalidated_tasks)` pair so the harness can
    /// soft-cancel any Running tasks whose ids appear in the list.
    /// Returns `None` when the session is not Amending, when
    /// `--session-id` is unset (no `ProgressClient`), or on any
    /// network / parse error. Errors are swallowed and treated as
    /// "not currently amending" — the conservative path is to leave
    /// running tasks alone rather than stall execution.
    ///
    /// Uses the same `dispatch_gate_agent` path as
    /// `is_session_pausing_dispatch` so the bounded sender queue is
    /// never implicated.
    pub(crate) fn get_amending_invalidated_tasks(&self) -> Option<(String, Vec<String>)> {
        let url = format!(
            "{}/api/chat/session/{}/state",
            self.base_url.trim_end_matches('/'),
            self.session_id
        );
        let mut req = self.dispatch_gate_agent.get(&url);
        if let Some(tok) = self.auth_token.as_deref() {
            req = req.set("Authorization", &format!("Bearer {tok}"));
        }
        let text = req.call().ok()?.into_string().ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let state = v.get("state")?;
        if state.get("kind").and_then(|k| k.as_str()) != Some("amending") {
            return None;
        }
        let target_stage = state
            .get("target_stage")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string())?;
        let invalidated_tasks: Vec<String> = state
            .get("invalidated_tasks")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        Some((target_stage, invalidated_tasks))
    }

    /// Fired once at harness startup. Carries the backend name +
    /// budgets + version so the Progress tab can render a header row
    /// from t=0 instead of waiting for a `task_started` event. See §1.5
    /// of
    pub(crate) fn executor_selected(&self, info: ExecutorInfoWire) {
        let mut event = HarnessProgressEvent::bare(
            "executor_selected",
            "",
            "ready",
            &format!(
                "{} backend ready (cpu_budget={}, gpu_budget={})",
                info.name, info.cpu_budget, info.gpu_budget
            ),
        );
        event.executor_info = Some(info);
        self.post(&event);
    }

    /// Fired on harness exit (and periodically if loss ratio crosses a
    /// threshold). Carries the final health snapshot so the Performance
    /// tab can warn on degraded runs.
    pub(crate) fn progress_client_health(&self) {
        let snap = self.health_snapshot();
        let detail = format!(
            "total={} failed={} attempts={}",
            snap.total_posts, snap.failed_posts, snap.total_attempts
        );
        let mut event = HarnessProgressEvent::bare("progress_client_health", "", "report", &detail);
        event.client_health = Some(snap);
        self.post(&event);
    }

    /// Fired after a verified AWS orphan-reap sweep.
    pub(crate) fn orphan_instances_reaped(&self, reap: OrphanReapWire) {
        let detail = format!(
            "reaped {}/{} (policy={})",
            reap.verified_count, reap.candidate_count, reap.policy
        );
        let mut event =
            HarnessProgressEvent::bare("orphan_instances_reaped", "", "report", &detail);
        event.orphan_reap = Some(reap);
        self.post(&event);
    }

    /// Fired when one or more Running tasks' heartbeat files are older
    /// than `SWFC_TASK_HEARTBEAT_STALL_SECS`. Per-task emission — the
    /// task_id identifies the stalled task; also carries the
    /// `age_secs` so the Progress tab can render "15m no heartbeat".
    pub(crate) fn heartbeat_stalled(&self, task_id: &str, age_secs: u64) {
        let mut event = HarnessProgressEvent::bare(
            "heartbeat_stalled",
            task_id,
            "blocked",
            &format!("Heartbeat age {}s exceeds threshold", age_secs),
        );
        event.heartbeat_age_secs = Some(age_secs);
        self.post(&event);
    }

    /// Fired after every successful provision-cost check so operators
    /// can see cumulative-spend status via SSE, not only the abort-on-
    /// overage case. Carries a `CostGuardSnapshot` for the UI budget
    /// bar (`cumulative_usd / total_ceiling_usd`). `task_id` is the
    /// task being provisioned for; empty string when the check fires at
    /// the pre-loop provisioning step and no single task is in scope yet.
    #[allow(dead_code)]
    pub(crate) fn cost_guard_passed(&self, task_id: &str, snapshot: CostGuardSnapshot) {
        let detail = format!(
            "per-provision: ${:.2}; cumulative: ${:.2}/${:.2}",
            snapshot.estimated_usd, snapshot.cumulative_usd, snapshot.total_ceiling_usd,
        );
        let mut event = HarnessProgressEvent::bare("cost_guard_passed", task_id, "ok", &detail);
        event.cost_guard = Some(snapshot);
        self.post(&event);
    }

    /// Fired by the wall-clock watchdog when a Running task has exceeded its
    /// budget (`expected_wall_seconds × SWFC_WATCHDOG_MULTIPLIER` or
    /// `timeout_at` from the dispatch WAL). Posts a
    /// `task_wall_clock_exceeded` event so the server can transition the
    /// task to `Blocked { WallClockExceeded }`.
    pub(crate) fn wall_clock_exceeded(
        &self,
        task_id: &str,
        observed_secs: u64,
        threshold_secs: u64,
    ) {
        let detail = format!(
            "Wall-clock budget exceeded: {}s observed, {}s threshold",
            observed_secs, threshold_secs
        );
        self.post(&HarnessProgressEvent::bare(
            "task_wall_clock_exceeded",
            task_id,
            "blocked",
            &detail,
        ));
    }

    /// Fired by the wall-clock watchdog for every Running task on each poll.
    /// Carries `heartbeat_age_secs` so the UI Progress tab can render live
    /// staleness for every Running task, including CPU-bound loops that keep
    /// the heartbeat fresh.
    pub(crate) fn heartbeat_age_update(&self, task_id: &str, age_secs: u64) {
        let mut event = HarnessProgressEvent::bare(
            "heartbeat_age",
            task_id,
            "running",
            &format!("heartbeat_age_secs={}", age_secs),
        );
        event.heartbeat_age_secs = Some(age_secs);
        self.post(&event);
    }

    /// Write a task-state transition through the authoritative
    /// `POST /api/chat/session/:id/task/:task_id/state` endpoint.
    /// The harness mirrors every local
    /// `TaskState` mutation to the conversation server so that the
    /// server-side `Session::task_states` map (the post-Phase-D source
    /// of truth) does not get clobbered by the tool-loop merge.
    ///
    /// Enqueues into the bounded sender channel
    /// and returns immediately. The sender thread does the retry
    /// dance. The public method retains its `()` fire-and-forget
    /// contract; callers in `main.rs` that already do not propagate
    /// errors continue to work unchanged.
    pub fn set_task_state(&self, task_id: &str, state: &TaskState) {
        let Some(tx) = self.tx.as_ref() else {
            self.note_dropped_send("ProgressClient dropping");
            return;
        };
        let job = SenderJob::SetTaskState {
            task_id: task_id.to_string(),
            state: state.clone(),
        };
        if let Err(e) = tx.try_send(job) {
            self.note_dropped_send(&format!("{}", e));
            eprintln!(
                "[progress] queue full or sender dead — dropping set_task_state for {}: {}",
                task_id, e
            );
        }
    }

    /// Posts a `task_started` progress event for `task_id`.
    pub fn task_started(&self, task_id: &str, detail: &str) {
        self.post(&HarnessProgressEvent::bare(
            "task_started",
            task_id,
            "running",
            detail,
        ));
    }

    pub(crate) fn task_completed(&self, task_id: &str, detail: &str) {
        self.post(&HarnessProgressEvent::bare(
            "task_completed",
            task_id,
            "completed",
            detail,
        ));
    }

    /// Variant of `task_completed` that also carries the agent's LLM
    /// usage for the task. Used by executors after they've parsed
    /// `runtime/outputs/<task_id>/agent-usage.json`. The server
    /// forwards usage to `MetricsStore::record_agent_usage`.
    pub(crate) fn task_completed_with_usage(
        &self,
        task_id: &str,
        detail: &str,
        usage: AgentUsageWire,
    ) {
        let mut ev = HarnessProgressEvent::bare("task_completed", task_id, "completed", detail);
        ev.agent_usage = Some(usage);
        self.post(&ev);
    }

    /// Parse an agent-usage sidecar at a well-known path inside the
    /// package. Returns None when the file is missing or unparseable —
    /// missing instrumentation is not a failure, just an absence of
    /// cost data.
    pub(crate) fn read_agent_usage(
        package_dir: &std::path::Path,
        task_id: &str,
    ) -> Option<AgentUsageWire> {
        let path = package_dir
            .join("runtime")
            .join("outputs")
            .join(task_id)
            .join("agent-usage.json");
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice::<AgentUsageWire>(&bytes).ok()
    }

    pub(crate) fn task_failed(&self, task_id: &str, detail: &str) {
        self.post(&HarnessProgressEvent::bare(
            "task_failed",
            task_id,
            "failed",
            detail,
        ));
    }

    /// Posts a `task_blocked` progress event for `task_id`.
    pub fn task_blocked(&self, task_id: &str, detail: &str) {
        self.post(&HarnessProgressEvent::bare(
            "task_blocked",
            task_id,
            "blocked",
            detail,
        ));
    }

    pub(crate) fn execution_finished(&self) {
        self.post(&HarnessProgressEvent::bare(
            "execution_finished",
            "",
            "complete",
            "All tasks complete.",
        ));
    }

    /// Pilot started — carries the selected task ids so the server can
    /// render "running pilot on [a, b, c]" in the assistant turn.
    pub(crate) fn sizing_pilot_started(&self, task_ids: &[String]) {
        let mut event = HarnessProgressEvent::bare(
            "sizing_pilot_started",
            "",
            "running",
            &format!("pilot on {} tasks", task_ids.len()),
        );
        event.pilot_report = Some(serde_json::json!({ "task_ids": task_ids }));
        self.post(&event);
    }

    /// Pilot complete — carries the full `PilotReport` JSON so the UI
    /// Metrics tab can render it without a round-trip.
    pub(crate) fn sizing_pilot_complete<R: Serialize>(&self, report: &R) {
        let mut event =
            HarnessProgressEvent::bare("sizing_pilot_complete", "", "complete", "pilot complete");
        event.pilot_report = serde_json::to_value(report).ok();
        self.post(&event);
    }

    /// Pilot skipped — carries a plain-language reason.
    pub(crate) fn sizing_pilot_skipped(&self, reason: &str) {
        self.post(&HarnessProgressEvent::bare(
            "sizing_pilot_skipped",
            "",
            "skipped",
            reason,
        ));
    }

    /// Stall signal observed. The server transitions the session to
    /// `Blocked { Stalled }` on receipt (see
    /// `chat_routes::post_progress` task_stalled handler).
    pub(crate) fn task_stalled(
        &self,
        task_id: &str,
        signal: &StallSignalWire,
        suggested_action: StallAction,
    ) {
        let mut event =
            HarnessProgressEvent::bare("task_stalled", task_id, "stalled", "stall signal observed");
        event.stall_signal = Some(signal.clone());
        event.suggested_action = Some(suggested_action);
        self.post(&event);
    }

    /// Surfaced when the stall monitor projects that a resize would
    /// resolve the stall. Wired into the main stall-drain loop in
    /// `main.rs`, which pairs a CpuStarvation / MemoryPressure signal
    /// with a backend-reported current instance type and a
    /// `suggest_resize` bump.
    pub(crate) fn resize_recommended(
        &self,
        task_id: &str,
        from_instance_type: &str,
        to_instance_type: &str,
    ) {
        let mut event = HarnessProgressEvent::bare(
            "resize_recommended",
            task_id,
            "advisory",
            "resize recommended",
        );
        event.from_instance_type = Some(from_instance_type.into());
        event.to_instance_type = Some(to_instance_type.into());
        self.post(&event);
    }
}

impl Drop for ProgressClient {
    /// On drop:
    /// 1. Take `tx` out of the `Option` and let it drop, which
    ///    closes the channel. The sender thread's `rx.recv()` then
    ///    returns `Err(Disconnected)` once it finishes its current
    ///    iteration and the loop exits cleanly.
    /// 2. Join the sender thread with a bounded timeout so the
    ///    harness never hangs on shutdown — at most ~2.6s of
    ///    in-flight retries plus a tiny buffer. We use a watchdog
    ///    thread to ensure the join never blocks longer than the
    ///    timeout regardless of how the sender thread behaves.
    ///
    /// Clean shutdown lets queued events
    /// drain before the harness exits; the bounded wait keeps the
    /// harness's overall shutdown time predictable.
    fn drop(&mut self) {
        // Step 1 — close the channel.
        drop(self.tx.take());

        // Step 2 — join with a bounded timeout. The sender's worst
        // case for one in-flight job is ~2.6s of cumulative retry
        // sleeps, so a 3s ceiling is just past that — enough to let
        // the sender finish its current job in the happy case
        // without pinning harness shutdown when the server is
        // unreachable and dozens of queued jobs would otherwise
        // multiply the retry cost. On timeout we leak the watchdog
        // thread (it will reap at process exit) — the sender
        // thread itself dies with the process.
        if let Some(handle) = self.join_handle.take() {
            let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
            std::thread::Builder::new()
                .name("progress-client-drain".into())
                .spawn(move || {
                    let _ = handle.join();
                    let _ = done_tx.send(());
                })
                .ok();
            let _ = done_rx.recv_timeout(Duration::from_secs(3));
        }
    }
}

/// Parse the `SWFC_CLOCK_SKEW_THRESHOLD_SECS` environment variable with
/// clamping to `[10, 3600]`. Returns the default of 60 when the variable
/// is absent or unparseable.
fn clock_skew_threshold_secs() -> u64 {
    let raw = std::env::var("SWFC_CLOCK_SKEW_THRESHOLD_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    raw.clamp(10, 3600)
}

/// Sender-thread main loop. Owns its `ureq::Agent` so the agent's
/// connection pool stays single-threaded and we don't share a pool
/// with `dispatch_gate_agent` on the main thread (separate paths,
/// see plan §19.3 caveat).
///
/// Loops until the channel is closed (last `ProgressClient` dropped).
/// Each drained job triggers the same 3-attempt retry dance the
/// pre-19.3 sync path used (100ms / 500ms / 2000ms backoffs +
/// timeout_connect=2s + timeout=5s per attempt) — the change is that
/// these timeouts now burn on a background thread instead of the
/// harness main loop.
#[allow(clippy::too_many_arguments)]
fn sender_loop(
    session_id: String,
    base_url: String,
    auth_token: Option<String>,
    rx: std::sync::mpsc::Receiver<SenderJob>,
    health: std::sync::Arc<std::sync::Mutex<HealthCounters>>,
    package_dir: std::sync::Arc<std::sync::Mutex<Option<std::path::PathBuf>>>,
    clock: Arc<dyn Clock + Send + Sync>,
    first_post_sent: std::sync::Arc<AtomicBool>,
    clock_skew_blocker: std::sync::Arc<std::sync::Mutex<Option<BlockerKind>>>,
) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(2))
        .timeout(Duration::from_secs(5))
        .build();
    let backoffs = [
        Duration::from_millis(100),
        Duration::from_millis(500),
        Duration::from_millis(2000),
    ];
    while let Ok(job) = rx.recv() {
        // On the first Progress POST per session, stamp `client_now`
        // with the harness clock (RFC 3339) so the server can echo
        // `X-Server-Now` in the response headers. We use a
        // compare-exchange so only the very first event carries the
        // field even if two events race into the queue simultaneously.
        let is_first_progress_post = matches!(&job, SenderJob::Progress(_))
            && first_post_sent
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok();

        let (url, mut body) = match &job {
            SenderJob::Progress(event) => {
                let u = format!(
                    "{}/api/chat/session/{}/progress",
                    base_url.trim_end_matches('/'),
                    session_id
                );
                let b = match serde_json::to_value(event.as_ref()) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("[progress] failed to serialize {} event: {}", event.kind, e);
                        continue;
                    }
                };
                (u, b)
            }
            SenderJob::SetTaskState { task_id, state } => {
                let u = format!(
                    "{}/api/chat/session/{}/task/{}/state",
                    base_url.trim_end_matches('/'),
                    session_id,
                    task_id,
                );
                let b = serde_json::json!({ "state": state });
                (u, b)
            }
        };

        // Inject `client_now` into the body on the first Progress POST.
        let client_now_str: Option<String> = if is_first_progress_post {
            let ts = chrono::Utc::now().to_rfc3339();
            if let Some(obj) = body.as_object_mut() {
                obj.insert(
                    "client_now".to_string(),
                    serde_json::Value::String(ts.clone()),
                );
            }
            Some(ts)
        } else {
            None
        };

        let mut last_err: Option<String> = None;
        let mut attempts: u64 = 0;
        let mut ok = false;
        let mut server_now_header: Option<String> = None;
        for attempt in 0..=backoffs.len() {
            attempts += 1;
            let mut req = agent.post(&url);
            if let Some(tok) = auth_token.as_deref() {
                req = req.set("Authorization", &format!("Bearer {tok}"));
            }
            match req.send_json(&body) {
                Ok(resp) => {
                    // Capture `X-Server-Now` header only on the first POST.
                    if client_now_str.is_some() {
                        server_now_header = resp.header("X-Server-Now").map(|s| s.to_string());
                    }
                    ok = true;
                    break;
                }
                Err(e) => {
                    last_err = Some(format!("{}", e));
                    if attempt < backoffs.len() {
                        std::thread::sleep(backoffs[attempt]);
                    }
                }
            }
        }

        // After the first successful POST, evaluate clock skew.
        if let (Some(client_ts), Some(server_ts)) =
            (client_now_str.as_deref(), server_now_header.as_deref())
        {
            let threshold = clock_skew_threshold_secs();
            let skew_opt = chrono::DateTime::parse_from_rfc3339(client_ts)
                .ok()
                .zip(chrono::DateTime::parse_from_rfc3339(server_ts).ok())
                .map(|(ct, st)| (st - ct).num_seconds().abs());
            if let Some(observed) = skew_opt {
                if observed > threshold as i64 {
                    tracing::warn!(
                        session_id = %session_id,
                        observed_secs = observed,
                        threshold_secs = threshold,
                        "clock skew exceeds threshold — dispatch will be refused"
                    );
                    let blocker = BlockerKind::ClockSkew {
                        observed_secs: observed,
                        threshold_secs: threshold,
                    };
                    if let Ok(mut g) = clock_skew_blocker.lock() {
                        *g = Some(blocker);
                    }
                }
            }
        }

        {
            let mut g = lock_or_recover(&health);
            g.total_posts += 1;
            g.total_attempts += attempts;
            if ok {
                g.last_success_at = clock.now_rfc3339();
            } else {
                g.failed_posts += 1;
                if let Some(e) = &last_err {
                    g.last_error = e.clone();
                }
            }
        }
        if !ok {
            if let Some(e) = last_err {
                eprintln!("[progress] POST {} failed after retries: {}", url, e);
            }
        }
        // Fire-and-forget rolling sidecar every 10 posts.
        let should_write = {
            let g = lock_or_recover(&health);
            g.total_posts.is_multiple_of(10)
        };
        if should_write {
            let snap = {
                let g = lock_or_recover(&health);
                ProgressClientHealthWire {
                    total_posts: g.total_posts,
                    failed_posts: g.failed_posts,
                    total_attempts: g.total_attempts,
                    last_error: g.last_error.clone(),
                    last_success_at: g.last_success_at.clone(),
                }
            };
            let _ = ProgressClient::write_health_sidecar(&package_dir, &snap);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Security remediation
    /// `lock_or_recover` must recover from a poisoned `std::sync::Mutex`
    /// instead of unwinding through the sender thread on the next call.
    /// We poison the mutex by panicking inside a `catch_unwind`, then
    /// observe that `lock_or_recover` still returns a usable guard.
    #[test]
    fn lock_or_recover_returns_inner_after_poison() {
        let m = std::sync::Arc::new(std::sync::Mutex::new(42u32));
        let m_clone = m.clone();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut g = m_clone.lock().unwrap();
            *g = 99;
            panic!("simulated panic while holding the mutex");
        }));
        // The mutex is now poisoned; a bare `m.lock()` would return Err.
        assert!(m.lock().is_err(), "mutex should be poisoned");
        // `lock_or_recover` recovers and returns the last-written value.
        let g = lock_or_recover(&m);
        assert_eq!(*g, 99);
    }

    #[test]
    fn harness_progress_event_round_trips_schema_version() {
        let ev = HarnessProgressEvent::bare("task_started", "t1", "running", "hello");
        let json = serde_json::to_string(&ev).unwrap();
        let back: HarnessProgressEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(ev.schema_version, back.schema_version);
    }

    #[test]
    fn legacy_harness_progress_event_without_schema_version_loads_with_default() {
        let legacy = r#"{"kind":"task_started","task_id":"t1","status":"running","detail":""}"#;
        let ev: HarnessProgressEvent =
            serde_json::from_str(legacy).expect("legacy HarnessProgressEvent parses");
        assert_eq!(
            ev.schema_version,
            semver::Version::new(0, 1, 0),
            "missing schema_version must default to 0.1.0"
        );
    }

    #[test]
    fn orphan_reap_wire_round_trips_schema_version() {
        let reap = OrphanReapWire {
            schema_version: orphan_reap_wire_schema_version(),
            candidate_count: 3,
            verified_count: 2,
            unverified_ids: vec!["i-abc".into()],
            policy: "reap".into(),
            terminate_failures: vec![],
            verified_ids: vec!["i-def".into(), "i-ghi".into()],
        };
        let json = serde_json::to_string(&reap).unwrap();
        let back: OrphanReapWire = serde_json::from_str(&json).unwrap();
        assert_eq!(reap.schema_version, back.schema_version);
    }

    #[test]
    fn legacy_orphan_reap_wire_without_schema_version_loads_with_default() {
        let legacy = r#"{
            "candidate_count": 1,
            "verified_count": 1,
            "policy": "reap"
        }"#;
        let reap: OrphanReapWire =
            serde_json::from_str(legacy).expect("legacy OrphanReapWire parses");
        assert_eq!(
            reap.schema_version,
            semver::Version::new(0, 1, 0),
            "missing schema_version must default to 0.1.0"
        );
    }

    #[test]
    fn bare_event_shape_excludes_optional_fields() {
        let ev = HarnessProgressEvent::bare("task_started", "t1", "running", "hello");
        let json = serde_json::to_string(&ev).unwrap();
        assert!(json.contains("\"kind\":\"task_started\""));
        assert!(!json.contains("stall_signal"));
        assert!(!json.contains("pilot_report"));
    }

    #[test]
    fn stall_event_carries_signal_and_action() {
        let mut ev = HarnessProgressEvent::bare("task_stalled", "t42", "stalled", "");
        ev.stall_signal = Some(StallSignalWire::MemoryPressure {
            pct: 93.0,
            window_mins: 5,
        });
        ev.suggested_action = Some(StallAction::Resize);
        let v: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["stall_signal"]["kind"], "memory_pressure");
        assert_eq!(v["suggested_action"], "resize");
    }

    #[test]
    fn pilot_event_embeds_report_json() {
        let mut ev = HarnessProgressEvent::bare("sizing_pilot_complete", "", "complete", "");
        ev.pilot_report = Some(serde_json::json!({ "confidence": 0.8 }));
        let v: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert!((v["pilot_report"]["confidence"].as_f64().unwrap() - 0.8).abs() < 1e-9);
    }

    /// `set_task_state` must POST to the new
    /// `/api/chat/session/:id/task/:task_id/state` endpoint with the
    /// expected `{"state": <TaskState>}` body shape that
    /// `SetTaskStateRequest` deserializes.
    #[test]
    fn set_task_state_posts_to_task_state_endpoint_with_expected_body() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        // One-shot mock HTTP server on a throwaway port. Captures the
        // POST path + body, replies 204. Mirrors the pattern from
        // `tests/harness_pilot_integration_test.rs::spawn_mock_server`.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set non-blocking");
        let captured = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
        let captured_clone = captured.clone();
        let shutdown = Arc::new(Mutex::new(false));
        let shutdown_clone = shutdown.clone();
        let handle = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if *shutdown_clone.lock().unwrap() {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut reader = BufReader::new(&mut stream);
                        let mut first_line = String::new();
                        let _ = reader.read_line(&mut first_line);
                        let parts: Vec<&str> = first_line.split_whitespace().collect();
                        let path = if parts.len() >= 2 {
                            parts[1].to_string()
                        } else {
                            String::new()
                        };
                        let mut content_length: usize = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() {
                                break;
                            }
                            let trimmed = line.trim_end_matches(['\r', '\n']);
                            if trimmed.is_empty() {
                                break;
                            }
                            if let Some(v) = trimmed
                                .strip_prefix("Content-Length:")
                                .or_else(|| trimmed.strip_prefix("content-length:"))
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; content_length];
                        if content_length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        captured_clone
                            .lock()
                            .unwrap()
                            .push((path, String::from_utf8_lossy(&body).to_string()));
                        let _ = stream.write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });

        let session_id = "abc-123";
        let base_url = format!("http://127.0.0.1:{}", port);
        let pc = ProgressClient::new(session_id, base_url);
        let state = TaskState::Running {
            started_at: "2026-05-14T00:00:00Z".into(),
            remote: None,
        };
        pc.set_task_state("task_42", &state);

        // `set_task_state` now enqueues into a bounded
        // mpsc; the actual POST is performed by the sender thread.
        // Wait for at least one captured request before signaling
        // shutdown, otherwise we'd race the sender thread and assert
        // before it has a chance to drain the queue.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if !captured.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        *shutdown.lock().unwrap() = true;
        let _ = handle.join();

        let log = captured.lock().unwrap();
        assert_eq!(log.len(), 1, "exactly one POST captured");
        let (path, body) = &log[0];
        assert_eq!(
            path,
            &format!("/api/chat/session/{}/task/task_42/state", session_id),
            "URL path must include session_id and task_id segments"
        );
        let parsed: serde_json::Value = serde_json::from_str(body).expect("body is valid JSON");
        assert_eq!(
            parsed["state"]["status"], "running",
            "body must wrap TaskState under `state` per SetTaskStateRequest"
        );
        assert_eq!(
            parsed["state"]["started_at"], "2026-05-14T00:00:00Z",
            "TaskState fields must serialize through with the default serde shape"
        );
    }

    /// When the sender thread is stuck on a retried HTTP POST and
    /// the bounded mpsc (capacity 256) fills,
    /// subsequent `post()` calls hit `try_send -> Err(Full)`. The
    /// `events_dropped()` counter must reflect every dropped event and
    /// the very first drop must emit a structured `tracing::warn!` so
    /// operators see the saturation signal once in their log stream.
    ///
    /// Mechanism: point the client at a TCP listener that accepts the
    /// connection but never writes a response, so the sender thread
    /// burns the 5s per-attempt timeout × 4 attempts on event #1
    /// (worst case ~20s blocked) while we hammer ~300 events into the
    /// queue. The 257th queued event onwards trips the saturation
    /// drop path.
    ///
    /// Wall-clock budget: ~200ms — we only need 1 cycle of try_send
    /// failures, not for the sender to actually retry-exhaust.
    #[test]
    fn saturated_queue_drops_events_and_warns_once() {
        use std::io::Read;
        use std::net::TcpListener;
        use std::sync::Arc;

        // Black-hole listener: accept the connection, hold it open, never write a response.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
        let port = listener.local_addr().unwrap().port();
        // Background thread that accepts and keeps connections open
        // indefinitely so the sender's POST hangs on read.
        let _accept_thread = std::thread::spawn(move || {
            let _open_streams: Arc<std::sync::Mutex<Vec<std::net::TcpStream>>> =
                Arc::new(std::sync::Mutex::new(Vec::new()));
            for stream in listener.incoming().flatten() {
                stream.set_read_timeout(Some(Duration::from_secs(60))).ok();
                let s_clone = match stream.try_clone() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                _open_streams.lock().unwrap().push(s_clone);
                // Drain bytes from the client so it doesn't get
                // SIGPIPE during our enqueue loop, but never write
                // back. The handler thread exits when the connection
                // closes.
                std::thread::spawn(move || {
                    let mut s = stream;
                    let mut buf = [0u8; 4096];
                    let _ = s.read(&mut buf);
                    std::thread::sleep(Duration::from_secs(60));
                });
            }
        });

        let pc = ProgressClient::new("session-saturation", format!("http://127.0.0.1:{}", port));
        // Pre-condition: nothing dropped at startup.
        assert_eq!(pc.events_dropped(), 0);

        // Fire ~500 events as fast as possible. The bounded mpsc has
        // capacity 256; once the sender thread is blocked on its
        // first HTTP attempt (waiting for our black-hole listener),
        // every subsequent `try_send` past capacity hits Err(Full).
        // We expect *at least one* drop. We don't expect a precise
        // number because the sender does occasionally drain one
        // before the 5s timeout fires.
        let total = 500u64;
        for i in 0..total {
            pc.post(&HarnessProgressEvent::bare(
                "task_started",
                &format!("t{}", i),
                "running",
                "saturating",
            ));
        }

        let dropped = pc.events_dropped();
        assert!(
            dropped > 0,
            "expected at least 1 drop under saturation; got {dropped}"
        );
        // Soft upper bound: we sent `total` events; we can't have
        // dropped more than `total - 1` (the sender thread always
        // gets at least the first one out of the queue).
        assert!(
            dropped < total,
            "drop count {dropped} should be strictly less than the {total} events sent"
        );

        // Drop the client; the Drop impl bounded-joins so this test
        // doesn't leak the sender thread.
        drop(pc);
    }

    /// §9.1 — when the mock server echoes an `X-Server-Now` header
    /// whose value is more than `SWFC_CLOCK_SKEW_THRESHOLD_SECS` away
    /// from the harness's `client_now`, the sender thread must set the
    /// `clock_skew_blocker` flag so the harness main loop can refuse
    /// to dispatch.
    ///
    /// Mechanism: one-shot HTTP server that parses `client_now` from
    /// the POST body, replies 204 with
    /// `X-Server-Now: <client_now + 2h>`, then shuts down. After the
    /// sender thread drains the queue the test reads
    /// `clock_skew_blocker()` and expects a `ClockSkew` variant.
    #[test]
    fn clock_skew_detected_above_threshold() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        // Serve one request: echo X-Server-Now 2 hours in the future.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set non-blocking");
        let done = Arc::new(Mutex::new(false));
        let done_clone = done.clone();
        let _server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if *done_clone.lock().unwrap() {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut reader = BufReader::new(&mut stream);
                        let mut content_length: usize = 0;
                        // consume request headers
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() {
                                break;
                            }
                            let trimmed = line.trim_end_matches(['\r', '\n']);
                            if trimmed.is_empty() {
                                break;
                            }
                            if let Some(v) = trimmed
                                .strip_prefix("Content-Length:")
                                .or_else(|| trimmed.strip_prefix("content-length:"))
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; content_length];
                        if content_length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        // Parse client_now from body and add 2 hours.
                        let server_now = serde_json::from_slice::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| {
                                v.get("client_now")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.to_string())
                            })
                            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(&ts).ok())
                            .map(|dt| {
                                let future = dt + chrono::Duration::hours(2);
                                future.to_rfc3339()
                            })
                            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                        let response = format!(
                            "HTTP/1.1 204 No Content\r\nX-Server-Now: {server_now}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        *done_clone.lock().unwrap() = true;
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });

        let pc = ProgressClient::new(
            "session-clock-skew-test",
            format!("http://127.0.0.1:{}", port),
        );
        // Pre-condition: no blocker yet.
        assert!(
            pc.clock_skew_blocker().is_none(),
            "no skew blocker before first post"
        );

        // Fire a single event so the sender thread does the first POST
        // and reads X-Server-Now from the reply.
        pc.post(&HarnessProgressEvent::bare(
            "executor_selected",
            "",
            "ready",
            "test",
        ));

        // Wait for the server to handle the request and the sender thread
        // to store the blocker (or hit the 5s deadline).
        let mut skew_observed = false;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if pc.clock_skew_blocker().is_some() {
                skew_observed = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }

        // Verify the blocker variant before dropping the client.
        if let Some(blocker) = pc.clock_skew_blocker() {
            match blocker {
                BlockerKind::ClockSkew {
                    observed_secs,
                    threshold_secs,
                } => {
                    assert!(
                        observed_secs > threshold_secs as i64,
                        "observed_secs {observed_secs} must exceed threshold {threshold_secs}"
                    );
                }
                other => panic!("expected ClockSkew blocker, got {:?}", other),
            }
        }

        drop(pc);

        assert!(
            skew_observed,
            "clock skew blocker must be set when server_now is 2h ahead of client_now"
        );
        assert!(
            *done.lock().unwrap(),
            "server must have handled at least one request"
        );
    }

    /// §9.1 — when the mock server echoes an `X-Server-Now` header
    /// within the threshold window, no `ClockSkew` blocker is set.
    #[test]
    fn clock_skew_within_threshold_no_blocker() {
        use std::io::{BufRead, BufReader, Read, Write};
        use std::net::TcpListener;
        use std::sync::{Arc, Mutex};
        use std::time::Instant;

        // Serve one request: echo X-Server-Now matching client_now (0s skew).
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind port 0");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).expect("set non-blocking");
        let done = Arc::new(Mutex::new(false));
        let done_clone = done.clone();
        let _server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(10);
            while Instant::now() < deadline {
                if *done_clone.lock().unwrap() {
                    return;
                }
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
                        let mut reader = BufReader::new(&mut stream);
                        let mut content_length: usize = 0;
                        loop {
                            let mut line = String::new();
                            if reader.read_line(&mut line).is_err() {
                                break;
                            }
                            let trimmed = line.trim_end_matches(['\r', '\n']);
                            if trimmed.is_empty() {
                                break;
                            }
                            if let Some(v) = trimmed
                                .strip_prefix("Content-Length:")
                                .or_else(|| trimmed.strip_prefix("content-length:"))
                            {
                                content_length = v.trim().parse().unwrap_or(0);
                            }
                        }
                        let mut body = vec![0u8; content_length];
                        if content_length > 0 {
                            let _ = reader.read_exact(&mut body);
                        }
                        // Echo client_now back (0s skew).
                        let server_now = serde_json::from_slice::<serde_json::Value>(&body)
                            .ok()
                            .and_then(|v| {
                                v.get("client_now")
                                    .and_then(|c| c.as_str())
                                    .map(|s| s.to_string())
                            })
                            .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
                        let response = format!(
                            "HTTP/1.1 204 No Content\r\nX-Server-Now: {server_now}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        );
                        let _ = stream.write_all(response.as_bytes());
                        let _ = stream.shutdown(std::net::Shutdown::Both);
                        *done_clone.lock().unwrap() = true;
                        return;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => return,
                }
            }
        });

        let pc = ProgressClient::new(
            "session-clock-ok-test",
            format!("http://127.0.0.1:{}", port),
        );

        pc.post(&HarnessProgressEvent::bare(
            "executor_selected",
            "",
            "ready",
            "test",
        ));

        // Wait for the server to handle the request.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if *done.lock().unwrap() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Small extra window for the sender thread to process the reply.
        std::thread::sleep(Duration::from_millis(100));

        // No blocker should be set because the skew is 0 seconds.
        assert!(
            pc.clock_skew_blocker().is_none(),
            "no clock skew blocker expected when server_now == client_now"
        );

        drop(pc);
    }
}
