//! On-disk session store with atomic write-through.
//!
//! Each session lives at `<dir>/<session_id>.json`. Writes go through a
//! `.tmp` sibling and `rename` to keep the store recoverable across crashes.
//! Sessions inactive for more than 30 days are pruned at load time.
//!
//! per-session locking. The outer `DashMap`'s internal
//! sharding lets concurrent sessions take their per-session locks
//! without contending on a single map-level lock. Each session
//! state plus its disk write is serialized by its own
//! `tokio::sync::Mutex`. Concurrent turns on *different* sessions
//! proceed in parallel; concurrent turns on the *same* session still
//! serialize, preserving the lost-update fix documented below.

use crate::session::{Session, SessionId};
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use tokio::sync::Mutex;

const TTL_DAYS: i64 = 30;

tokio::task_local! {
    /// Re-entrancy probe for `SessionStore::transaction`. Set to `true`
    /// for the duration of `transaction`'s closure body via
    /// `tokio::task_local!::scope(...)`. `ToolContext::with_store`
    /// `debug_assert!`s the inverse so any future wiring that hands a
    /// re-entrant store handle to a tool dispatched inside a transaction
    /// trips at the wiring site instead of deadlocking on the tokio
    /// Mutex.
    ///
    /// This is task-scoped (not thread-scoped): the value propagates
    /// across `.await` within the SAME task and is invisible to every
    /// other task. A thread-local equivalent would leak state to
    /// unrelated tasks when the tokio worker is reused after an `.await`
    /// boundary inside a transaction body — e.g. a brand-new session's
    /// POST /turn would observe a leftover `IN_TRANSACTION = true` flag
    /// and trip the `with_store` assertion despite being a separate
    /// session with no transaction at all.
    static IN_TRANSACTION: bool;
}

/// Public probe used by `ToolContext::with_store`'s `debug_assert!`.
/// Returns whether the current task is inside a `SessionStore::transaction`
/// body. Returns `false` when called from outside any
/// `IN_TRANSACTION::scope(...)` (the canonical state for every tool-loop
/// dispatch that runs from `send_turn`).
pub fn in_transaction() -> bool {
    IN_TRANSACTION.try_with(|v| *v).unwrap_or(false)
}

/// Listing-grade metadata projected from a `Session`.
///
/// The lineage-render path (`branches.rs::list_recent_sessions`,
/// `service::children_of`) calls `iter_sessions` for every request,
/// which forces a full disk scan + deserialize over the entire store.
/// With 1000 sessions on disk that's seconds of wall clock per click.
///
/// `SessionMetadata` carries the subset of fields those listings
/// actually project (id, owner, title, lineage parent, state-kind
/// label, n_turns, project_class label, last_activity) and is held in
/// an in-memory `DashMap` that's populated lazily as sessions are
/// loaded and updated on every `save()` / `update()` / `prune()`. Cached
/// listings then return without ever touching the disk.
#[derive(Debug, Clone)]
pub struct SessionMetadata {
    /// Unique session identifier.
    pub id: SessionId,
    /// User-facing session title (auto-populated by the Haiku side-call after 3+ turns).
    pub title: Option<String>,
    /// Username or user-id of the session owner.
    pub owner_user: String,
    /// UTC timestamp when the session was first created.
    pub created_at: DateTime<Utc>,
    /// UTC timestamp of the most recent activity.
    pub last_activity: DateTime<Utc>,
    /// Parent session id when this session was branched.
    pub parent_session_id: Option<SessionId>,
    /// Lineage path-summary: `(parent_id, branched_at, branched_from_turn_index)`.
    /// None when the session was not branched.
    pub lineage_summary: Option<LineageSummary>,
    /// Static label of the `SessionState` variant (e.g. `"intake"`,
    /// `"emitted"`, `"blocked"`). Matches `session_state_kind` so the
    /// `/sessions/recent` shape can be served from the cache without
    /// re-walking the SessionState enum.
    pub state_kind: &'static str,
    /// Number of turns in the session's conversation history.
    pub n_turns: usize,
    /// `format!("{:?}", session.project_class)` — kept as a string so
    /// the metadata projection is self-contained (no enum import from
    /// `core` needed by the cache key path).
    pub project_class: String,
}

/// Compact lineage record stored on `SessionMetadata` so the session
/// listing endpoint can render the branch tree without loading full sessions.
#[derive(Debug, Clone)]
pub struct LineageSummary {
    /// Session id this session was branched from.
    pub parent_session_id: SessionId,
    /// UTC timestamp when the branch was created.
    pub branched_at: DateTime<Utc>,
    /// Turn index in the parent session at which the branch was made.
    pub branched_from_turn_index: Option<usize>,
}

impl SessionMetadata {
    /// Project a `Session` into its listing-grade subset. Called from
    /// `save`, `update`, and the lazy load path so the cache stays
    /// coherent with the source `Session`.
    pub fn from_session(s: &Session) -> Self {
        Self {
            id: s.id,
            title: s.title.clone(),
            owner_user: s.owner_user.clone(),
            created_at: s.created_at,
            last_activity: s.last_activity,
            parent_session_id: s.lineage.as_ref().map(|l| l.parent_session_id),
            lineage_summary: s.lineage.as_ref().map(|l| LineageSummary {
                parent_session_id: l.parent_session_id,
                branched_at: l.branched_at,
                branched_from_turn_index: l.branched_from_turn_index,
            }),
            state_kind: state_kind_label(&s.state),
            n_turns: s.conversation.len(),
            project_class: format!("{:?}", s.project_class),
        }
    }
}

/// Static-label map for SessionState variants. Mirrors
/// `chat_routes::wire_types::session_state_kind`; duplicated here
/// because `core` and `server` do not share `wire_types`. Kept as a
/// thin match so a new variant added in `session::state` produces a
/// compiler error here.
fn state_kind_label(state: &crate::session::SessionState) -> &'static str {
    use crate::session::SessionState as S;
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
    }
}

/// Callback fired for each session id removed by a prune sweep.
/// The service layer uses this to clear its own per-session maps
/// when the backing session file expires.
pub type PruneHook = Arc<dyn Fn(SessionId) + Send + Sync + 'static>;

/// Durable atomic-write helper. Writes `bytes` to
/// `tmp_path`, fsyncs the file, renames to `final_path`, then fsyncs
/// the parent directory so the rename itself is durable.
///
/// Without the parent-directory fsync, a power loss between rename
/// and the implicit dir flush can lose the directory entry — the
/// `final_path` file disappears, even though we already
/// successfully wrote + fsynced its bytes. Catches the rename half
/// of the "tempfile::persist() footgun" the plan explicitly warns
/// against.
/// Public wrapper around `atomic_write_bytes` that derives the
/// temp-path from `final_path` (appends `.tmp`). Use this from
/// callers outside `SessionStore` (e.g. `emit::cross_version_diff`)
/// to get the same `.tmp + fsync + rename + parent fsync` durability
/// guarantee without having to manage two paths.
pub async fn atomic_write_bytes_to(final_path: &Path, bytes: &[u8]) -> Result<()> {
    use std::ffi::OsString;
    let mut tmp_os: OsString = final_path.as_os_str().to_owned();
    tmp_os.push(".tmp");
    let tmp_path: PathBuf = tmp_os.into();
    atomic_write_bytes(&tmp_path, final_path, bytes).await
}

async fn atomic_write_bytes(tmp_path: &Path, final_path: &Path, bytes: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;
    // Resilience: SessionStore::open creates the directory at boot, but the
    // dir can be deleted out from under us by external tooling (e.g. test
    // harnesses cleaning /tmp between runs). Auto-recreate the parent
    // directory on ENOENT instead of returning 500 — the alternative is a
    // fragile invariant that every caller of `save` must check.
    let mut tmp = match tokio::fs::File::create(tmp_path).await {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = tmp_path.parent() {
                tokio::fs::create_dir_all(parent).await.with_context(|| {
                    format!("recreating session-store parent dir '{}'", parent.display())
                })?;
            }
            tokio::fs::File::create(tmp_path).await.with_context(|| {
                format!(
                    "creating tmp file '{}' after dir recreate",
                    tmp_path.display()
                )
            })?
        }
        Err(e) => {
            return Err(e).with_context(|| format!("creating tmp file '{}'", tmp_path.display()));
        }
    };
    tmp.write_all(bytes)
        .await
        .with_context(|| format!("writing tmp file '{}'", tmp_path.display()))?;
    tmp.sync_data()
        .await
        .with_context(|| format!("fsyncing tmp file '{}'", tmp_path.display()))?;
    drop(tmp);
    tokio::fs::rename(tmp_path, final_path)
        .await
        .with_context(|| {
            format!(
                "renaming '{}' → '{}'",
                tmp_path.display(),
                final_path.display()
            )
        })?;
    if let Some(parent) = final_path.parent() {
        // Open the parent dir for fsync. Linux dir fsync requires
        // O_RDONLY which std::fs::File::open does by default;
        // tokio::fs::File doesn't expose the OpenOptions::custom_flags
        // path so we use a blocking call — bounded duration (one
        // syscall per rename), worth the simplicity.
        let parent_owned = parent.to_path_buf();
        tokio::task::spawn_blocking(move || {
            std::fs::File::open(&parent_owned)
                .and_then(|f| f.sync_data())
                .map_err(|e| anyhow!("dir fsync failed for {}: {}", parent_owned.display(), e))
        })
        .await
        .map_err(|e| anyhow!("dir-fsync join error: {}", e))??;
    }
    Ok(())
}

type SessionHandle = Arc<Mutex<Session>>;

/// Persistent session store backed by a flat directory of JSON files.
/// Provides an in-memory `DashMap` cache with lazy population and
/// TTL-based pruning for sessions inactive >30 days.
#[derive(Clone)]
pub struct SessionStore {
    dir: PathBuf,
    inner: Arc<DashMap<SessionId, SessionHandle>>,
    /// In-memory `(id → SessionMetadata)` cache.
    /// Populated lazily on first disk read for each session and
    /// updated by every `save()` / `update()` / `prune_expired_now()`.
    /// `iter_session_metadata` returns a clone without touching the
    /// disk, so listing endpoints scale O(in-memory map size) rather
    /// than O(N session-files-on-disk).
    metadata: Arc<DashMap<SessionId, SessionMetadata>>,
    /// Tracks whether the metadata cache has been populated from disk
    /// for this process lifetime. The first `iter_session_metadata`
    /// call lazily fills the cache by enumerating the directory and
    /// loading each file once; subsequent calls hit the cache.
    metadata_populated: Arc<std::sync::atomic::AtomicBool>,
    prune_hook: Arc<OnceLock<PruneHook>>,
}

impl SessionStore {
    /// Open (creating if needed) the on-disk session directory.
    ///
    /// startup no longer loads every session file into memory.
    /// The in-memory map begins empty; `get`, `update`, and
    /// `iter_sessions` lazy-load from disk on first access. A
    /// background task prunes expired files (batches of 100 with a
    /// 1 s inter-batch yield) so a 10 k-file store no longer stalls
    /// server startup on O(s) deserialize. Deterministic callers
    /// that need prune-before-observe semantics can await
    /// `prune_expired_now()`.
    /// Expose the directory for callers that want to colocate
    /// sidecar files (e.g., MetricsStore writing `*.metrics.json`).
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Open (or create) the on-disk session store at `dir`.
    pub async fn open(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        tokio::fs::create_dir_all(&dir)
            .await
            .with_context(|| format!("creating session store dir '{}'", dir.display()))?;

        let store = Self {
            dir,
            inner: Arc::new(DashMap::new()),
            metadata: Arc::new(DashMap::new()),
            metadata_populated: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            prune_hook: Arc::new(OnceLock::new()),
        };

        // Kick off the background prune. If the task panics or the
        // runtime is dropped before it completes, the next startup
        // will pick up the unprune'd files. After the initial sweep,
        // loop on `ECAA_TTL_PRUNE_INTERVAL_SECS` (default 3600s) so
        // a long-running server still reaps expired sessions during
        // its lifetime. A recurring sweep ensures expired sessions
        // are reaped during the server's lifetime, not just at boot.
        //
        // Wrap each iteration's body in `AssertUnwindSafe + catch_unwind`
        // so a panic from `prune_expired_now` (e.g. an I/O backend
        // panic, an unwrap on a corrupt sidecar) doesn't terminate the
        // whole TTL loop and silently leak prune work indefinitely.
        let bg = store.clone();
        tokio::spawn(async move {
            use futures::FutureExt;
            let _ = std::panic::AssertUnwindSafe(bg.prune_expired_now())
                .catch_unwind()
                .await;
            let interval = ttl_prune_interval_from_env();
            loop {
                tokio::time::sleep(interval).await;
                let bg_iter = bg.clone();
                let outcome =
                    std::panic::AssertUnwindSafe(async move { bg_iter.prune_expired_now().await })
                        .catch_unwind()
                        .await;
                match outcome {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => {
                        tracing::warn!(error = %err, "recurring TTL prune failed");
                    }
                    Err(_panic) => {
                        tracing::error!("recurring TTL prune panicked; loop continues");
                    }
                }
            }
        });

        Ok(store)
    }

    /// Register a callback fired once per session id removed during
    /// `prune_expired_now`. Idempotent: only the first registration
    /// wins for a store handle and its clones.
    pub fn set_prune_hook<F>(&self, hook: F)
    where
        F: Fn(SessionId) + Send + Sync + 'static,
    {
        let _ = self.prune_hook.set(Arc::new(hook));
    }

    /// synchronously prune every expired file in the store
    /// directory. Called by `open` via `tokio::spawn` for background
    /// work; tests and operators who need deterministic prune
    /// semantics can await this directly. Batches 100 files between
    /// 1 s yields so a 10 k-file sweep doesn't starve the reactor.
    pub async fn prune_expired_now(&self) -> Result<()> {
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        let cutoff = Utc::now() - Duration::days(TTL_DAYS);
        let mut batch_count: usize = 0;
        let mode = SessionLoadMode::from_env();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            // Skip sidecar files written by MetricsStore as
            // `<session_id>.metrics.json`. They share the `.json`
            // extension but are not Session payloads — attempting to
            // deserialize them as Session was the source of boot-time
            // `[session_load_corrupt]` spam.
            if is_sidecar_filename(&path) {
                continue;
            }
            // Permissive-mode load so a corrupt file
            // doesn't stall the prune sweep. Strict mode (CI/test)
            // surfaces the error.
            let load = match read_session_with_mode(&path, mode).await {
                Ok(Some(s)) => Some(s),
                Ok(None) => None,
                Err(e) => return Err(e),
            };
            if let Some(s) = load {
                if is_expired(&s.last_activity, cutoff) {
                    let _ = tokio::fs::remove_file(&path).await;
                    self.inner.remove(&s.id);
                    // R2-N13: keep the metadata cache coherent with
                    // prune removals.
                    self.metadata.remove(&s.id);
                    if let Some(hook) = self.prune_hook.get() {
                        hook(s.id);
                    }
                }
            }
            batch_count += 1;
            if batch_count.is_multiple_of(100) {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        Ok(())
    }

    /// R3.5 — crash-recovery for sessions persisted in `Emitting`.
    /// A server crash mid-emit leaves the session stuck without a UI
    /// affordance to recover. Use a conservative 120s staleness
    /// threshold so a long-running but legitimate emit isn't
    /// misclassified — `emit_package` typically completes in
    /// seconds; minutes of inactivity in Emitting almost always
    /// means the host process died.
    async fn recover_stale_emitting_on_load(&self, session: &mut Session) {
        const EMIT_STALE_SECS: i64 = 120;
        if session.recover_stale_emitting(EMIT_STALE_SECS) {
            // Best-effort backfill write so the recovered Blocked state
            // is durable on the next load too. Failure is non-fatal:
            // the next load will re-trigger the recovery.
            let tmp_path = self.dir.join(format!("{}.json.tmp", session.id));
            let final_path = self.dir.join(format!("{}.json", session.id));
            if let Ok(bytes) = serde_json::to_vec_pretty(&session) {
                if let Err(e) = atomic_write_bytes(&tmp_path, &final_path, &bytes).await {
                    tracing::debug!(
                        session_id = %session.id,
                        err = %e,
                        "recover_stale_emitting write failed (non-fatal)"
                    );
                }
            }
        }
    }

    /// Migration (C5): sessions persisted before the audit_writer_secret
    /// field existed deserialize with a zero-byte sentinel via
    /// `default_audit_writer_secret()`. Generate a real secret now and
    /// write it back so HMAC sidecars on this session are verifiable
    /// going forward. New secret applies forward only — previously-signed
    /// rows (from a prior emit under a different in-memory secret) are not
    /// re-verified here.
    async fn migrate_audit_writer_secret_on_load(&self, session: &mut Session) {
        if session.audit_writer_secret == [0u8; 32] {
            use rand::RngCore;
            rand::rngs::OsRng.fill_bytes(&mut session.audit_writer_secret);
            // Best-effort backfill write. Failure is non-fatal — the next
            // process load will regenerate again. We log at debug level to
            // avoid spamming the operator on every load of a legacy session.
            let tmp_path = self.dir.join(format!("{}.json.tmp", session.id));
            let final_path = self.dir.join(format!("{}.json", session.id));
            if let Ok(bytes) = serde_json::to_vec(&session) {
                if let Err(e) = atomic_write_bytes(&tmp_path, &final_path, &bytes).await {
                    tracing::debug!(
                        session_id = %session.id,
                        err = %e,
                        "audit_writer_secret migration write failed (non-fatal)"
                    );
                }
            }
        }
    }

    /// Reconcile `task_states` against the on-disk `WORKFLOW.json`
    /// when an emitted package exists. The harness writes task
    /// transitions to BOTH the server (via POST /task/.../state) AND
    /// its local WORKFLOW.json (`write_dag` in the harness). If the
    /// server was down when a POST fired, the in-memory map and the
    /// persisted `session.task_states` lag the on-disk truth. Without
    /// this catch-up, the next `GET /state` reports stale counts and
    /// every downstream `current_dag()` consumer (dashboard, summary,
    /// execution status, task results, impact) overlays stale state.
    ///
    /// Bounded: runs once per session per server boot (the
    /// `ensure_loaded` cache-miss path), and only when the package
    /// exists on disk. WORKFLOW.json parse is cheap (single file read,
    /// tasks map walk).
    async fn reconcile_task_states_on_load(&self, session: &mut Session) {
        let Some(updates) = reconcile_task_states_from_workflow_json(session).await else {
            return;
        };
        if updates.is_empty() {
            return;
        }
        let updated_count = updates.len();
        for (task_id, new_state) in updates {
            session.set_task_state(&task_id, new_state);
        }
        tracing::info!(
            session_id = %session.id,
            reconciled = updated_count,
            "reconciled task_states from WORKFLOW.json on session load"
        );
        // Best-effort backfill so subsequent process loads start
        // already reconciled. Failure is non-fatal — the
        // reconciliation will simply re-run next boot.
        let tmp_path = self.dir.join(format!("{}.json.tmp", session.id));
        let final_path = self.dir.join(format!("{}.json", session.id));
        if let Ok(bytes) = serde_json::to_vec_pretty(&session) {
            if let Err(e) = atomic_write_bytes(&tmp_path, &final_path, &bytes).await {
                tracing::debug!(
                    session_id = %session.id,
                    err = %e,
                    "task_states reconciliation write failed (non-fatal)"
                );
            }
        }
    }

    /// resolve a session id to a locked handle, falling back
    /// to a one-file disk read when the in-memory cache misses.
    /// Expired files observed here are removed in passing so a stale
    /// id never returns a zombie handle.
    async fn ensure_loaded(&self, id: SessionId) -> Option<SessionHandle> {
        if let Some(h) = self.inner.get(&id) {
            return Some(h.clone());
        }
        let path = self.dir.join(format!("{}.json", id));
        let mut session = match read_session(&path).await {
            Ok(s) => s,
            Err(_) => return None,
        };
        let cutoff = Utc::now() - Duration::days(TTL_DAYS);
        if is_expired(&session.last_activity, cutoff) {
            let _ = tokio::fs::remove_file(&path).await;
            return None;
        }
        self.recover_stale_emitting_on_load(&mut session).await;
        self.migrate_audit_writer_secret_on_load(&mut session).await;
        self.reconcile_task_states_on_load(&mut session).await;
        // R2-N13 — refresh the metadata cache as a side effect of
        // the lazy load. Doing this *before* the entry::or_insert
        // means a concurrent winner's handle reads the same cached
        // metadata snapshot; the first writer wins on both maps.
        self.metadata
            .insert(id, SessionMetadata::from_session(&session));
        // DashMap::entry is the race-safe "insert if absent" pattern:
        // if two concurrent loads land, the first wins and the second
        // gets the same handle.
        let handle = self
            .inner
            .entry(id)
            .or_insert_with(|| Arc::new(Mutex::new(session)))
            .clone();
        Some(handle)
    }

    /// Populate the metadata cache from every
    /// session file on disk. Idempotent: the
    /// `metadata_populated` flag flips after the first successful
    /// sweep so subsequent listing calls return without re-walking the
    /// directory. Cheap on the first call (one read + parse per file,
    /// no Session-shape allocation kept in memory).
    async fn ensure_metadata_populated(&self) {
        if self
            .metadata_populated
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return;
        }
        let mode = SessionLoadMode::from_env();
        let mut entries = match tokio::fs::read_dir(&self.dir).await {
            Ok(e) => e,
            Err(_) => return,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            if is_sidecar_filename(&path) {
                continue;
            }
            // Skip if already cached via an earlier `ensure_loaded`.
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok(id) = uuid::Uuid::parse_str(stem) else {
                continue;
            };
            if self.metadata.contains_key(&id) {
                continue;
            }
            // Permissive load is the right default — a corrupt file
            // doesn't gate the listing path.
            match read_session_with_mode(&path, mode).await {
                Ok(Some(s)) => {
                    self.metadata
                        .insert(s.id, SessionMetadata::from_session(&s));
                }
                Ok(None) | Err(_) => continue,
            }
        }
        self.metadata_populated
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Snapshot every session's listing-grade metadata without
    /// touching the disk after the first populate. UI listings
    /// (`/sessions/recent`, `/sessions?parent=`) that only need the
    /// projection use this rather than the full session payload.
    pub async fn iter_session_metadata(&self) -> Vec<SessionMetadata> {
        self.ensure_metadata_populated().await;
        self.metadata.iter().map(|e| e.value().clone()).collect()
    }

    /// Single-session metadata read. Populates the
    /// cache on miss via the same lazy disk fallback used by
    /// `ensure_loaded`. Returns `None` when the session is unknown or
    /// the disk file is corrupt.
    pub async fn get_metadata(&self, id: SessionId) -> Option<SessionMetadata> {
        if let Some(m) = self.metadata.get(&id) {
            return Some(m.value().clone());
        }
        // Fall through to a one-file disk read; ensure_loaded
        // populates the cache as a side effect.
        let _ = self.ensure_loaded(id).await?;
        self.metadata.get(&id).map(|m| m.value().clone())
    }

    /// Insert (or replace) a session during new-session creation or
    /// load-time seeding.
    ///
    /// Do not use this for existing-session mutations. Use [`Self::update`]
    /// instead, which honors the per-session Tokio mutex. A concurrent
    /// `save` against an `update` swaps the in-memory handle without
    /// acquiring that mutex, so one writer can silently lose the other's
    /// mutation.
    pub async fn save(&self, session: &Session) -> Result<()> {
        let tmp_path = self.dir.join(format!("{}.json.tmp", session.id));
        let final_path = self.dir.join(format!("{}.json", session.id));
        let bytes = serde_json::to_vec(session).context("serializing session")?;
        // Full crash-durable atomic-write pipeline:
        // 1. Write bytes to.tmp
        // 2. fsync the.tmp file (sync_data) so its data is on the
        // disk-firmware boundary before we rename
        // 3. rename.tmp → final
        // 4. fsync the parent directory so the rename itself is on
        // the disk-firmware boundary (otherwise a power loss
        // post-rename can lose the directory entry)
        //
        // Avoid the `tempfile::NamedTempFile::persist()` shortcut —
        // it's a known footgun: it doesn't fsync the parent dir on
        // most platforms, so the rename can still be lost on a power
        // failure that follows close on the heels of `persist`.
        // Same hazard the PostgreSQL "20-year fsync bug" lesson
        // applies to (S2.1).
        atomic_write_bytes(&tmp_path, &final_path, &bytes).await?;
        self.inner
            .insert(session.id, Arc::new(Mutex::new(session.clone())));
        // R2-N13 — keep the metadata cache coherent with the
        // newly-saved session. `save` is the new-session/load-time
        // seeding path; the metadata projection is cheap.
        self.metadata
            .insert(session.id, SessionMetadata::from_session(session));
        Ok(())
    }

    /// Load and return a clone of the session with `id`, or `None` if not found.
    pub async fn get(&self, id: SessionId) -> Option<Session> {
        let handle = self.ensure_loaded(id).await?;
        let locked = handle.lock().await;
        Some(locked.clone())
    }

    /// Snapshot every in-memory
    /// session — the SessionTree endpoint filters this by lineage.
    /// With per-session locking this is no longer a single-lock
    /// snapshot: sibling sessions can advance between the moment the
    /// outer read lock is released and the per-session clone. That
    /// divergence is acceptable for lineage rendering (SessionTree
    /// tolerates sibling drift) and reduces the outer-lock hold time
    /// to O(N) handle copies instead of O(N) deep Session clones.
    pub async fn iter_sessions(&self) -> Vec<Session> {
        // with lazy loading the in-memory map only holds the
        // sessions touched this process lifetime. Enumerate every
        // *.json on disk, ensure each is loaded, then clone under
        // its per-session lock. Expired files observed here are
        // removed by `ensure_loaded`.
        let mut ids: Vec<SessionId> = Vec::new();
        if let Ok(mut entries) = tokio::fs::read_dir(&self.dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) != Some("json") {
                    continue;
                }
                // Skip MetricsStore sidecars; only session payloads
                // belong on the lineage-render path.
                if is_sidecar_filename(&path) {
                    continue;
                }
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(id) = uuid::Uuid::parse_str(stem) {
                        ids.push(id);
                    }
                }
            }
        }
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(h) = self.ensure_loaded(id).await {
                out.push(h.lock().await.clone());
            }
        }
        out
    }

    /// Apply `f` to a mutable session, then persist it atomically.
    ///
    /// the prior coarse `RwLock<HashMap<_, Session>>`
    /// version released the outer read lock between read and save,
    /// which let two parallel progress-event handlers both read the
    /// same pre-transition snapshot and have the later writer
    /// overwrite the earlier writer's state change (the
    /// `block_from_harness` Emitted→Blocked transition got reverted
    /// This way when a concurrent task_completed event
    /// milliseconds later).
    ///
    /// The per-session Mutex preserves that same-session serialization
    /// while freeing concurrent turns on *different* sessions from
    /// each other's disk I/O.
    pub async fn update<F>(&self, id: SessionId, f: F) -> Result<Session>
    where
        F: FnOnce(&mut Session) -> Result<()>,
    {
        let handle: SessionHandle = self
            .ensure_loaded(id)
            .await
            .ok_or_else(|| anyhow!("no session '{}'", id))?;
        let mut locked = handle.lock().await;
        f(&mut locked)?;
        // Persist while holding the per-session lock so concurrent
        // updates on *this* session observe the new state on their
        // next read. Other sessions are free to proceed — they hold
        // their own Mutex.
        let tmp_path = self.dir.join(format!("{}.json.tmp", locked.id));
        let final_path = self.dir.join(format!("{}.json", locked.id));
        // Compact JSON (not pretty-printed): session files are server-internal;
        // pretty-print added ~30% bytes + serialization time per write.
        let bytes = serde_json::to_vec(&*locked).context("serializing session")?;
        // Durable atomic-write via the helper above.
        atomic_write_bytes(&tmp_path, &final_path, &bytes).await?;
        // R2-N13 — refresh the metadata cache so subsequent listing
        // calls observe the new `last_activity`, `state_kind`, and
        // turn count without re-deserializing the on-disk file.
        self.metadata
            .insert(locked.id, SessionMetadata::from_session(&locked));
        Ok(locked.clone())
    }

    /// Run an async closure `f` inside a held per-session lock,
    /// persisting changes atomically before releasing the lock.
    ///
    /// This is the canonical surface for every multi-step session
    /// mutation that must be serialized against concurrent writers
    /// without the read-snapshot/run/merge-back pattern that
    /// produced the OR-merge race in `send_turn.rs`.
    ///
    /// Contract: the per-session `tokio::sync::Mutex` is held from
    /// before `f` begins through after the persist completes. A
    /// concurrent `/confirm`, `/reject`, or `/branch` for the same
    /// session blocks on this lock and is naturally serialized —
    /// not lost to a delta-merge race. Concurrent transactions on
    /// *different* sessions proceed in parallel (DashMap shards the
    /// per-session handles).
    ///
    /// Seeds C2 (ConfirmationToken) and the C9 amendment-invalidation
    /// epoch in the same remediation plan — both require this
    /// transactional surface to operate atomically across a
    /// multi-step mutation.
    ///
    /// Usage:
    /// ```ignore
    /// store.transaction(session_id, |session| {
    ///     Box::pin(async move {
    ///         session.intake_methods.insert("alignment".into(), "STAR".into());
    ///         session.last_activity = chrono::Utc::now();
    ///         Ok(())
    ///     })
    /// }).await?;
    /// ```
    ///
    /// Caveat: an async `f` that performs long-running I/O (e.g. a
    /// 60s LLM call) holds the lock for that duration, blocking
    /// concurrent SME mutations on this session. That is the
    /// correct behavior — those mutations should serialize, not
    /// race. Callers whose work is read-only-then-write should
    /// continue to use `update` (sync closure) which has tighter
    /// hold semantics.
    ///
    /// Implementation note: the closure returns a
    /// `Pin<Box<dyn Future + Send + 'a>>` rather than an
    /// `impl Future` because the closure must work for *any*
    /// lifetime `'a` of the `&mut Session` borrow (HRTB), which the
    /// compiler can't infer through bare `impl Future` in stable
    /// 2021-edition Rust. The one box-allocation per transaction is
    /// dwarfed by the per-session disk write that follows.
    pub async fn transaction<F, T>(&self, id: SessionId, f: F) -> Result<T>
    where
        F: for<'a> FnOnce(
            &'a mut Session,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>,
        >,
    {
        let handle: SessionHandle = self
            .ensure_loaded(id)
            .await
            .ok_or_else(|| anyhow!("no session '{}'", id))?;
        let mut locked = handle.lock().await;
        // Set the task-local re-entrancy flag for the duration of `f`.
        // `ToolContext::with_store` asserts the inverse, so any code
        // path that tries to wire a fresh store handle into a tool
        // dispatched inside this body trips a debug_assert at the
        // wiring site instead of deadlocking on the per-session Mutex
        // below. `tokio::task_local!::scope` makes the flag visible
        // across every `.await` inside `f` on THIS task and invisible
        // to every other concurrently-running task — see the
        // `IN_TRANSACTION` docs above for why a thread-local would leak
        // cross-task false positives.
        let result = IN_TRANSACTION.scope(true, f(&mut locked)).await?;
        // Persist BEFORE releasing the lock — same invariant as
        // `update`, just with an async closure body. The
        // atomic-write helper writes through .tmp + fsync + rename
        // + parent-dir-fsync.
        let tmp_path = self.dir.join(format!("{}.json.tmp", locked.id));
        let final_path = self.dir.join(format!("{}.json", locked.id));
        let bytes = serde_json::to_vec(&*locked).context("serializing session")?;
        atomic_write_bytes(&tmp_path, &final_path, &bytes).await?;
        // R2-N13 — keep the listing-grade metadata cache coherent
        // with the just-persisted session shape.
        self.metadata
            .insert(locked.id, SessionMetadata::from_session(&locked));
        Ok(result)
    }
}

/// Load mode for session deserialization.
///
/// - **Strict**: corrupted bytes / unrecognised schema_version is a
///   hard error. Used in CI/test where silent skip would mask
///   regressions.
/// - **Permissive** (default in production): same upcast pipeline,
///   but a deserialize error after upcasting falls through to
///   `None` and a one-line `[session_load_corrupt]` stderr trace.
///   The caller treats `None` as "session not found"; a separate
///   pass appends a `BlockerKind::ReplayCorruption` chip to a
///   freshly-created session so the SME knows replay was lossy.
///
/// Selected via `ECAA_SESSION_LOAD_MODE=strict|permissive`; default
/// `permissive` so a single bad file doesn't hose the whole server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLoadMode {
    /// Any deserialize failure is a hard error. Used in CI and tests.
    Strict,
    /// Deserialize failures are logged but the session is replaced
    /// with a fresh default. Used in production so one corrupt file
    /// doesn't hose the whole server.
    Permissive,
}

impl SessionLoadMode {
    /// Resolve from `ECAA_SESSION_LOAD_MODE`. Unset / empty / unknown
    /// → Permissive (production safe default). `strict` flips on the
    /// hard-fail path used in CI + tests.
    pub fn from_env() -> Self {
        match std::env::var("ECAA_SESSION_LOAD_MODE")
            .ok()
            .as_deref()
            .map(str::trim)
        {
            Some("strict") => SessionLoadMode::Strict,
            _ => SessionLoadMode::Permissive,
        }
    }
}

/// Recurring TTL prune cadence. Default 3600s (1 hour); override via
/// `ECAA_TTL_PRUNE_INTERVAL_SECS`. Unset / unparseable / zero falls
/// back to the default with a `tracing::warn!`. The recurring sweep
/// reaps expired sessions during the server's lifetime, not just at
/// boot.
fn ttl_prune_interval_from_env() -> std::time::Duration {
    const DEFAULT_SECS: u64 = 3600;
    match std::env::var("ECAA_TTL_PRUNE_INTERVAL_SECS").ok() {
        Some(raw) => match raw.trim().parse::<u64>() {
            Ok(n) if n > 0 => std::time::Duration::from_secs(n),
            Ok(_) => {
                tracing::warn!(
                    "ECAA_TTL_PRUNE_INTERVAL_SECS=0 not allowed; falling back to {DEFAULT_SECS}s"
                );
                std::time::Duration::from_secs(DEFAULT_SECS)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    raw = %raw,
                    "ECAA_TTL_PRUNE_INTERVAL_SECS unparseable; falling back to {DEFAULT_SECS}s"
                );
                std::time::Duration::from_secs(DEFAULT_SECS)
            }
        },
        None => std::time::Duration::from_secs(DEFAULT_SECS),
    }
}

/// Upcast a v1 session JSON value to v2 in place.
///
/// Today's v1→v2 differences are forward-compat-by-serde-default:
/// `composer_version` and `pilot_recommendation` use serde defaults so
/// a v1 session deserializes cleanly without intervention. The
/// upcasting branch's job is to bump `schema_version` to 2 so writes
/// round-trip at the new version, leaving v2-specific structural
/// changes a single hook to edit when they arrive.
///
/// Returns Ok if the value is already v2+ (idempotent) or was a v1
/// successfully upcast. Errors only when the JSON shape is so
/// corrupt that `schema_version` itself can't be read — those flow
/// to permissive mode's skip-and-log branch.
pub fn upcast_session_v1_to_v2(value: &mut serde_json::Value) -> Result<()> {
    upcast_session_v1_to_v2_with_filename(value, None)
}

/// Variant of `upcast_session_v1_to_v2` that accepts an optional
/// filename hint to recover a missing `id` field.
///
/// Earlier server versions persisted sessions without a top-level
/// `id` field — the canonical id lived only in the filename. When
/// the deserialize hits a struct that now requires `id`, we
/// retroactively inject it from `<uuid>.json` (or the leading uuid
/// of `<uuid>.<suffix>.json`) so legacy on-disk packages stay
/// loadable. If the filename can't be parsed as a UUID the caller
/// flows to permissive-mode skip-with-log at `debug` level so the
/// boot log isn't spammed.
pub fn upcast_session_v1_to_v2_with_filename(
    value: &mut serde_json::Value,
    filename_hint: Option<&Path>,
) -> Result<()> {
    let obj = value
        .as_object_mut()
        .ok_or_else(|| anyhow!("session JSON root is not an object"))?;
    // v3 P7 — `schema_version` is now a SemVer field; on-disk shape
    // can still be a bare `u64` (legacy) or the canonical SemVer
    // string. Treat any major < 2 (or absent field) as v1 for
    // upcast purposes. The SemVer string form `"2.0.0"` upcasts to a
    // no-op; the legacy `1` upcasts to `"2.0.0"`.
    let major: u64 = match obj.get("schema_version") {
        Some(serde_json::Value::Number(n)) => n.as_u64().unwrap_or(1),
        Some(serde_json::Value::String(s)) => s
            .split('.')
            .next()
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(1),
        _ => 1,
    };

    // Backward-compat: inject `id` from filename when the on-disk
    // shape predates the field. Runs for both v1 and v2+ JSON
    // because the `id` field was added independently of the
    // schema_version bump rail. Only fills in when `id` is absent or
    // null — never overwrites a recorded value.
    let needs_id = matches!(obj.get("id"), None | Some(serde_json::Value::Null));
    if needs_id {
        if let Some(uuid_str) = filename_hint.and_then(uuid_prefix_of_path) {
            obj.insert("id".to_string(), serde_json::Value::String(uuid_str));
        }
    }

    if major >= 2 {
        return Ok(());
    }
    // V1 → V2 fixups (none structural today; this is the rail).
    // When a structural change arrives (e.g. SessionLineage shape
    // change, TurnContent enum split), do it here.
    obj.insert(
        "schema_version".to_string(),
        serde_json::Value::String("2.0.0".to_string()),
    );
    Ok(())
}

/// Return `true` when `path`'s basename matches the
/// `<session_id>.<suffix>.json` shape used by sidecars
/// (today: MetricsStore writes `<id>.metrics.json`). Conservative —
/// requires the inner segment to be a non-empty, non-`tmp` token so
/// we don't accidentally skip a `<id>.json.tmp` race window.
fn is_sidecar_filename(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    // Strip trailing `.json` (caller already filtered the extension).
    let Some(stem) = name.strip_suffix(".json") else {
        return false;
    };
    // A bare `<uuid>` stem has no inner `.`; a sidecar has at least
    // one. Examples:
    // "ff9caf3a-471f-43fb-ba04-01f5cc052254" → false (session)
    // "ff9caf3a-471f-43fb-ba04-01f5cc052254.metrics" → true (sidecar)
    let Some((id_part, suffix)) = stem.rsplit_once('.') else {
        return false;
    };
    if uuid::Uuid::parse_str(id_part).is_err() {
        return false;
    }
    !suffix.is_empty() && suffix != "tmp"
}

/// Extract the leading UUID from a filename like `<uuid>.json` or
/// `<uuid>.<suffix>.json`. Returns the canonical hyphenated string
/// when the leading token parses as a UUID, otherwise `None`.
fn uuid_prefix_of_path(path: &Path) -> Option<String> {
    let name = path.file_name().and_then(|s| s.to_str())?;
    let leading = name.split('.').next()?;
    let parsed = uuid::Uuid::parse_str(leading).ok()?;
    Some(parsed.to_string())
}

/// Reconcile a freshly-loaded session's `task_states` against the
/// on-disk `WORKFLOW.json` of its emitted package. Returns the set of
/// `(task_id, new_state)` pairs the caller should apply via
/// `Session::set_task_state` (which preserves the monotonicity
/// invariants).
///
/// Returns `None` when no emitted package is set or the file cannot be
/// read/parsed — the session keeps its current `task_states` and the
/// caller falls back to the pre-existing in-memory snapshot.
///
/// Returns `Some(vec![])` (or an empty vec) when reconciliation found
/// no drift. The caller skips the backfill write in that case.
///
/// Why this exists: during a server restart, the harness keeps writing
/// task transitions to its local `WORKFLOW.json` even when its POSTs
/// to `/task/<id>/state` return 404. After the server comes back,
/// `session.task_states` (loaded from the session JSON file) lags the
/// on-disk DAG truth. Without this catch-up, every `current_dag()`
/// consumer (dashboard, summary, execution status, task results,
/// impact) overlays the stale snapshot.
async fn reconcile_task_states_from_workflow_json(
    session: &Session,
) -> Option<Vec<(String, ecaa_workflow_core::dag::TaskState)>> {
    let package_path = session.emitted_package_path.as_ref()?.clone();
    let task_states_snapshot = session.task_states.clone();
    tokio::task::spawn_blocking(move || {
        reconcile_task_states_sync(&package_path, &task_states_snapshot)
    })
    .await
    .ok()
    .flatten()
}

/// Synchronous worker for `reconcile_task_states_from_workflow_json`.
/// Reads `<package_path>/WORKFLOW.json`, parses the `tasks` map, and
/// returns the set of task ids whose on-disk state strictly differs
/// from the in-memory snapshot.
///
/// The match is structural (full `TaskState` deserialize): a transition
/// from `Running { remote: None, ... }` to `Running { remote: Some, ...
/// }` counts as drift and gets applied. The caller's
/// `Session::set_task_state` enforces monotonicity (terminal →
/// non-terminal is refused), so an unrelated DAG that briefly went
/// non-terminal can't downgrade a Completed task.
fn reconcile_task_states_sync(
    package_path: &Path,
    in_memory: &std::collections::BTreeMap<String, ecaa_workflow_core::dag::TaskState>,
) -> Option<Vec<(String, ecaa_workflow_core::dag::TaskState)>> {
    let workflow_json = package_path.join("WORKFLOW.json");
    let content = std::fs::read_to_string(&workflow_json).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    let tasks = value.get("tasks").and_then(|t| t.as_object())?;
    let mut updates = Vec::new();
    for (id, task) in tasks.iter() {
        let Some(state_json) = task.get("state") else {
            continue;
        };
        let on_disk: ecaa_workflow_core::dag::TaskState =
            match serde_json::from_value(state_json.clone()) {
                Ok(s) => s,
                Err(_) => continue,
            };
        // Skip the noisy default — a task that's `Pending` on disk
        // doesn't override an in-memory `Pending` (or absent) state.
        if matches!(on_disk, ecaa_workflow_core::dag::TaskState::Pending) {
            continue;
        }
        match in_memory.get(id) {
            Some(existing) if existing == &on_disk => continue,
            _ => updates.push((id.clone(), on_disk)),
        }
    }
    Some(updates)
}

async fn read_session(path: &Path) -> Result<Session> {
    let bytes = tokio::fs::read(path).await?;
    let mut value: serde_json::Value =
        serde_json::from_slice(&bytes).context("parsing session JSON")?;
    // Pass the path so the upcast can recover a missing top-level
    // `id` from the filename's UUID prefix (legacy sessions
    // predating the explicit `id` field).
    upcast_session_v1_to_v2_with_filename(&mut value, Some(path))
        .context("upcasting session schema")?;
    let session: Session = serde_json::from_value(value).context("deserializing upcast session")?;
    Ok(session)
}

/// `read_session` variant that respects load mode.
///
/// Strict mode propagates errors to the caller (test/CI fail loud).
/// Permissive mode logs a one-line `[session_load_corrupt]` trace
/// and returns `Ok(None)` so the caller can fall back to "session
/// not found" semantics. Either way the call site is responsible
/// for surfacing the corruption to a future session via
/// `BlockerKind::ReplayCorruption`.
async fn read_session_with_mode(path: &Path, mode: SessionLoadMode) -> Result<Option<Session>> {
    match read_session(path).await {
        Ok(s) => Ok(Some(s)),
        Err(e) if mode == SessionLoadMode::Permissive => {
            // Downgrade the missing-`id` + non-UUID-filename case to
            // a debug-level trace. Earlier server versions wrote
            // sessions without a top-level `id`, and a non-UUID
            // filename rules out the filename-hint recovery — that's
            // the spammy boot-time pattern. Other deserialize errors
            // (legitimately corrupt or schema-mismatched payloads)
            // keep the louder `eprintln!` so an operator notices.
            if is_missing_id_error(&e) && uuid_prefix_of_path(path).is_none() {
                tracing::debug!(
                    target: "session_load_corrupt",
                    path = %path.display(),
                    err = %format!("{:#}", e),
                    "skipping session with missing id and non-uuid filename",
                );
            } else {
                eprintln!("[session_load_corrupt] path={} err={:#}", path.display(), e);
            }
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Detect the `missing field \`id\`` deserialize error chain so the
/// permissive load path can decide between debug-level skip
/// (backward-compat shape) and eprintln-level skip (real
/// corruption).
fn is_missing_id_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| e.to_string().contains("missing field `id`"))
}

fn is_expired(last_activity: &DateTime<Utc>, cutoff: DateTime<Utc>) -> bool {
    *last_activity < cutoff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, Turn};

    #[test]
    fn upcast_v1_session_bumps_schema_version_to_2() {
        // V1 session (schema_version absent → defaults to 1). v3 P7
        // rewrites the legacy bare-`u64` into a canonical SemVer
        // string `"2.0.0"`.
        let mut value = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "schema_version": 1,
            "intake_prose": "test"
        });
        upcast_session_v1_to_v2(&mut value).expect("upcast must succeed");
        assert_eq!(value["schema_version"], serde_json::json!("2.0.0"));
    }

    #[test]
    fn upcast_v2_session_is_idempotent() {
        let mut value = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "schema_version": "2.0.0",
            "intake_prose": "test"
        });
        let original = value.clone();
        upcast_session_v1_to_v2(&mut value).expect("upcast must be a no-op");
        assert_eq!(value, original);
    }

    #[test]
    fn upcast_session_with_no_schema_field_treats_as_v1() {
        // A pre-S4.6 session without schema_version was implicitly v1.
        let mut value = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "intake_prose": "test"
        });
        upcast_session_v1_to_v2(&mut value).expect("missing field defaults to 1");
        assert_eq!(value["schema_version"], serde_json::json!("2.0.0"));
    }

    /// v3 P7 — `upcast_session_v1_to_v2` accepts legacy `u64`
    /// `schema_version`, but a SemVer-shape v2 session that came in
    /// from a new emit must also be left alone (idempotent across
    /// shape changes).
    #[test]
    fn upcast_v2_semver_string_session_is_idempotent() {
        let mut value = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000004",
            "schema_version": "2.1.0",
            "intake_prose": "test"
        });
        let original = value.clone();
        upcast_session_v1_to_v2(&mut value).expect("upcast must be a no-op");
        assert_eq!(value, original);
    }

    #[test]
    fn upcast_rejects_non_object_root() {
        let mut value = serde_json::json!([1, 2, 3]);
        let err = upcast_session_v1_to_v2(&mut value).unwrap_err();
        assert!(err.to_string().contains("not an object"));
    }

    #[test]
    fn session_load_mode_defaults_permissive() {
        std::env::remove_var("ECAA_SESSION_LOAD_MODE");
        assert_eq!(SessionLoadMode::from_env(), SessionLoadMode::Permissive);
    }

    #[test]
    fn session_load_mode_strict_via_env() {
        std::env::set_var("ECAA_SESSION_LOAD_MODE", "strict");
        assert_eq!(SessionLoadMode::from_env(), SessionLoadMode::Strict);
        std::env::remove_var("ECAA_SESSION_LOAD_MODE");
    }

    #[tokio::test]
    async fn read_session_upcasts_v1_to_v2() {
        // Persist a hand-crafted v1 session JSON and load it; the
        // returned Session should report schema_version 2.
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let path = dir.path().join(format!("{}.json", id));
        // Create a real v2 session, save it, then mutate the persisted
        // JSON to v1 (the real path: a session that was persisted by
        // an older binary). On reload the upcasting branch promotes it.
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        s.id = id;
        store.save(&s).await.unwrap();

        // Mutate file in place to schema_version=1 (mimicking a v1
        // persistence). serde_json round-trip preserves order so the
        // diff is stable.
        let bytes = tokio::fs::read(&path).await.unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("schema_version".to_string(), serde_json::json!(1));
        tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();

        // Open a fresh store so the in-memory cache is clean.
        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2.get(id).await.expect("upcast must succeed");
        // v3 P7 — `schema_version` is now `semver::Version`; the
        // upcast rewrites `1u32` → `"2.0.0"` and the struct
        // deserializes it as SemVer `(2,0,0)`.
        assert_eq!(loaded.schema_version, semver::Version::new(2, 0, 0));
    }

    #[tokio::test]
    async fn read_session_with_mode_permissive_skips_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        tokio::fs::write(&path, b"not json").await.unwrap();
        let result = read_session_with_mode(&path, SessionLoadMode::Permissive).await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn read_session_with_mode_strict_propagates_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("corrupt.json");
        tokio::fs::write(&path, b"not json").await.unwrap();
        let err = read_session_with_mode(&path, SessionLoadMode::Strict)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("parsing session JSON"));
    }

    #[tokio::test]
    async fn roundtrip_save_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        Arc::make_mut(&mut s.conversation).push(Turn::user("hello world"));
        store.save(&s).await.unwrap();

        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2.get(s.id).await.unwrap();
        assert_eq!(loaded.id, s.id);
        assert_eq!(loaded.conversation.len(), 1);
        assert_eq!(loaded.conversation[0].content, "hello world");
    }

    #[tokio::test]
    async fn atomic_save_no_partial_file_visible() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();

        // No leftover.tmp file
        let mut entries = tokio::fs::read_dir(dir.path()).await.unwrap();
        let mut tmp_count = 0;
        while let Some(e) = entries.next_entry().await.unwrap() {
            if e.path().extension().and_then(|s| s.to_str()) == Some("tmp") {
                tmp_count += 1;
            }
        }
        assert_eq!(tmp_count, 0);
    }

    #[tokio::test]
    async fn expired_session_is_pruned_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        // Make it 31 days old
        s.last_activity = Utc::now() - Duration::days(31);
        store.save(&s).await.unwrap();

        // open() now kicks prune into the background, so
        // tests that need deterministic observation await an
        // explicit `prune_expired_now()`.
        let store2 = SessionStore::open(dir.path()).await.unwrap();
        store2.prune_expired_now().await.unwrap();
        let loaded = store2.get(s.id).await;
        assert!(loaded.is_none());
    }

    #[tokio::test]
    async fn lazy_load_recovers_sessions_from_disk_across_restarts() {
        // Regression guard: a session written by one store instance
        // must be retrievable via a fresh store pointed at the same
        // dir, even though the fresh store's in-memory map starts
        // empty.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        Arc::make_mut(&mut s.conversation).push(Turn::user("recovered"));
        store.save(&s).await.unwrap();
        let id = s.id;

        // Simulate a server restart: drop the first store, open a
        // fresh one. With lazy loading, `get` should read from disk
        // on first access and cache the result.
        drop(store);
        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2.get(id).await.unwrap();
        assert_eq!(loaded.conversation.len(), 1);
        assert_eq!(loaded.conversation[0].content, "recovered");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_on_different_sessions_do_not_serialize() {
        // Distinct sessions should enter their update closures concurrently.
        // The old coarse `RwLock<HashMap<_, Session>>` forced closures to run
        // one-at-a-time. Avoid wall-clock fsync ratios here: under a full
        // workspace test run, parallel durable writes can be slower than
        // sequential writes even when locking is correct.
        use std::sync::{Condvar, Mutex as StdMutex};
        use std::time::{Duration as StdDuration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();

        let n = 4usize;
        let mut ids = Vec::with_capacity(n);
        for _ in 0..n {
            let s = Session::new(false);
            store.save(&s).await.unwrap();
            ids.push(s.id);
        }

        let gate = Arc::new((StdMutex::new(0usize), Condvar::new()));
        let mut handles = Vec::with_capacity(n);
        for id in &ids {
            let store = store.clone();
            let id = *id;
            let gate = gate.clone();
            handles.push(tokio::spawn(async move {
                store
                    .update(id, |s| {
                        let (count_lock, cvar) = &*gate;
                        let mut entered = count_lock.lock().unwrap();
                        *entered += 1;
                        cvar.notify_all();
                        let deadline = Instant::now() + StdDuration::from_secs(2);
                        while *entered < n {
                            let now = Instant::now();
                            if now >= deadline {
                                anyhow::bail!(
                                    "only {}/{} update closures entered before timeout; \
                                     updates appear serialized across different sessions",
                                    *entered,
                                    n
                                );
                            }
                            let remaining = deadline.saturating_duration_since(now);
                            let (next, timeout) = cvar.wait_timeout(entered, remaining).unwrap();
                            entered = next;
                            if timeout.timed_out() && *entered < n {
                                anyhow::bail!(
                                    "only {}/{} update closures entered before timeout; \
                                     updates appear serialized across different sessions",
                                    *entered,
                                    n
                                );
                            }
                        }
                        drop(entered);
                        Arc::make_mut(&mut s.conversation).push(Turn::user("parallel"));
                        Ok(())
                    })
                    .await
                    .unwrap();
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_updates_on_same_session_serialize() {
        // Regression guard: the per-session Mutex must preserve the
        // same-session lost-update fix. Two parallel updates that both
        // push a Turn should produce 2 turns, not 1.
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();
        let id = s.id;

        let s1 = store.clone();
        let s2 = store.clone();
        let h1 = tokio::spawn(async move {
            s1.update(id, |s| {
                Arc::make_mut(&mut s.conversation).push(Turn::user("a"));
                Ok(())
            })
            .await
            .unwrap();
        });
        let h2 = tokio::spawn(async move {
            s2.update(id, |s| {
                Arc::make_mut(&mut s.conversation).push(Turn::user("b"));
                Ok(())
            })
            .await
            .unwrap();
        });
        h1.await.unwrap();
        h2.await.unwrap();

        let loaded = store.get(id).await.unwrap();
        assert_eq!(
            loaded.conversation.len(),
            2,
            "both updates must land — same-session serialization is load-bearing"
        );
    }

    /// Backward-compat: a v1 session file written before the
    /// top-level `id` field existed should still load — the loader
    /// recovers the id from the filename's UUID prefix.
    #[tokio::test]
    async fn read_session_recovers_missing_id_from_filename() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let path = dir.path().join(format!("{}.json", id));

        // Persist a real session, then strip the `id` field to mimic
        // the legacy on-disk shape. Serializing through `Session`
        // first guarantees every *other* required field is present
        // so the only deserialize failure path is the missing `id`.
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        s.id = id;
        store.save(&s).await.unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value.as_object_mut().unwrap().remove("id");
        tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();

        // Fresh store so the in-memory cache misses and the load
        // path actually exercises the upcast filename hook.
        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2
            .get(id)
            .await
            .expect("missing id must be recoverable from filename");
        assert_eq!(loaded.id, id);
    }

    /// Files with truly missing required fields (other than `id`)
    /// remain skipped under permissive mode — the loader still
    /// returns `Ok(None)` rather than silently fabricating data.
    #[tokio::test]
    async fn read_session_with_mode_skips_other_missing_fields() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let path = dir.path().join(format!("{}.json", id));
        // A nominally-shaped session JSON with `id` present but
        // every other required field missing.
        let payload = serde_json::json!({
            "id": id.to_string(),
            "schema_version": "2.0.0"
        });
        tokio::fs::write(&path, serde_json::to_vec(&payload).unwrap())
            .await
            .unwrap();
        let result = read_session_with_mode(&path, SessionLoadMode::Permissive)
            .await
            .unwrap();
        assert!(
            result.is_none(),
            "files missing fields other than `id` must continue to skip"
        );
    }

    /// Filename without a UUID prefix and JSON missing `id`: skip,
    /// but at debug level (verified separately by inspecting log
    /// helpers). The user-visible contract is that the loader still
    /// returns `Ok(None)`; no panic, no error propagation.
    #[tokio::test]
    async fn read_session_with_mode_skips_missing_id_non_uuid_filename() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-a-uuid.json");
        let payload = serde_json::json!({
            "schema_version": "2.0.0",
            "intake_prose": "test"
        });
        tokio::fs::write(&path, serde_json::to_vec(&payload).unwrap())
            .await
            .unwrap();
        let result = read_session_with_mode(&path, SessionLoadMode::Permissive)
            .await
            .unwrap();
        assert!(result.is_none(), "non-uuid filename + missing id must skip");
        // And the helper that gates the debug-vs-eprintln branch
        // agrees it's a missing-id error with no filename UUID.
        let err = read_session(&path).await.unwrap_err();
        assert!(is_missing_id_error(&err));
        assert!(uuid_prefix_of_path(&path).is_none());
    }

    /// MetricsStore sidecars (`<uuid>.metrics.json`) are filtered
    /// out of the prune/iter sweeps before the loader sees them —
    /// they aren't sessions and previously produced the
    /// `[session_load_corrupt]` boot-time spam.
    #[tokio::test]
    async fn metrics_sidecar_is_not_treated_as_session() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();
        // Drop a fake metrics sidecar next to it; missing `id` would
        // otherwise be flagged as corruption.
        let metrics_path = dir.path().join(format!("{}.metrics.json", s.id));
        tokio::fs::write(&metrics_path, b"{\"turn_count\": 0}")
            .await
            .unwrap();
        // The sidecar filter must classify the metrics file as
        // not-a-session, and the session payload itself must remain
        // visible to iter_sessions.
        assert!(is_sidecar_filename(&metrics_path));
        let session_path = dir.path().join(format!("{}.json", s.id));
        assert!(!is_sidecar_filename(&session_path));
        let listed = store.iter_sessions().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, s.id);
        // Prune sweep also tolerates the sidecar without erroring.
        store.prune_expired_now().await.unwrap();
    }

    #[test]
    fn uuid_prefix_of_path_recognises_uuid_and_sidecar_shapes() {
        let id = "ff9caf3a-471f-43fb-ba04-01f5cc052254";
        assert_eq!(
            uuid_prefix_of_path(Path::new(&format!("/tmp/{id}.json"))),
            Some(id.to_string())
        );
        assert_eq!(
            uuid_prefix_of_path(Path::new(&format!("/tmp/{id}.metrics.json"))),
            Some(id.to_string())
        );
        assert_eq!(uuid_prefix_of_path(Path::new("/tmp/not-a-uuid.json")), None);
    }

    #[test]
    fn upcast_injects_id_from_filename_when_missing() {
        let id = uuid::Uuid::new_v4();
        let mut value = serde_json::json!({
            "schema_version": "2.0.0",
            "intake_prose": "test"
        });
        let path = std::path::PathBuf::from(format!("/tmp/{id}.json"));
        upcast_session_v1_to_v2_with_filename(&mut value, Some(&path)).unwrap();
        assert_eq!(value["id"], serde_json::Value::String(id.to_string()));
    }

    #[test]
    fn upcast_leaves_existing_id_alone() {
        let recorded = "00000000-0000-0000-0000-000000000abc";
        let mut value = serde_json::json!({
            "id": recorded,
            "schema_version": "2.0.0"
        });
        let filename_id = uuid::Uuid::new_v4();
        let path = std::path::PathBuf::from(format!("/tmp/{filename_id}.json"));
        upcast_session_v1_to_v2_with_filename(&mut value, Some(&path)).unwrap();
        assert_eq!(value["id"], serde_json::Value::String(recorded.to_string()));
    }

    #[tokio::test]
    async fn update_persists_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();
        let id = s.id;

        store
            .update(id, |s| {
                Arc::make_mut(&mut s.conversation).push(Turn::user("from update"));
                Ok(())
            })
            .await
            .unwrap();

        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2.get(id).await.unwrap();
        assert_eq!(loaded.conversation.len(), 1);
    }

    /// R2-N13 — `save` populates the in-memory metadata cache so
    /// subsequent `iter_session_metadata` and `get_metadata` calls
    /// return without re-reading the disk file.
    #[tokio::test]
    async fn save_populates_metadata_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();

        // get_metadata hits the cache (cache was populated by save).
        let meta = store.get_metadata(s.id).await.expect("cached metadata");
        assert_eq!(meta.id, s.id);
        assert_eq!(meta.owner_user, s.owner_user);
        assert_eq!(meta.state_kind, "greeting");
        assert_eq!(meta.n_turns, 0);
    }

    /// R2-N13 — `update` keeps the cache coherent with mutations
    /// (turn count, last_activity, state_kind).
    #[tokio::test]
    async fn update_refreshes_metadata_cache() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        store.save(&s).await.unwrap();
        let id = s.id;

        store
            .update(id, |s| {
                Arc::make_mut(&mut s.conversation).push(Turn::user("hi"));
                Arc::make_mut(&mut s.conversation).push(Turn::user("there"));
                Ok(())
            })
            .await
            .unwrap();

        let meta = store.get_metadata(id).await.unwrap();
        assert_eq!(meta.n_turns, 2);
    }

    /// R2-N13 — `iter_session_metadata` lazy-populates the cache
    /// from disk on first call and returns cached entries on
    /// subsequent calls without re-reading.
    #[tokio::test]
    async fn iter_session_metadata_lazily_populates_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        // Seed three sessions through one store, then drop it; a
        // fresh store starts with an empty metadata cache.
        {
            let store = SessionStore::open(dir.path()).await.unwrap();
            for _ in 0..3 {
                let s = Session::new(false);
                store.save(&s).await.unwrap();
            }
        }
        let store = SessionStore::open(dir.path()).await.unwrap();
        let metas = store.iter_session_metadata().await;
        assert_eq!(metas.len(), 3);
        // Second call returns the same set without re-reading the
        // disk (cache is populated). Functionally indistinguishable
        // from the first; we assert count only.
        let metas2 = store.iter_session_metadata().await;
        assert_eq!(metas2.len(), 3);
    }

    /// E24 — `audit_writer_secret` survives a save/load roundtrip and
    /// is unique per session.
    #[tokio::test]
    async fn audit_writer_secret_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let s = Session::new(false);
        let original_secret = s.audit_writer_secret;
        // Fresh secret is non-zero (OsRng is not deterministically zero).
        assert_ne!(original_secret, [0u8; 32], "fresh secret must not be zero");
        store.save(&s).await.unwrap();
        let loaded = store.get(s.id).await.unwrap();
        assert_eq!(
            loaded.audit_writer_secret, original_secret,
            "secret must survive save/load roundtrip"
        );
    }

    /// E24 — two sessions created in the same process get distinct
    /// secrets (OsRng entropy; collision probability negligible).
    #[tokio::test]
    async fn audit_writer_secret_unique_across_sessions() {
        let s1 = Session::new(false);
        let s2 = Session::new(false);
        assert_ne!(
            s1.audit_writer_secret, s2.audit_writer_secret,
            "two sessions must get distinct secrets"
        );
    }

    /// E24 — loading a legacy session JSON without `audit_writer_secret`
    /// generates a non-zero secret and backfills the on-disk file.
    #[tokio::test]
    async fn audit_writer_secret_migrated_from_legacy_session() {
        let dir = tempfile::tempdir().unwrap();
        let id = uuid::Uuid::new_v4();
        let path = dir.path().join(format!("{}.json", id));
        // Write a minimal session JSON without the `audit_writer_secret` field.
        // Required fields without #[serde(default)]: id, created_at, last_activity,
        // state, conversation, intake_prose. All others are optional / defaulted.
        let now = chrono::Utc::now().to_rfc3339();
        let legacy_json = serde_json::json!({
            "id": id,
            "schema_version": "2.0.0",
            "created_at": now,
            "last_activity": now,
            "state": {"kind": "greeting"},
            "conversation": [],
            "intake_prose": "",
        });
        tokio::fs::write(&path, serde_json::to_vec(&legacy_json).unwrap())
            .await
            .unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store.get(id).await.unwrap();
        assert_ne!(
            loaded.audit_writer_secret, [0u8; 32],
            "migrated legacy session must get a non-zero secret"
        );
        // The secret must also have been written back to disk.
        let bytes = tokio::fs::read(&path).await.unwrap();
        let disk: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let disk_secret = disk["audit_writer_secret"].as_str().expect("hex string");
        assert_eq!(
            disk_secret.len(),
            64,
            "hex-encoded 32-byte secret is 64 chars"
        );
        assert_ne!(
            disk_secret,
            "0".repeat(64),
            "backfilled secret must be non-zero"
        );
    }

    /// R2-N13 — `prune_expired_now` removes pruned sessions from the
    /// metadata cache so listing endpoints never return zombie
    /// entries.
    #[tokio::test]
    async fn prune_clears_metadata_cache_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let mut s = Session::new(false);
        s.last_activity = Utc::now() - Duration::days(31);
        store.save(&s).await.unwrap();
        let id = s.id;
        // Cache is populated by save.
        assert!(store.get_metadata(id).await.is_some());
        store.prune_expired_now().await.unwrap();
        assert!(
            store.get_metadata(id).await.is_none(),
            "expired session must be evicted from the metadata cache"
        );
    }

    /// D9 regression: an already-emitted session reloaded from disk
    /// after a server restart must still report `is_confirmed()=true`
    /// so the LLM-facing `get_session_state` projection (which reads
    /// `session.is_confirmed()`) does not prompt the SME to re-click
    /// Confirm on a package that has already been written. Without
    /// the Emitted+package_path short-circuit on `is_confirmed()`,
    /// the consumed token (post-emit_package_post_ok) makes
    /// `ConfirmationToken::authorizes` return false, and the LLM
    /// produces the buggy "could you click the Confirm button again?"
    /// turn observed in the live session.
    #[tokio::test]
    async fn d9_emitted_session_reload_preserves_is_confirmed() {
        use crate::session::{SessionState, StateTrigger};
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();

        // Build a session that traverses the full intake → confirm →
        // emit happy-path; mint a token bound to a real
        // pending_emission_id and then consume it (mirroring what
        // emit_package_post_ok does on a successful emit).
        let mut s = Session::new(false);
        s.try_transition(StateTrigger::AppendProse).unwrap();
        s.try_transition(StateTrigger::ProposeSummaryConfirmation)
            .unwrap();
        s.pending_emission_id = Some(uuid::Uuid::new_v4());
        let _ = s.mint_confirmation_token(
            chrono::Utc::now(),
            crate::audit_actor::AuditActor::User("sme".into()),
        );
        s.try_transition(StateTrigger::UserClickedConfirm).unwrap();
        s.try_transition(StateTrigger::EmitPackageStart).unwrap();
        s.try_transition(StateTrigger::EmitPackageOk).unwrap();
        if let Some(t) = s.confirmation_token.as_mut() {
            t.consume();
        }
        s.emitted_package_path = Some(std::path::PathBuf::from(
            dir.path().join("scripps-pkg-deadbeef"),
        ));
        let id = s.id;
        store.save(&s).await.unwrap();

        // Simulate the server restart: drop the first store and open a
        // fresh one against the same on-disk session directory. The
        // fresh store's in-memory map starts empty so `get` exercises
        // the full disk → deserialize → ensure_loaded path.
        drop(store);
        let store2 = SessionStore::open(dir.path()).await.unwrap();
        let loaded = store2.get(id).await.expect("session must reload");

        // Post-emit invariants survived the round trip.
        assert_eq!(loaded.state, SessionState::Emitted);
        assert!(loaded.emitted_package_path.is_some());
        // The per-emit token is consumed (its single-use latch wasn't
        // re-armed by load), but `is_confirmed()` short-circuits to
        // true because the durable RO-Crate IS the confirmation
        // artifact for an Emitted session.
        assert!(
            loaded
                .confirmation_token
                .as_ref()
                .is_some_and(|t| t.is_consumed()),
            "post-emit consumed token must survive the save/load round trip"
        );
        assert!(
            loaded.is_confirmed(),
            "D9: an Emitted session reloaded after a server restart must \
             still report is_confirmed()=true so the LLM does not prompt \
             the SME to re-confirm an already-emitted package"
        );
    }

    #[tokio::test]
    async fn save_recreates_sessions_dir_when_externally_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let store = SessionStore::open(dir.path()).await.unwrap();
        let session = Session::new(false);

        // Normal save writes to disk.
        store
            .save(&session)
            .await
            .expect("initial save must succeed");

        // External tooling deletes the sessions directory between calls —
        // mirrors the failure mode where a cleanup script `rm -rf`s the
        // sessions dir between two server-side saves.
        std::fs::remove_dir_all(dir.path()).expect("teardown");
        assert!(!dir.path().exists(), "precondition: sessions dir removed");

        // After external removal, save must transparently recreate
        // the directory and succeed instead of bubbling an ENOENT.
        let session2 = Session::new(false);
        store
            .save(&session2)
            .await
            .expect("save must recreate the directory after external rm -rf");
        assert!(dir.path().exists(), "sessions dir was recreated");
        let final_path = dir.path().join(format!("{}.json", session2.id));
        assert!(
            final_path.exists(),
            "session file must land on disk after dir recreate"
        );
    }
}
