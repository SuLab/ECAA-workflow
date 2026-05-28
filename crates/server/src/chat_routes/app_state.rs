//! `ChatAppState` + per-session execution handles + rate-bucket
//! helpers. Lives outside `chat_routes/mod.rs` so the top-level
//! module stays merge-only.

use super::{
    dispositions, event_sink, wire_types::ArtifactRef, EnvelopedEvent, GitHookPool,
    IdempotencyStore, SsePayload,
};
use axum::http::StatusCode;
use dashmap::{DashMap, DashSet};
use governor::{DefaultDirectRateLimiter, Quota, RateLimiter};
use ecaa_workflow_conversation::{
    AnthropicClient, BatcherConfig, ConversationService, HarnessBatcher, LlmBackend,
    MockLlmBackend, ServiceEventSink, SessionId, SessionStore,
};
use ecaa_workflow_core::config::Config;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, RwLock};

/// Per-session execution handle tracked by `ChatAppState::executions`.
/// The harness runs as a detached tokio::process::Child; we keep the
/// metadata needed for status reporting + a watcher task updates
/// `exit_status` when the child reaps. The actual stdin/stdout are
/// inherited so the server's own logs stream them.
///
/// Lifecycle flags drive the four execution-control endpoints:
/// - `/execution/pause` flips `pause_requested = true` and writes a
///   `runtime/.harness-pause` sentinel file. The harness checks the
///   file at the top of each iteration and at agent dispatch time;
///   when set, the harness suspends iterations + writes
///   `runtime/.harness-paused` (read by `/execution` for status).
/// - `/execution/resume` clears both flags + the sentinel files.
/// - `/execution/stop` flips `stop_requested = true` and writes
///   `runtime/.harness-stop`. The harness on its next iteration:
///   SIGTERMs the in-flight agent, marks the in-flight task
///   state back to `ready` (NOT `running` — prevents
///   orphan-recovery false-fire), archives its WAL line, exits 0.
/// - `/execution/kill` sends SIGTERM to the harness's pgid (kills
///   harness + agent + claude tree atomically). No state cleanup —
///   SME confirms via modal that they accept the dirty state.
///
/// Sentinel for `ExecutionHandle::exit_status` meaning "not yet exited".
///
/// `exit_status` is an `Arc<AtomicI64>` so the watcher task (which writes
/// the exit code when `child.wait()` reaps) and the read-only status /
/// pause / stop / kill handlers can synchronize without a Mutex — the
/// prior `Arc<Mutex<Option<i32>>>` design was poison-prone: a panic
/// in any holder bricked status reporting for the session. `i32::MIN`
/// (= `-2_147_483_648`) is reserved as the "unset" marker because POSIX
/// exit codes are limited to 0..=255 and Rust signal-derived
/// `ExitStatus::code()` falls back to `-1` on signal — neither path
/// can collide with `i32::MIN`.
pub(crate) const EXIT_STATUS_UNSET: i64 = i32::MIN as i64;

#[derive(Debug, Clone)]
pub struct ExecutionHandle {
    /// OS process ID of the running harness subprocess.
    pub pid: u32,
    /// POSIX process group id — used by `/execution/kill` to take down
    /// the entire harness → agent-shell → npm → claude subtree
    /// atomically. The harness sets up its own pgid via setsid() (see
    /// the spawn path in execution.rs).
    pub pgid: u32,
    /// Timestamp when the harness was launched.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Absolute path to the package directory the harness operates on.
    pub package_dir: std::path::PathBuf,
    /// Full agent command string passed to the harness.
    pub agent_command: String,
    /// Lock-free exit-code slot. `EXIT_STATUS_UNSET` (= `i32::MIN as i64`)
    /// means "still running"; any other value is the i32 exit code stored
    /// in the low 32 bits. Read via `exit_status_get()`; written via
    /// `exit_status_set()`. The slot is read on every status / pause /
    /// stop / kill handler call and written exactly once when the harness
    /// child reaps, so a single relaxed atomic exchange is enough and
    /// avoids the Mutex poison failure mode (the broader mutex-poison
    /// recovery wrapper in `execution/mod.rs::lock_recover` is for the
    /// remaining `Option<DateTime>` slots).
    pub exit_status: Arc<AtomicI64>,
    /// Set by `/execution/pause`, cleared by `/execution/resume`.
    pub pause_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Set by `/execution/stop`, cleared on next `start-execution`.
    pub stop_requested: Arc<std::sync::atomic::AtomicBool>,
    /// Timestamp mutexes left as `Arc<Mutex<Option<DateTime<Utc>>>>` —
    /// they're written only by their respective endpoint handlers (not
    /// the hot watcher path), and they carry an `Option<DateTime>` not
    /// a primitive so a lock-free swap would require an indirection
    /// (`Arc<AtomicPtr<DateTime>>` or similar) that buys nothing on the
    /// hot read path. The `lock_recover` helper protects them from the
    /// poison failure mode.
    /// Timestamp when the harness was paused; `None` while running or stopped.
    pub paused_at: Arc<std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
    /// Timestamp when a stop was requested; `None` until `/execution/stop` is called.
    pub stop_requested_at: Arc<std::sync::Mutex<Option<chrono::DateTime<chrono::Utc>>>>,
}

impl ExecutionHandle {
    /// Read the exit status, returning `None` while the child is still
    /// running and `Some(code)` once the watcher task has reaped it.
    /// `Acquire` pairs with the watcher's `Release` store so a reader
    /// observing the post-exit value also observes any prior writes
    /// the watcher made (it makes none, but the pairing is the correct
    /// memory-model contract).
    pub fn exit_status_get(&self) -> Option<i32> {
        match self.exit_status.load(Ordering::Acquire) {
            EXIT_STATUS_UNSET => None,
            v => Some(v as i32),
        }
    }

    /// Record the child's exit code. Called exactly once per
    /// ExecutionHandle, from the watcher task in
    /// `execution::start::spawn_harness_for_session_reserved` after
    /// `child.wait().await` returns. `Release` ordering is paired with
    /// the reader's `Acquire`.
    pub fn exit_status_set(&self, code: i32) {
        self.exit_status.store(code as i64, Ordering::Release);
    }
}

/// Per-session token-bucket rate limiter for progress POSTs. Throttles
/// runaway agents at `PROGRESS_RATE_PER_SEC` with `PROGRESS_RATE_BURST`
/// allowance.
/// Steady-state progress-POST token refill rate (tokens per second).
pub const PROGRESS_RATE_PER_SEC: f64 = 20.0;
/// Maximum burst allowance for the progress-POST rate limiter.
pub const PROGRESS_RATE_BURST: f64 = 50.0;

/// Token-bucket state for per-session progress-POST rate limiting.
#[derive(Debug)]
pub struct RateBucket {
    tokens: f64,
    last_refill: std::time::Instant,
}

impl RateBucket {
    /// Construct a bucket pre-filled with `initial_tokens` tokens.
    pub fn new(initial_tokens: f64) -> Self {
        Self {
            tokens: initial_tokens,
            last_refill: std::time::Instant::now(),
        }
    }

    /// Try to consume one token. Refills based on elapsed wall time;
    /// returns true if a token was available.
    pub fn try_take(&mut self, rate_per_sec: f64, burst: f64) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * rate_per_sec).min(burst);
        self.last_refill = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Per-session
/// per-minute rate limits on the LLM-firing chat endpoints. Each field
/// is a `DashMap<SessionId, DefaultDirectRateLimiter>` so the buckets
/// are lock-free for the common case (cache hit on an existing session)
/// while still lazily growing the map on first use.
///
/// The endpoint table is intentionally narrow: only routes that drive
/// an LLM call (Sonnet, Haiku side-call, Opus remediation) or a
/// resource-heavy spawn (`/start-execution`) are capped. Read-only
/// endpoints (state, transcript, metrics, artifact listings) are
/// sub-millisecond and protected by the global per-IP governor in
/// `crate::lib::run`.
///
/// Per-minute budgets — operator-tunable later, hard-coded for now:
/// - `/turn` — 30 / min
/// - `/score` — 6 / min
/// - `/explain` — 30 / min
/// - `/dashboard/summary` — 6 / min
/// - `/remediation-suggestions` — 6 / min
/// - `/start-execution` — 12 / min
/// - `/branch` — 6 / min
#[derive(Default)]
pub struct LlmRateBuckets {
    /// Bucket for `POST /turn` (LLM chat turns).
    pub turn: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /score` (rubric scorer side-call).
    pub score: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /explain` (explanation side-call).
    pub explain: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /dashboard/summary` (Haiku summary side-call).
    pub summary: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /task/:id/remediation-suggestions` (Opus side-call).
    pub remediation: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /start-execution`.
    pub start_exec: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
    /// Bucket for `POST /branch`.
    pub branch: DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
}

impl LlmRateBuckets {
    /// Get-or-insert a `RateLimiter` for `sid` in `bucket` and consume
    /// one token. Returns `Ok(())` on success, `Err(429)` when the
    /// bucket is empty. `per_minute` is the steady-state quota; the
    /// `governor` library treats it as both the long-run rate and the
    /// burst ceiling so a clean session can spend all `per_minute`
    /// requests in a single second, then waits.
    ///
    /// Constructing the limiter is constant-time (no allocations beyond
    /// the `Arc`); the per-session cost of the first call on a fresh
    /// session is paid once and amortizes immediately.
    pub fn check(
        bucket: &DashMap<SessionId, Arc<DefaultDirectRateLimiter>>,
        sid: SessionId,
        per_minute: u32,
    ) -> Result<(), StatusCode> {
        // `or_insert_with` runs on miss; on hit it returns the
        // existing entry without allocating. `Arc::clone` is cheap.
        let limiter = bucket
            .entry(sid)
            .or_insert_with(|| {
                let n = NonZeroU32::new(per_minute.max(1)).expect("per_minute clamped to >= 1");
                let quota = Quota::per_minute(n);
                Arc::new(RateLimiter::direct(quota))
            })
            .clone();
        limiter
            .check()
            .map(|_| ())
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)
    }
}

/// Security remediation per-session
/// sliding-window counter on auto-relaunch spawns. Caps the rate at
/// `max_per_minute` so an unblock→re-block→unblock loop cannot pump
/// harness/agent dispatches faster than the configured ceiling.
///
/// State per session: `(window_start, count_in_window)`. The window is
/// "rolling-reset": when a sample arrives after the window expired
/// (>=60s since the start), the entry is reset to `(now, 0)` before
/// the cap check. Concurrent calls for the same session serialize
/// inside DashMap's per-entry lock so two simultaneous unblocks cannot
/// double-budget the same window.
#[derive(Default)]
pub struct RelaunchTracker {
    inner: DashMap<SessionId, (Instant, u32)>,
}

impl RelaunchTracker {
    /// Construct an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns true when the caller is within budget (and consumes one
    /// token), false when the per-minute cap is hit. Callers that get
    /// `false` must skip the auto-relaunch and log a rate-limit notice
    /// so the operator can spot pathological loops in the journal.
    pub fn allow(&self, sid: SessionId, max_per_minute: u32) -> bool {
        let now = Instant::now();
        let mut entry = self.inner.entry(sid).or_insert((now, 0));
        if now.duration_since(entry.0).as_secs() >= 60 {
            *entry = (now, 0);
        }
        if entry.1 >= max_per_minute {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// Shared server state threaded through every Axum handler via `State<ChatAppState>`.
///
/// All fields are reference-counted (`Arc`) so clones are shallow.
#[derive(Clone)]
pub struct ChatAppState {
    /// Conversation service driving the LLM tool loop and session store.
    pub conversation: Arc<ConversationService>,
    /// Harness-event batcher that accumulates events and flushes them
    /// as synthetic assistant turns on a quiet window.
    pub batcher: Arc<HarnessBatcher>,
    /// Per-session SSE broadcast channels (lazy-init). `DashMap` —
    /// reads + entry-style insertions take only the per-shard lock, so
    /// fanout reads no longer contend with a single global write-lock
    /// when a fresh subscriber attaches.
    pub broadcasters: Arc<DashMap<SessionId, broadcast::Sender<EnvelopedEvent>>>,
    /// Per-session monotonic seq counter. Every
    /// SSE event is stamped with `next_sse_seq()` before broadcast so
    /// subscribers can drop out-of-order deliveries. `DashMap` for
    /// lock-free reads; per-entry `AtomicU64` so concurrent callers
    /// for the same session never observe a duplicate seq.
    pub sse_seq: Arc<DashMap<SessionId, Arc<AtomicU64>>>,
    /// Active harness subprocess per session.
    pub executions: Arc<DashMap<SessionId, ExecutionHandle>>,
    /// Sessions with a harness spawn in progress. This reserves the
    /// per-session spawn slot while slow filesystem work and
    /// `cmd.spawn()` run outside the `executions` map lock.
    pub starting_executions: Arc<DashSet<SessionId>>,
    /// Per-session token buckets (lazy-init).
    pub progress_rate_limits: Arc<DashMap<SessionId, Arc<std::sync::Mutex<RateBucket>>>>,
    /// Artifact listing cache. Key: (session_id, task_id).
    pub artifact_cache: ArtifactCache,
    /// §R8 — scorer-result cache; re-clicks within `SCORER_CACHE_TTL_SECS`
    /// return the cached score.
    pub scorer_cache: ScorerCache,
    /// Git-backed provenance config; live-reload via Arc.
    pub git_config: crate::git_routes::GitConfigHandle,
    /// Per-session disposition queue.
    pub dispositions: dispositions::DispositionQueue,
    /// Count of SSE payloads the sink path has
    /// successfully placed onto a per-session broadcast channel.
    /// Increments only when the broadcast `send` returns `Ok` (i.e.
    /// there is at least one live subscriber). Used by the
    /// `sse_fanout_no_drops` regression test to prove the sink's
    /// fanout path is not dropping events. Not surfaced via any HTTP
    /// route — observability is in the workspace logs already.
    pub sse_sent_count: Arc<AtomicU64>,
    /// Bounded permit set for SSE fanout spawns. Prevents an unbounded
    /// tokio task queue when subscribers are slow.
    pub sse_fanout_sem: Arc<tokio::sync::Semaphore>,
    /// Telemetry counter for fanout drops due to semaphore saturation.
    pub sse_fanout_dropped_count: Arc<AtomicU64>,
    /// Bounded executor for fire-and-forget git-commit hooks. Replaces
    /// the prior ad-hoc `tokio::task::spawn_blocking(hook_commit_clone)`
    /// pattern across `turns`, `branches`, `tasks::impact`, and
    /// `proposal`. Hangs in git can no longer exhaust tokio's blocking
    /// pool; over-capacity hooks are dropped with a warn log instead.
    pub git_hook_pool: Arc<GitHookPool>,
    /// Per-session
    /// per-minute rate limits for the LLM-firing chat endpoints. See
    /// `LlmRateBuckets` for the bucket list and operator budgets.
    pub llm_buckets: Arc<LlmRateBuckets>,
    /// Per-session
    /// rolling-window counter for `maybe_auto_relaunch_harness`. Capped
    /// at 4 spawns / minute / session so an SME's repeated unblock
    /// clicks (or a buggy harness re-entering Blocked seconds after
    /// each relaunch) cannot pump an open-ended chain of harness
    /// processes.
    pub relaunch_tracker: Arc<RelaunchTracker>,
    /// Per-endpoint per-minute rate-limit caps for the LLM-firing
    /// routes. Centralizes literal limits in one struct so they don't
    /// drift at each `LlmRateBuckets::check(...)` call site; env-var
    /// overrides are documented on `LlmEndpointRateLimits`.
    pub llm_rate_limits: super::_rate_limits::LlmEndpointRateLimits,
    /// Process-local LRU cache that backs the
    /// `Idempotency-Key` header semantics on high-impact mutating
    /// endpoints (`confirm`, `branch_session`, `start-execution`).
    /// A retry within `SWFC_IDEMPOTENCY_TTL_SECS` (default 1 hour)
    /// replays the cached response instead of re-firing the action.
    /// See `_idempotency.rs` for the handler-side API.
    pub idempotency: Arc<IdempotencyStore>,
    /// Typed configuration loaded once at startup from env-vars.
    /// Centralises every `std::env::var` consumer in the server so
    /// env-var parsing happens at boot, not per-request.
    pub config: Arc<Config>,
    /// Per-session cache for the `reconciled_progress_and_blocked_sync`
    /// sweep that `GET /api/chat/session/:id/state` runs on every poll. The
    /// sweep enumerates sibling-package directories and parses
    /// `WORKFLOW.json` for each candidate; with many tasks that's
    /// tens of milliseconds per state read and the UI polls every
    /// few seconds. The cache holds the result keyed by session id
    /// and is invalidated on every `set_task_state` write (the only
    /// authoritative path that mutates the values feeding the
    /// reconciliation).
    pub reconciled_progress_cache: Arc<DashMap<SessionId, ReconciledProgressEntry>>,
    /// Test-only override for the `SWFC_AUTO_TITLE` env-var gate. `None`
    /// (production) keeps the legacy "read env on every request"
    /// semantics; `Some(true)` / `Some(false)` lets unit tests pin the
    /// flag onto the app state so they don't have to mutate the
    /// process-wide env table to exercise the auto-title routes. The
    /// `auto_title` + `config` handlers consult this field first and
    /// fall back to `env_bool("SWFC_AUTO_TITLE")` when unset, so prod
    /// behavior is unchanged.
    pub auto_title_override: Option<bool>,
}

/// R2-N14 — cached reconciled-progress payload. `valid` is flipped to
/// `false` by `set_task_state`; the next reader observes the stale
/// flag and recomputes before returning.
#[derive(Debug, Clone)]
pub struct ReconciledProgressEntry {
    /// Cached aggregated task-state counts.
    pub progress: super::ProgressSummary,
    /// Cached list of task ids currently in `Blocked` state.
    pub blocked_tasks: Vec<String>,
    /// False when the entry is stale and must be recomputed.
    pub valid: bool,
}

impl ChatAppState {
    /// Return a reference to the git-provenance config handle.
    pub fn git_config(&self) -> &crate::git_routes::GitConfigHandle {
        &self.git_config
    }
}

/// Per-session scorer result cache. Key: session id → `(transcript_len, cached_at, score)`.
pub type ScorerCache = Arc<
    RwLock<
        std::collections::HashMap<
            SessionId,
            (
                usize,
                std::time::Instant,
                ecaa_workflow_conversation::RubricScore,
            ),
        >,
    >,
>;

/// §R8 — scorer cache TTL.
pub const SCORER_CACHE_TTL_SECS: u64 = 30;

/// Artifact listing cache backing-store. See [`ChatAppState::artifact_cache`].
pub type ArtifactCache = Arc<DashMap<(SessionId, String), (u64, u64, Vec<ArtifactRef>)>>;

impl ChatAppState {
    /// Construct the production `ChatAppState` from env-vars. Reads
    /// `SWFC_CHAT_SESSIONS_DIR`, `SWFC_CONFIG_DIR`, `SWFC_ANTHROPIC_API_KEY`, and
    /// `SWFC_CHAT_MODE` among others via `Config::from_env`.
    pub async fn new() -> anyhow::Result<Self> {
        let config = Arc::new(Config::from_env()?);
        let session_dir = config.chat_sessions_dir.clone();
        let store = SessionStore::open(&session_dir).await?;
        let llm: Arc<dyn LlmBackend> = if config.chat_mode
            == ecaa_workflow_core::config::ChatMode::Offline
            || ecaa_workflow_conversation::anthropic_api_key().is_none()
        {
            // Offline kill switch — empty mock means /turn returns the
            // exhausted error, so the front end can fall back to the
            // deterministic classifier path.
            Arc::new(MockLlmBackend::new(vec![]))
        } else {
            Arc::new(AnthropicClient::new()?)
        };
        let config_dir = config.config_dir.clone();
        let broadcasters: Arc<DashMap<SessionId, broadcast::Sender<EnvelopedEvent>>> =
            Arc::new(DashMap::new());
        let sse_seq: Arc<DashMap<SessionId, Arc<AtomicU64>>> = Arc::new(DashMap::new());
        let sse_sent_count = Arc::new(AtomicU64::new(0));
        let sse_fanout_sem = Arc::new(tokio::sync::Semaphore::new(128));
        let sse_fanout_dropped_count = Arc::new(AtomicU64::new(0));
        let app_once: Arc<std::sync::OnceLock<ChatAppState>> = Arc::new(std::sync::OnceLock::new());
        let sink: Arc<dyn ServiceEventSink> = Arc::new(event_sink::BroadcastEventSink {
            broadcasters: broadcasters.clone(),
            sse_seq: sse_seq.clone(),
            sse_sent_count: sse_sent_count.clone(),
            sse_fanout_sem: sse_fanout_sem.clone(),
            sse_fanout_dropped_count: sse_fanout_dropped_count.clone(),
            app: app_once.clone(),
        });
        let service =
            ConversationService::new(llm, store.clone(), config_dir).with_event_sink(sink.clone());
        let batcher = Arc::new(
            HarnessBatcher::new(store, BatcherConfig::from_env())
                .with_metrics(service.metrics().clone())
                .with_event_sink(sink),
        );
        let app = Self {
            conversation: Arc::new(service),
            batcher,
            broadcasters,
            sse_seq,
            executions: Arc::new(DashMap::new()),
            starting_executions: Arc::new(DashSet::new()),
            progress_rate_limits: Arc::new(DashMap::new()),
            artifact_cache: Arc::new(DashMap::new()),
            scorer_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            git_config: std::sync::Arc::new(crate::git_routes::GitConfigStore::open_or_default(
                crate::git_routes::git_config_path(),
            )),
            dispositions: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
            sse_sent_count,
            sse_fanout_sem,
            sse_fanout_dropped_count,
            git_hook_pool: Arc::new(GitHookPool::new(8, Duration::from_secs(30))),
            llm_buckets: Arc::new(LlmRateBuckets::default()),
            relaunch_tracker: Arc::new(RelaunchTracker::default()),
            llm_rate_limits: super::_rate_limits::LlmEndpointRateLimits::from_env(),
            idempotency: Arc::new(IdempotencyStore::from_env()),
            config,
            reconciled_progress_cache: Arc::new(DashMap::new()),
            auto_title_override: None,
        };
        let _ = app_once.set(app.clone());
        Ok(app)
    }

    /// Resolve the auto-title feature flag. Tests pin
    /// `auto_title_override` so they don't have to mutate the
    /// process-wide env table; production leaves the override as `None`
    /// and reads `SWFC_AUTO_TITLE` on every request (the documented
    /// behavior for the `auto-title` + `/api/chat/config` routes).
    pub fn auto_title_enabled(&self) -> bool {
        if let Some(v) = self.auto_title_override {
            return v;
        }
        // Production: read from the pre-loaded typed Config (one
        // env read at boot, never re-read on the hot path).
        self.config.auto_title
    }

    /// Dependency-injection constructor that takes an
    /// explicit `LlmBackend` + `SessionStore` + `config_dir`. Used by
    /// in-crate unit tests via `chat_routes::test_support::make_router`
    /// and by integration tests under `crates/server/tests/` that need
    /// a working ChatAppState without an `ANTHROPIC_API_KEY` or the
    /// production `~/.scripps-workflow/sessions` directory.
    pub fn with_backend(
        llm: Arc<dyn LlmBackend>,
        store: SessionStore,
        config_dir: PathBuf,
    ) -> Self {
        let broadcasters: Arc<DashMap<SessionId, broadcast::Sender<EnvelopedEvent>>> =
            Arc::new(DashMap::new());
        let sse_seq: Arc<DashMap<SessionId, Arc<AtomicU64>>> = Arc::new(DashMap::new());
        let sse_sent_count = Arc::new(AtomicU64::new(0));
        let sse_fanout_sem = Arc::new(tokio::sync::Semaphore::new(128));
        let sse_fanout_dropped_count = Arc::new(AtomicU64::new(0));
        let app_once: Arc<std::sync::OnceLock<ChatAppState>> = Arc::new(std::sync::OnceLock::new());
        let sink: Arc<dyn ServiceEventSink> = Arc::new(event_sink::BroadcastEventSink {
            broadcasters: broadcasters.clone(),
            sse_seq: sse_seq.clone(),
            sse_sent_count: sse_sent_count.clone(),
            sse_fanout_sem: sse_fanout_sem.clone(),
            sse_fanout_dropped_count: sse_fanout_dropped_count.clone(),
            app: app_once.clone(),
        });
        let git_config_path = store.dir().join("git-config.json");
        let service = ConversationService::new(llm, store.clone(), config_dir.clone())
            .with_event_sink(sink.clone());
        let batcher = Arc::new(
            HarnessBatcher::new(store, BatcherConfig::from_env())
                .with_metrics(service.metrics().clone())
                .with_event_sink(sink),
        );
        let app = Self {
            conversation: Arc::new(service),
            batcher,
            broadcasters,
            sse_seq,
            executions: Arc::new(DashMap::new()),
            starting_executions: Arc::new(DashSet::new()),
            progress_rate_limits: Arc::new(DashMap::new()),
            artifact_cache: Arc::new(DashMap::new()),
            scorer_cache: Arc::new(RwLock::new(std::collections::HashMap::new())),
            git_config: std::sync::Arc::new(crate::git_routes::GitConfigStore::open_or_default(
                git_config_path,
            )),
            dispositions: Arc::new(RwLock::new(std::collections::BTreeMap::new())),
            sse_sent_count,
            sse_fanout_sem,
            sse_fanout_dropped_count,
            git_hook_pool: Arc::new(GitHookPool::new(8, Duration::from_secs(30))),
            llm_buckets: Arc::new(LlmRateBuckets::default()),
            relaunch_tracker: Arc::new(RelaunchTracker::default()),
            llm_rate_limits: super::_rate_limits::LlmEndpointRateLimits::from_env(),
            idempotency: Arc::new(IdempotencyStore::from_env()),
            config: Arc::new(Config::for_test().config_dir(config_dir).build()),
            reconciled_progress_cache: Arc::new(DashMap::new()),
            auto_title_override: None,
        };
        let _ = app_once.set(app.clone());
        app
    }

    pub(super) async fn broadcaster(&self, id: SessionId) -> broadcast::Sender<EnvelopedEvent> {
        // DashMap `entry()` holds a per-shard write lock for the closure
        // only; the get-or-insert + clone is atomic per shard. Capacity
        // raised from 64 → 256 so harness bursts (task_started fan-outs,
        // state_advanced, task_completed_reviewable) don't become silent
        // drops.
        self.broadcasters
            .entry(id)
            .or_insert_with(|| broadcast::channel(256).0)
            .value()
            .clone()
    }

    /// Mint the next monotonic sequence number for
    /// session `id`. Per-session seqs start at 1 and increment by 1 on
    /// each call. Uses `Relaxed` ordering: program-order causality on a
    /// per-session atomic is sufficient — cross-session ordering doesn't
    /// matter to clients, and `Relaxed` avoids the cross-core cache-line
    /// flush that `SeqCst` would impose on the SSE hot path.
    pub fn next_sse_seq(&self, id: SessionId) -> u64 {
        let entry = self
            .sse_seq
            .entry(id)
            .or_insert_with(|| Arc::new(AtomicU64::new(0)))
            .clone();
        entry.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Atomic get-or-insert + subscribe. The previous events_stream path
    /// called `broadcaster(id).await` then `tx.subscribe()` as two
    /// separate steps; any `fanout()` that arrived between the two could
    /// publish into the channel before our receiver was attached and the
    /// first events of a fresh session were lost without a resync signal.
    /// `DashMap::entry` locks the shard for the closure body, so the
    /// get-or-insert + subscribe is atomic per shard: any later sink-path
    /// fanout takes the same shard lock and observes the channel after
    /// our receiver is registered.
    pub async fn broadcaster_subscribe(
        &self,
        id: SessionId,
    ) -> broadcast::Receiver<EnvelopedEvent> {
        let entry = self
            .broadcasters
            .entry(id)
            .or_insert_with(|| broadcast::channel(256).0);
        entry.subscribe()
    }

    /// Broadcast a single SSE payload to all subscribers for `id`.
    pub async fn broadcast(&self, id: SessionId, payload: SsePayload) {
        let tx = self.broadcaster(id).await;
        let seq = self.next_sse_seq(id);
        if tx.send(EnvelopedEvent { seq, payload }).is_ok() {
            self.sse_sent_count.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Sink-style fanout from a sync context. The broadcasters store is
    /// a `DashMap`, so reads are lock-free at the shard level and the
    /// only contention with a parallel writer (lazy-init of a fresh
    /// subscriber's channel) is on the per-shard lock — microseconds in
    /// practice. Each fanned-out event carries a per-session monotonic
    /// `seq` so clients can detect drops between resyncs. The spawned
    /// task holds a semaphore permit for its full lifetime to bound the
    /// in-flight tokio task count.
    ///
    /// The trait method on `BroadcastEventSink` delegates here.
    pub fn spawn_fanout(&self, id: SessionId, payload: SsePayload) {
        // Bound the tokio task queue: if 128 fanout spawns are already
        // outstanding (subscribers slow / broadcasters shard burst),
        // drop the event and bump telemetry rather than letting the queue
        // grow unbounded.
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
        let seq = self.next_sse_seq(id);
        tokio::spawn(async move {
            let _permit = permit; // held across send for the full task
            if let Some(tx) = broadcasters.get(&id).map(|e| e.value().clone()) {
                if tx.send(EnvelopedEvent { seq, payload }).is_ok() {
                    counter.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    /// Snapshot of the running fanout-success counter. Used by the
    /// `sse_fanout_no_drops` regression test.
    pub fn sse_sent_count(&self) -> u64 {
        self.sse_sent_count.load(Ordering::Relaxed)
    }

    /// Drop every cached artifact listing for `task_id` across all
    /// sessions. Called by rerun paths so the next `get_task_result`
    /// re-scans instead of serving the pre-rerun listing.
    pub async fn invalidate_artifact_cache_for_task(&self, task_id: &str) {
        self.artifact_cache.retain(|(_, tid), _| tid != task_id);
    }

    /// Try to consume a progress-event token for this session. Returns
    /// true when the caller should proceed with full progress handling
    /// (store.update + state transitions + metrics); false means the
    /// session is bursting past PROGRESS_RATE_PER_SEC and should take
    /// the light path (batcher enqueue only).
    pub async fn try_consume_progress_token(&self, id: SessionId) -> bool {
        let bucket = self
            .progress_rate_limits
            .entry(id)
            .or_insert_with(|| {
                Arc::new(std::sync::Mutex::new(RateBucket::new(PROGRESS_RATE_BURST)))
            })
            .value()
            .clone();
        let mut b = bucket.lock().unwrap_or_else(|p| p.into_inner());
        b.try_take(PROGRESS_RATE_PER_SEC, PROGRESS_RATE_BURST)
    }
}

#[allow(dead_code)]
pub(crate) fn sessions_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SWFC_CHAT_SESSIONS_DIR") {
        return PathBuf::from(d);
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".scripps-workflow/sessions");
    }
    PathBuf::from("./.scripps-sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    /// First `max_per_minute` calls in a fresh
    /// window must all succeed; the (`max+1`)th must be denied.
    #[test]
    fn relaunch_tracker_admits_first_max_then_denies() {
        let tracker = RelaunchTracker::new();
        let sid = Uuid::new_v4();
        for _ in 0..4 {
            assert!(tracker.allow(sid, 4));
        }
        assert!(
            !tracker.allow(sid, 4),
            "5th call in the same window must be denied"
        );
    }

    /// Independent sessions get independent budgets — a noisy session
    /// must not consume budget belonging to a quiet one.
    #[test]
    fn relaunch_tracker_per_session_isolation() {
        let tracker = RelaunchTracker::new();
        let s1 = Uuid::new_v4();
        let s2 = Uuid::new_v4();
        for _ in 0..4 {
            assert!(tracker.allow(s1, 4));
        }
        assert!(!tracker.allow(s1, 4));
        // s2 has its own counter and is unaffected.
        assert!(tracker.allow(s2, 4));
    }

    /// `with_backend` produces a `ChatAppState` whose `config` field is an
    /// `Arc<Config>` loaded from the `for_test()` builder. Verifies the
    /// Config is present and the config_dir matches what was passed in.
    #[test]
    fn with_backend_config_field_is_populated() {
        use ecaa_workflow_conversation::MockLlmBackend;
        use std::sync::Arc;
        use tempfile::tempdir;

        let tmp = tempdir().unwrap();
        let store = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(ecaa_workflow_conversation::SessionStore::open(
                tmp.path(),
            ))
            .unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let llm: Arc<dyn ecaa_workflow_conversation::LlmBackend> =
            Arc::new(MockLlmBackend::new(vec![]));
        let state = ChatAppState::with_backend(llm, store, config_dir.clone());
        // The config field must be present (not panicking means Arc is set).
        assert_eq!(
            state.config.config_dir, config_dir,
            "config.config_dir must match the injected config_dir"
        );
        // auto_title defaults to false in for_test().
        assert!(
            !state.config.auto_title,
            "auto_title must default to false in test config"
        );
    }
}
