//! Standalone end-of-run package finalization.
//!
//! The server finalizes per-task on each `task_completed` event
//! (`core::finalize::finalize_task` via `verification::reverify_and_block_on_mismatch`).
//! A standalone harness run (`--no-interactive`, no `--session-id`) sends no
//! session events, so nothing finalizes the package: the signed verdict sink is
//! never written, the plaintext `runtime/claim-verification.json` stays an empty
//! emit-time stub, evidence is unregistered, and the at-rest audit-proof falls
//! back to the vacuous emit stub.
//!
//! This module sources the finalize inputs from the SELF-CONTAINED emitted
//! package (plus a host-resolved `config_dir` for the base interpretation
//! policy + extractor config, and `ECAA_AUDIT_SECRET` for the HMAC sink) and
//! calls [`ecaa_workflow_core::finalize::finalize_package`] once at the end of a
//! completed run. It is intentionally a thin, testable shim around the core
//! orchestration: no session, no HTTP, no state transition.
//!
//! Failure is NON-FATAL — every error path here logs and returns so the caller
//! can still truncate the WAL and exit `Ok(())`. Finalizing a package is a
//! best-effort post-exec convenience, never a gate.

use crate::env_snapshot::{self, SnapshotOpts, SnapshotOutcome};
use crate::env_snapshot::cache_scan::resolve_cache_dir;
use crate::env_snapshot::record::record_digest;
use ecaa_workflow_core::decision_log::DecisionRecord;
use ecaa_workflow_core::finalize::{
    coverage_should_block, finalize_package, finalize_task, PackageFinalizeSummary,
};
use ecaa_workflow_core::project_class::ProjectClass;
use std::path::{Path, PathBuf};

/// Env var that turns the offline end-of-run repair loop ON. Default OFF.
///
/// When truthy, [`run_auto_repair_best_effort`] runs the OFFLINE repair loop
/// once at the harness loop-exit convergence point — on BOTH the standalone/CLI
/// run and the session/web-UI run (the server spawns this harness as the
/// execution engine on both). It applies deterministic prose/manifest repairs
/// (e.g. prose-vs-table counts re-synced, BagIt manifests re-sealed) and routes
/// any agentic / offline-unverifiable gap to the signed review list
/// (`runtime/repair-status.json` + `runtime/repair-requests.jsonl`). It NEVER
/// re-executes an agent at end-of-run — agentic auto-repair stays the manual
/// `ecaa-workflow repair --agent` path.
pub const ENV_AUTO_REPAIR: &str = "ECAA_AUTO_REPAIR";

/// Whether the offline end-of-run repair loop is enabled. Default OFF — only the
/// canonical truthy table (`1` / `true` / `yes` / `on` / `t` / `y`,
/// case-insensitive, trimmed; identical to
/// [`crate::validation_recovery::recovery_enabled`] and `core::config`'s
/// `parse_bool`) enables it, so a typo never silently runs a post-finalize
/// repair pass.
pub fn auto_repair_enabled() -> bool {
    matches!(
        std::env::var(ENV_AUTO_REPAIR)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1") | Some("true") | Some("yes") | Some("on") | Some("t") | Some("y")
    )
}

/// Stage stems that mark a run as confirmatory (DE / differential-accessibility
/// / variant-calling / a clinical primary-endpoint). A package whose DAG names
/// any of these is treated as confirmatory for the finalize call. Pre-Task-5
/// `decisions` is empty, so `is_confirmatory` is provably inert today (it only
/// gates `demote_claims_from_deviations`, which needs `PostHocDeviation`
/// records); the heuristic is implemented so it is correct once Task 5 starts
/// populating `runtime/decisions.jsonl`.
const CONFIRMATORY_STAGE_STEMS: &[&str] = &[
    "differential_expression",
    "differential_accessibility",
    "variant_calling",
    "primary_endpoint",
];

/// Derive the 32-byte HMAC key from `ECAA_AUDIT_SECRET`, byte-identically to
/// how `ecaa-workflow-audit-proof` derives its `--secret`/`ECAA_AUDIT_SECRET`
/// key (`crates/cli/src/bin/ecaa-workflow-audit-proof.rs::writer_from_hex`):
/// hex-decode the trimmed string and require EXACTLY 32 bytes (64 hex chars).
///
/// Matching that derivation is load-bearing: Task 9 later runs
/// `ecaa-workflow-audit-proof --secret "$ECAA_AUDIT_SECRET"` to VERIFY the HMAC
/// this harness wrote. A loose derivation (e.g. raw UTF-8 bytes on hex failure)
/// would sign with a key the verifier can never reproduce.
///
/// Returns `None` when the env var is unset/empty, OR when it is not a valid
/// 64-hex-char string — in the latter case the harness logs a warning and skips
/// the signed sink rather than signing with a key the audit-proof tool would
/// reject. (Passing `None` to `finalize_package` leaves audit-proof Inv 1/5
/// Unverified, which is the documented degraded mode.)
pub fn audit_secret_from_env() -> Option<[u8; 32]> {
    let raw = std::env::var("ECAA_AUDIT_SECRET").ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    match hex::decode(trimmed) {
        Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
            Ok(key) => Some(key),
            Err(_) => {
                tracing::warn!(
                    target: "harness-finalize",
                    got_bytes = bytes.len(),
                    "ECAA_AUDIT_SECRET decoded to {} byte(s), not 32 (64 hex chars) — \
                     signed verdict sink not written; set a 64-hex-char secret to match \
                     ecaa-workflow-audit-proof --secret",
                    bytes.len()
                );
                None
            }
        },
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "ECAA_AUDIT_SECRET is not valid hex — signed verdict sink not written; \
                 it must be the 64-hex-char per-session secret that \
                 ecaa-workflow-audit-proof --secret reads"
            );
            None
        }
    }
}

/// Resolve the config directory the finalize path reads its base
/// `interpretation-policy.json` (+ class overlay + extractor config) from:
///
/// 1. `ECAA_CONFIG_DIR` — explicit operator override, always wins.
/// 2. `<package_root>/policies` — the emitted package's OWN copied policies.
///
/// The emitter copies every downstream-policy `.json` FLAT into
/// `<root>/policies/` (no `downstream-policy/` subdir), and
/// `core::finalize` resolves policy files with a downstream-policy-first /
/// flat-fallback precedence — so pointing `config_dir` at `<root>/policies`
/// makes the package self-contained for verification regardless of where the
/// harness was launched. This drops the prior fragile binary-walk-up / CWD
/// fallback for the finalize path.
pub fn resolve_config_dir(package_root: &Path) -> PathBuf {
    if let Ok(explicit) = std::env::var("ECAA_CONFIG_DIR") {
        return PathBuf::from(explicit);
    }
    package_root.join("policies")
}

/// Read `runtime/decisions.jsonl` back into `Vec<DecisionRecord>`. Line-delimited
/// JSON; a malformed line is skipped (whole-run warned) rather than aborting.
/// Returns an empty vec when the file is absent or empty — the correct value for
/// a standalone run until Task 5 starts populating the log.
pub fn load_decisions(package_root: &Path) -> Vec<DecisionRecord> {
    let path = package_root.join("runtime").join("decisions.jsonl");
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut skipped = 0usize;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<DecisionRecord>(line) {
            Ok(rec) => out.push(rec),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        tracing::warn!(
            target: "harness-finalize",
            skipped,
            "skipped {} malformed line(s) in runtime/decisions.jsonl",
            skipped
        );
    }
    out
}

/// Derive `is_confirmatory` from the package's DAG: true when any task's id or
/// `source_atom_id` stem matches a [`CONFIRMATORY_STAGE_STEMS`] entry. The
/// package carries no session, so this is the package-derivable analog of the
/// server's `session.mode.is_confirmatory()`. Reads `WORKFLOW.json`; returns
/// `false` when it is absent/unparsable (the conservative default).
pub fn derive_is_confirmatory(package_root: &Path) -> bool {
    let Ok(bytes) = std::fs::read(package_root.join("WORKFLOW.json")) else {
        return false;
    };
    let Ok(wf) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    let Some(tasks) = wf.get("tasks").and_then(|t| t.as_object()) else {
        return false;
    };
    let is_confirmatory_stem = |s: &str| {
        CONFIRMATORY_STAGE_STEMS
            .iter()
            .any(|stem| s.contains(stem))
    };
    tasks.iter().any(|(task_id, t)| {
        if is_confirmatory_stem(task_id) {
            return true;
        }
        t.get("source_atom_id")
            .and_then(|v| v.as_str())
            .map(is_confirmatory_stem)
            .unwrap_or(false)
    })
}

// ---------------------------------------------------------------------------
// Environment snapshot integration
// ---------------------------------------------------------------------------

/// Check whether `ECAA_ENV_SNAPSHOT` opts OUT of snapshotting.
///
/// Default-ON: unset → `true`; `"0"` / `"false"` / `"no"` / `"off"`
/// (case-insensitive, trimmed) → `false`; any other value → `true`.
fn snapshot_enabled_from_env() -> bool {
    match std::env::var("ECAA_ENV_SNAPSHOT")
        .ok()
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        None | Some("") => true,
        Some("0") | Some("false") | Some("no") | Some("off") => false,
        Some(_) => true,
    }
}

/// Build [`SnapshotOpts`] from the live environment and the package contents.
///
/// Returns `None` when snapshotting is impossible or irrelevant:
/// - `resolve_cache_dir()` returns `None` (no HOME / ECAA_AGENT_CACHE_DIR).
/// - No `determinism-env.json` found under any task in `<pkg>/runtime/outputs/`
///   (nothing to snapshot for this run).
fn build_snapshot_opts(package_root: &Path) -> Option<SnapshotOpts> {
    let enabled = snapshot_enabled_from_env();

    let registry = std::env::var("ECAA_IMAGE_REGISTRY")
        .ok()
        .filter(|s| !s.is_empty());

    let cache_dir = resolve_cache_dir()?;

    // Scan <pkg>/runtime/outputs/ for the first task that carries a
    // determinism-env.json to extract base_digest + source_date_epoch.
    let outputs_dir = package_root.join("runtime").join("outputs");
    let mut task_dirs: Vec<std::fs::DirEntry> = std::fs::read_dir(&outputs_dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    // Sort for determinism.
    task_dirs.sort_by_key(|e| e.file_name());

    let mut base_digest = String::new();
    let mut source_date_epoch: i64 = 0;
    let mut found = false;

    for entry in &task_dirs {
        let det_path = entry.path().join("determinism-env.json");
        if !det_path.exists() {
            continue;
        }
        let raw = match std::fs::read_to_string(&det_path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let val: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(d) = val.get("task_container_digest").and_then(|v| v.as_str()) {
            base_digest = d.to_owned();
        } else {
            continue;
        }
        source_date_epoch = val
            .get("source_date_epoch")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);
        found = true;
        break;
    }

    if !found {
        return None;
    }

    Some(SnapshotOpts {
        enabled,
        registry,
        base_digest,
        source_date_epoch,
        cache_dir,
    })
}

/// Collect all task-directory names under `<pkg>/runtime/outputs/` that have a
/// `determinism-env.json`.  Returned in sorted order for determinism.
fn compute_task_ids(package_root: &Path) -> Vec<String> {
    let outputs_dir = package_root.join("runtime").join("outputs");
    let mut ids: Vec<String> = std::fs::read_dir(&outputs_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.path().is_dir() && e.path().join("determinism-env.json").exists()
        })
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids
}

/// Testable seam: identical to [`maybe_snapshot`] but accepts an injected
/// `snapshot_fn` so hermetic tests can substitute a stub without invoking
/// docker.  Never panics; never returns an error.
fn maybe_snapshot_with<F>(package_root: &Path, snapshot_fn: F)
where
    F: Fn(&SnapshotOpts) -> SnapshotOutcome,
{
    let Some(opts) = build_snapshot_opts(package_root) else {
        return;
    };

    match snapshot_fn(&opts) {
        SnapshotOutcome::Captured { digest, .. } => {
            tracing::info!(
                target: "harness-env-snapshot",
                %digest,
                "env snapshot captured; recording digest into package"
            );
            let ids = compute_task_ids(package_root);
            if let Err(e) = record_digest(package_root, &digest, &ids) {
                tracing::warn!(
                    target: "harness-env-snapshot",
                    error = %e,
                    "snapshot digest record failed: {e}"
                );
            }
        }
        SnapshotOutcome::SkippedNoInstalls => {
            tracing::info!(
                target: "harness-env-snapshot",
                "env snapshot skipped (no installs); base digest retained"
            );
        }
        SnapshotOutcome::Failed { reason } => {
            tracing::warn!(
                target: "harness-env-snapshot",
                %reason,
                "env snapshot failed: {reason}; base digest retained"
            );
        }
    }
}

/// Attempt a compute-environment snapshot at end of run.  Best-effort: never
/// panics, never returns an error, never breaks a run.
fn maybe_snapshot(package_root: &Path) {
    maybe_snapshot_with(package_root, env_snapshot::snapshot_environment);
}

// ---------------------------------------------------------------------------
// End-of-run package finalization
// ---------------------------------------------------------------------------

/// Source every input from the self-contained package (+ host config_dir + env
/// secret) and run [`finalize_package`] once. The whole thing is best-effort:
/// any failure is logged and `Ok(())` is returned so the caller still truncates
/// the WAL and exits cleanly. This is the unit-testable extraction of the
/// harness end-of-run finalize that `run_loop` calls inside its
/// `after.is_complete()` block.
///
/// `config_dir` is passed in explicitly (rather than resolved here) so the
/// caller can resolve it once at startup and the test can point it at the
/// repo's real config tree.
pub fn finalize_completed_package(package_root: &Path, config_dir: &Path) {
    let secret = audit_secret_from_env();
    let project_class = ProjectClass::default();
    let is_confirmatory = derive_is_confirmatory(package_root);
    let decisions = load_decisions(package_root);

    // Record the compute-environment snapshot digest into the package BEFORE
    // finalize_package runs, so any later crate projection sees the recorded
    // digest.  Non-fatal: any failure is swallowed inside maybe_snapshot.
    maybe_snapshot(package_root);

    // Surface a genuinely-missing/unreadable interpretation policy loudly: a
    // standalone run finalizing against the package's own `policies/` would
    // otherwise verify nothing (every task → Unavailable → no signed sink,
    // n_checked stays 0) without a trace. `assert_default_policy_present`
    // logs an error on Unavailable, a warn on Disabled, and is quiet when the
    // policy loads — so a real misconfiguration is no longer a silent no-op.
    ecaa_workflow_core::finalize::assert_default_policy_present(config_dir);

    match finalize_package(
        package_root,
        config_dir,
        project_class,
        &decisions,
        is_confirmatory,
        secret.as_ref(),
    ) {
        Ok(PackageFinalizeSummary {
            tasks_finalized,
            coverage_gaps,
        }) => {
            println!(
                "  finalized {} task(s); {} coverage gap(s)",
                tasks_finalized,
                coverage_gaps.len()
            );
            for gap in &coverage_gaps {
                tracing::warn!(target: "harness-finalize", gap = %gap, "coverage gap");
            }
            if secret.is_none() {
                tracing::warn!(
                    target: "harness-finalize",
                    "ECAA_AUDIT_SECRET unset/invalid — signed verdict sink not written; \
                     audit-proof Inv 1/5 stay Unverified"
                );
            }
        }
        Err(e) => {
            tracing::warn!(target: "harness-finalize", error = %e, "package finalize failed");
        }
    }
}

/// Run the OFFLINE repair loop once at end-of-run, strictly best-effort.
///
/// Called from the harness loop-exit convergence point (`main.rs`, the
/// `after.is_complete()` block) on BOTH run paths — the standalone/CLI run
/// (`progress.is_none()`) and the session/web-UI run where the server spawned
/// this harness with `--session-id` (`progress.is_some()`). The harness is the
/// execution engine on both paths, and the repair loop is self-sufficient:
/// [`run_repair_loop`] → `assess_package` re-runs `finalize_package` internally
/// and is idempotent, so it is correct to run here regardless of session. On the
/// session path this only ADDS repair (the server's incremental finalize never
/// repairs) and the idempotent re-finalize does not conflict with it. It is
/// gated solely by [`auto_repair_enabled`] (default OFF), independent of the
/// `progress` gate that scopes the standalone end-of-run finalize.
///
/// Uses [`ecaa_workflow_core::repair_loop::ReviewRoutingRunner`] — the offline
/// default: it applies deterministic prose/manifest repairs (prose-vs-table
/// counts, manifest re-seal) and ROUTES any agentic / offline-unverifiable gap
/// to the signed review list (`runtime/repair-status.json` +
/// `runtime/repair-requests.jsonl`) rather than re-executing an agent. AGENTIC
/// auto-repair deliberately stays the MANUAL `ecaa-workflow repair --agent`
/// path: running an agentic runner at end-of-run would re-enter the execution
/// loop, which the end-of-run hook must not do.
///
/// Every failure mode is swallowed here so the caller's run outcome is
/// untouched:
/// * a returned `Err` is logged at `warn` and dropped;
/// * a `panic!` inside the loop is caught via [`std::panic::catch_unwind`],
///   logged, and dropped.
pub fn run_auto_repair_best_effort(package_root: &Path, config_dir: &Path) {
    use ecaa_workflow_core::repair_loop::{run_repair_loop, ReviewRoutingRunner};

    // `catch_unwind` needs `UnwindSafe`; `&Path` is unwind-safe, and the
    // closure borrows only references. A panic across this boundary cannot
    // corrupt shared state because the repair loop owns its own on-disk writes.
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_repair_loop(package_root, config_dir, &ReviewRoutingRunner)
    }));

    match outcome {
        Ok(Ok(status)) => {
            tracing::info!(
                target: "harness-auto-repair",
                verdict = ?status.verdict,
                rounds = status.rounds,
                review_items = status.review.len(),
                "offline end-of-run repair loop finished"
            );
        }
        Ok(Err(e)) => {
            tracing::warn!(
                target: "harness-auto-repair",
                error = %e,
                "offline end-of-run repair loop failed (continuing — finalize outcome unchanged)"
            );
        }
        Err(_) => {
            tracing::warn!(
                target: "harness-auto-repair",
                "offline end-of-run repair loop panicked (caught — continuing, \
                 finalize outcome unchanged)"
            );
        }
    }
}

/// Reason-string marker prefix the standalone coverage gate writes into a
/// re-blocked task's `BlockedRecord.reason`. The core blocker mapper
/// (`ecaa_workflow_core::blocker::parse_agent_blocker_kind`) promotes this
/// prefix to `BlockerKind::ValidationFailed { check: "claim_coverage:<id>" }`,
/// matching the server's incremental verify path byte-for-byte.
const CLAIM_COVERAGE_MARKER: &str = "[claim_coverage]";

/// Per-task coverage gate for the STANDALONE harness path.
///
/// Finalizes a just-completed task FROM SOURCE (verify + sign the verdict sink
/// + refresh the plaintext sidecar + register evidence + regenerate audit-proof
/// — idempotent with the end-of-run [`finalize_completed_package`]) and inspects
/// the returned coverage. Returns `Some(reason)` — the `[claim_coverage]`
/// re-block marker the caller writes into the task's DAG state — ONLY when ALL
/// of:
///
/// 1. the task carries a Required expected-claim manifest entry (otherwise
///    `finalize_task` returns `coverage: None` and a non-confirmatory task
///    no-ops here naturally — no extra confirmatory check is needed),
/// 2. that coverage shows a Required recall gap
///    ([`coverage_should_block`] — absent or unverifiable), and
/// 3. enforcement is on (advisory / warn-only OFF).
///
/// The advisory toggle is [`validation_recovery::advisory_enabled`], whose
/// truthy table (`1`/`true`/`yes`/`on`/`t`/`y`, case-insensitive, trimmed) is
/// the same `ECAA_HARNESS_CONTRACT_ADVISORY` interpretation the server reads via
/// `Config.harness_contract_advisory` (both source from `core::config`'s
/// `parse_bool`). In advisory mode the gap is logged and the task is LEFT
/// completed — the signed verdict sink already persisted the gap as a durable
/// diagnostic — so this returns `None`.
///
/// A `finalize_task` error is non-fatal: it is logged and `None` is returned so
/// the harness never aborts a run on a finalize hiccup (the gate is additive,
/// not a hard guard on the dispatch loop).
pub fn coverage_reblock_reason(
    root: &Path,
    task_id: &str,
    config_dir: &Path,
    project_class: ProjectClass,
    decisions: &[DecisionRecord],
    is_confirmatory: bool,
    secret: Option<&[u8; 32]>,
) -> Option<String> {
    let res = match finalize_task(
        root,
        task_id,
        config_dir,
        project_class,
        decisions,
        is_confirmatory,
        secret,
    ) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "harness-coverage",
                error = %e,
                task_id,
                "coverage gate: finalize_task failed — leaving task completed"
            );
            return None;
        }
    };

    // `coverage` is `Some` only for a task with a Required manifest entry, so
    // a non-confirmatory / un-anchored task naturally falls through here.
    let cov = res.coverage?;
    if !coverage_should_block(&cov) {
        return None;
    }

    let detail = format!(
        "recall gap on task {}: {} required claim(s) absent, {} unverifiable",
        task_id, cov.required_absent, cov.required_unverifiable
    );

    // Advisory / warn-only mode (default OFF). Mirrors the server's
    // `harness_contract_advisory` branch: the recall gap is already persisted
    // into the signed verdict sink + audit-proof report by `finalize_task`
    // above, so the task is LEFT completed and the re-block is suppressed.
    if crate::validation_recovery::advisory_enabled() {
        tracing::warn!(
            target: "contract-advisory",
            %task_id,
            "[contract-advisory] {detail} (advisory, not blocking)"
        );
        return None;
    }

    tracing::warn!(
        target: "harness-coverage",
        %task_id,
        "{detail} — re-blocking"
    );
    Some(format!("{CLAIM_COVERAGE_MARKER} task={task_id} — {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env_snapshot::{SnapshotOutcome, StoreLocation};

    // -----------------------------------------------------------------------
    // Helpers for env-snapshot tests
    // -----------------------------------------------------------------------

    /// Build a minimal package tree under `tmp` with `task_ids` each having a
    /// `determinism-env.json`, and a `policies/container.json` with `{"image": null}`.
    fn make_snapshot_pkg(
        tmp: &tempfile::TempDir,
        task_ids: &[&str],
        base_digest: &str,
        source_date_epoch: i64,
    ) -> std::path::PathBuf {
        let pkg = tmp.path().to_path_buf();

        let policies = pkg.join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("container.json"), r#"{"image": null}"#).unwrap();

        for task_id in task_ids {
            let dir = pkg.join("runtime").join("outputs").join(task_id);
            std::fs::create_dir_all(&dir).unwrap();
            let content = serde_json::json!({
                "task_container_digest": base_digest,
                "source_date_epoch": source_date_epoch,
                "lang": "R"
            });
            std::fs::write(
                dir.join("determinism-env.json"),
                serde_json::to_string_pretty(&content).unwrap(),
            )
            .unwrap();
        }

        pkg
    }

    fn read_container_json(pkg: &std::path::Path) -> serde_json::Value {
        let raw = std::fs::read_to_string(pkg.join("policies").join("container.json")).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn read_det_env(pkg: &std::path::Path, task_id: &str) -> serde_json::Value {
        let raw = std::fs::read_to_string(
            pkg.join("runtime")
                .join("outputs")
                .join(task_id)
                .join("determinism-env.json"),
        )
        .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    // -----------------------------------------------------------------------
    // build_snapshot_opts: ECAA_ENV_SNAPSHOT gating
    // -----------------------------------------------------------------------

    // Safety note: std::env::set_var / remove_var are unsafe in Rust 1.93+
    // when other threads might be reading the environment concurrently.
    // Under cargo-nextest each test runs in its own process, making this safe.
    // The unsafe blocks below are bounded to environment-mutation test helpers
    // only — no production code uses unsafe.
    #[allow(unsafe_code)]
    #[test]
    fn build_snapshot_opts_disabled_when_env_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);

        // JUSTIFICATION: safe under nextest (process-per-test isolation).
        unsafe { std::env::set_var("ECAA_ENV_SNAPSHOT", "0"); }
        unsafe { std::env::set_var("HOME", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }

        let opts = build_snapshot_opts(&pkg);
        // opts may be None if cache_dir resolves to a path that exists but we
        // mostly care that IF opts is Some, enabled is false.
        if let Some(o) = opts {
            assert!(!o.enabled, "ECAA_ENV_SNAPSHOT=0 must produce enabled=false");
        }

        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }
    }

    #[allow(unsafe_code)]
    #[test]
    fn build_snapshot_opts_enabled_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 123);

        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }
        // Point agent cache into our tmp so resolve_cache_dir() returns Some.
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }

        let opts = build_snapshot_opts(&pkg);
        assert!(opts.is_some(), "should produce opts when cache_dir resolves");
        let o = opts.unwrap();
        assert!(o.enabled, "opts.enabled should be true when ECAA_ENV_SNAPSHOT unset");
        assert_eq!(o.base_digest, "sha256:base");
        assert_eq!(o.source_date_epoch, 123);

        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
    }

    // -----------------------------------------------------------------------
    // maybe_snapshot_with: SkippedNoInstalls → container.json unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn maybe_snapshot_with_skipped_leaves_container_json_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);

        maybe_snapshot_with(&pkg, |_opts| SnapshotOutcome::SkippedNoInstalls);

        let cj = read_container_json(&pkg);
        assert!(
            cj.get("digest").map(|v| v.is_null()).unwrap_or(true),
            "digest must not be written on SkippedNoInstalls; got: {:?}",
            cj.get("digest")
        );
        assert_eq!(cj["image"], serde_json::Value::Null,
            "image must remain null on SkippedNoInstalls");
    }

    // -----------------------------------------------------------------------
    // maybe_snapshot_with: Captured → digest written into container.json + task
    // -----------------------------------------------------------------------

    #[test]
    fn maybe_snapshot_with_captured_writes_digest() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);
        // Set ECAA_AGENT_CACHE_DIR so build_snapshot_opts returns Some.
        // JUSTIFICATION: safe under nextest (process-per-test isolation).
        #[allow(unsafe_code)]
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        maybe_snapshot_with(&pkg, |_opts| SnapshotOutcome::Captured {
            digest: "sha256:new".to_owned(),
            location: StoreLocation::LocalCas(std::path::PathBuf::from("/tmp/snap.tar")),
            note: None,
        });

        let cj = read_container_json(&pkg);
        assert_eq!(cj["digest"], "sha256:new", "container.json digest must be updated");
        assert_eq!(cj["image"], "sha256:new", "container.json image must be promoted from null");

        let det = read_det_env(&pkg, "task_a");
        assert_eq!(det["task_container_digest"], "sha256:new",
            "task_a determinism-env.json must be updated");

        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
    }

    // -----------------------------------------------------------------------
    // maybe_snapshot_with: Failed → no panic, container.json unchanged
    // -----------------------------------------------------------------------

    #[test]
    fn maybe_snapshot_with_failed_does_not_panic_and_leaves_container_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);

        // Even without ECAA_AGENT_CACHE_DIR, the snapshot_fn returning Failed
        // must not panic (build_snapshot_opts may return None, in which case
        // maybe_snapshot_with returns immediately without calling snapshot_fn
        // — that is also acceptable for the non-fatal contract).
        maybe_snapshot_with(&pkg, |_opts| SnapshotOutcome::Failed {
            reason: "boom".to_owned(),
        });

        // If container.json was untouched (no "digest" key), that is correct.
        let cj = read_container_json(&pkg);
        // Accept either: digest absent, or digest not written.
        let digest_written = cj.get("digest").map(|v| !v.is_null()).unwrap_or(false);
        assert!(!digest_written, "digest must not be written on Failed; got: {cj:?}");
    }

    #[test]
    fn audit_secret_requires_exactly_64_hex_chars() {
        // 64 hex chars → 32 bytes → Some.
        std::env::set_var("ECAA_AUDIT_SECRET", "ab".repeat(32));
        assert!(audit_secret_from_env().is_some(), "64 hex chars must decode");
        // Wrong length (62 chars) → None, not a loose fallback.
        std::env::set_var("ECAA_AUDIT_SECRET", "ab".repeat(31));
        assert!(
            audit_secret_from_env().is_none(),
            "31 bytes must be rejected (no raw-bytes fallback)"
        );
        // Non-hex → None.
        std::env::set_var("ECAA_AUDIT_SECRET", "not-hex-at-all-zz");
        assert!(audit_secret_from_env().is_none(), "non-hex must be None");
        // Unset → None.
        std::env::remove_var("ECAA_AUDIT_SECRET");
        assert!(audit_secret_from_env().is_none(), "unset must be None");
    }

    #[test]
    fn derive_is_confirmatory_detects_de_stage() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            r#"{"tasks":{"differential_expression":{"source_atom_id":"differential_expression"}}}"#,
        )
        .unwrap();
        assert!(derive_is_confirmatory(tmp.path()));
    }

    #[test]
    fn derive_is_confirmatory_false_for_non_confirmatory_dag() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("WORKFLOW.json"),
            r#"{"tasks":{"alignment":{"source_atom_id":"alignment"}}}"#,
        )
        .unwrap();
        assert!(!derive_is_confirmatory(tmp.path()));
        // Absent WORKFLOW.json → conservative false.
        let empty = tempfile::tempdir().unwrap();
        assert!(!derive_is_confirmatory(empty.path()));
    }

    #[test]
    fn load_decisions_absent_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_decisions(tmp.path()).is_empty());
    }

    #[test]
    fn load_decisions_skips_malformed_lines() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("runtime")).unwrap();
        // One blank, one malformed, no valid → empty but no panic.
        std::fs::write(
            tmp.path().join("runtime").join("decisions.jsonl"),
            "\n{ not json \n",
        )
        .unwrap();
        assert!(load_decisions(tmp.path()).is_empty());
    }

    #[test]
    fn resolve_config_dir_points_at_package_policies() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        // No ECAA_CONFIG_DIR override → the package's own policies/ dir.
        std::env::remove_var("ECAA_CONFIG_DIR");
        assert_eq!(resolve_config_dir(&root), root.join("policies"));
        // Explicit override wins.
        std::env::set_var("ECAA_CONFIG_DIR", "/some/explicit/config");
        assert_eq!(
            resolve_config_dir(&root),
            std::path::PathBuf::from("/some/explicit/config")
        );
        std::env::remove_var("ECAA_CONFIG_DIR");
    }
}
