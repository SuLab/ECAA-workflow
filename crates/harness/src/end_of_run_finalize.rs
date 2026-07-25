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
use ecaa_workflow_core::provenance::DivergenceRecord;
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

/// Whether the offline end-of-run repair loop is enabled. **Default ON** — the
/// loop runs unless `ECAA_AUTO_REPAIR` is explicitly set to a canonical falsy
/// value (`0` / `false` / `no` / `off` / `f` / `n`, case-insensitive, trimmed).
/// An unset var, an empty value, or any truthy value all enable it.
///
/// Rationale for the default flip (was OFF): the loop is best-effort, non-fatal,
/// and — under the default [`ReviewRoutingRunner`] — only APPLIES deterministic
/// prose/manifest repairs (prose-vs-table re-syncs that match the computed
/// ground-truth table, BagIt re-seals) while ROUTING any judgment-bearing or
/// agentic correction to the signed review list (`runtime/repair-status.json` +
/// `runtime/repair-requests.jsonl`). It never silently rewrites a scientific
/// claim and never re-executes an agent at end-of-run, so running it by default
/// surfaces issues (and fixes the unambiguous ones) rather than letting a run
/// finalize with un-actioned verification failures. Set `ECAA_AUTO_REPAIR=0` to
/// opt out (e.g. for bit-reproducibility arms that must not re-seal manifests).
pub fn auto_repair_enabled() -> bool {
    !matches!(
        std::env::var(ENV_AUTO_REPAIR)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off") | Some("f") | Some("n")
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
            Err(e) => {
                tracing::debug!(
                    target: "harness-env-snapshot",
                    path = %det_path.display(),
                    error = %e,
                    "skipping unparseable determinism-env.json"
                );
                continue;
            }
        };
        if let Some(d) = val.get("task_container_digest").and_then(|v| v.as_str()) {
            base_digest = d.to_owned();
        } else {
            continue;
        }
        source_date_epoch = val
            .get("source_date_epoch")
            .and_then(|v| v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse::<i64>().ok())))
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
/// `snapshot_fn` and a `reseal_fn` so hermetic tests can substitute stubs
/// without invoking docker or touching the real BagIt manifest.  Never panics;
/// never returns an error.
///
/// `reseal_fn` is called ONLY on the `Captured` outcome (the only arm that
/// mutates package files).  It is called AFTER `record_digest` regardless of
/// whether `record_digest` succeeded — a partial write still changes the
/// manifest covers, so re-sealing is always the right move on `Captured`.
/// Errors from `reseal_fn` are logged at `warn` and swallowed: the re-seal is
/// best-effort and must never promote to a fatal outcome.
fn maybe_snapshot_with<F, R>(package_root: &Path, snapshot_fn: F, reseal_fn: R)
where
    F: Fn(&SnapshotOpts) -> SnapshotOutcome,
    R: Fn(&std::path::Path) -> std::io::Result<()>,
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
            // Re-seal the BagIt manifest now that policies/container.json and
            // each runtime/outputs/<task>/determinism-env.json have been
            // mutated by record_digest.  This makes the manifest correct on
            // BOTH run paths — the session path (progress.is_some()) never
            // calls finalize_completed_package, so without this re-seal the
            // manifest would be stale and `bagit verify` would fail.
            if let Err(e) = reseal_fn(package_root) {
                tracing::warn!(
                    target: "harness-env-snapshot",
                    error = %e,
                    "BagIt manifest re-seal after snapshot failed (continuing — package written)"
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
///
/// Public so `main.rs` can call it unconditionally on BOTH run paths (the
/// standalone/CLI path AND the session/web-UI path where the server owns
/// incremental finalization).  The harness is the only component with the
/// assembled conda-envs/R-libs cache, so the server cannot snapshot it.
///
/// After a successful `Captured` outcome this function re-seals the BagIt
/// manifest itself (via `ecaa_workflow_core::emitter::regenerate_bagit_manifest`)
/// so the manifest is correct on BOTH run paths — not relying on a later
/// `finalize_completed_package` or auto-repair pass.
pub fn maybe_snapshot(package_root: &Path) {
    maybe_snapshot_with(
        package_root,
        env_snapshot::snapshot_environment,
        |p| {
            ecaa_workflow_core::emitter::regenerate_bagit_manifest(
                p,
                &ecaa_workflow_core::clock::WallClock,
            )
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
        },
    );
}

/// Capture a pinned determinism-env for the input-staging stage(s) at
/// end-of-run (T5.9 / DR-12), on BOTH run paths.
///
/// The input-staging `data_acquisition` stage is frequently pre-staged /
/// pre-completed at emit and never dispatched through the harness
/// determinism-env-stamp seam (`main::stamp_determinism_env`), so its
/// `determinism-env.json` keeps the emitter's empty pinning while every
/// executed sibling recorded the run-stable envelope
/// (`SOURCE_DATE_EPOCH` + `C.UTF-8` locale + `PYTHONHASHSEED=0`). This
/// backfills the staging stage FROM a populated sibling
/// ([`env_snapshot::record::backfill_missing_determinism_env`]) so every
/// stage records the same pinning, then re-seals the BagIt manifest
/// (each `runtime/outputs/<task>/determinism-env.json` is hashed into the
/// payload manifest on reseal).
///
/// Best-effort: a backfill / reseal failure logs and returns; it never
/// fails the run.
pub fn capture_staging_determinism_env(package_root: &Path) {
    match env_snapshot::record::backfill_missing_determinism_env(package_root) {
        Ok(0) => {}
        Ok(n) => {
            tracing::info!(
                target: "harness-finalize",
                backfilled = n,
                "backfilled pinned determinism-env for {n} staging task(s) that recorded empty pinning"
            );
            if let Err(e) = ecaa_workflow_core::emitter::regenerate_bagit_manifest(
                package_root,
                &ecaa_workflow_core::clock::WallClock,
            ) {
                tracing::warn!(
                    target: "harness-finalize",
                    error = %e,
                    "BagIt manifest re-seal after determinism-env backfill failed (continuing — files written)"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "determinism-env backfill for staging stage failed (continuing — run outcome unchanged)"
            );
        }
    }
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
///
/// NOTE: the compute-environment snapshot ([`maybe_snapshot`]) is NOT called
/// here.  It is called unconditionally by the harness on BOTH run paths from
/// `main.rs`.  [`maybe_snapshot`] re-seals the BagIt manifest itself after
/// recording the digest, so the manifest is already correct before this
/// function runs (or, on the session path, before the server re-seals).
pub fn finalize_completed_package(package_root: &Path, config_dir: &Path) {
    let secret = audit_secret_from_env();
    let project_class = ProjectClass::default();
    let is_confirmatory = derive_is_confirmatory(package_root);
    let decisions = load_decisions(package_root);

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

    // Guarantee the complete significant-entities table is present in the
    // terminal report before sealing — the reporting agent transcribes it
    // unreliably. Deterministic + idempotent (rendered from report-data.json);
    // returns whether a report file changed so the re-seal below covers it.
    let report_tables_written = ensure_full_significant_tables(package_root);

    // Design §5.2 C5 — fold what the run ACTUALLY read back into the emitted
    // RO-Crate's observed-provenance graph. This is the post-exec re-reconcile
    // the conversation emit path cannot do (it runs the same reconcile only at
    // INITIAL emit, before `runtime/invocations.jsonl` exists — a no-op then).
    // Runs after `finalize_package` so its verdict-injection writes cannot
    // clobber the ParameterConnection stamps. Best-effort; a mutation re-seals
    // the BagIt manifest because ro-crate-metadata.json is a manifested file.
    let (reconcile_wrote, divergences) = reconcile_observed_reads_inner(package_root);

    // T6.1 — a genuine observed-read divergence must NOT ship on the
    // STANDALONE path. The conversation/emit path blocks the SESSION on a
    // `Divergent` verdict (`apply_provenance_divergence_blockers`); a
    // no-session run has no session to transition, so re-block the offending
    // task(s) in WORKFLOW.json directly here — the standalone analog. The
    // divergences are already recorded durably on the RO-Crate root Dataset's
    // `ecaax:provenanceDivergence` array by the reconcile above; this
    // additionally flips the DAG so a downstream reader / re-run / deposit
    // gate cannot treat a run with an undeclared-read divergence as clean.
    let blocked_a_task = block_divergent_reads_in_dag(package_root, &divergences);

    // Single re-seal covering BOTH mutations (ro-crate-metadata.json from the
    // reconcile AND WORKFLOW.json from the divergence block) — both are
    // hashed into the payload manifest on reseal.
    if reconcile_wrote || blocked_a_task || report_tables_written {
        if let Err(e) = ecaa_workflow_core::emitter::regenerate_bagit_manifest(
            package_root,
            &ecaa_workflow_core::clock::WallClock,
        ) {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "BagIt manifest re-seal after observed-read reconcile / divergence block failed (continuing — files written)"
            );
        }
    }
}

/// Inject the deterministic complete significant-entities table (rendered from
/// `report-data.json`) into the terminal report(s), so the exhaustive table is
/// guaranteed present regardless of what the reporting agent hand-rendered.
/// Returns `true` iff a report file was modified (so the caller re-seals the
/// BagIt manifest). Idempotent and best-effort: a missing/unparseable
/// `report-data.json`, or nothing inlinable to render, is a silent no-op.
pub fn ensure_full_significant_tables(package_root: &Path) -> bool {
    use ecaa_workflow_core::report_contract::{
        ReportData, inject_full_tables, significant_entities_section,
    };
    let outputs = package_root.join("runtime").join("outputs");
    let Ok(raw) = std::fs::read_to_string(outputs.join("reporting").join("report-data.json")) else {
        return false;
    };
    let Ok(report_data) = serde_json::from_str::<ReportData>(&raw) else {
        return false;
    };
    let Some(block) = significant_entities_section(&report_data) else {
        return false;
    };
    let mut modified = false;
    for rel in ["final_reporting/final_report.md", "reporting/report.md"] {
        let path = outputs.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let updated = inject_full_tables(&text, &block);
        if updated != text {
            let tmp = path.with_extension("md.tmp");
            if std::fs::write(&tmp, &updated).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
                modified = true;
            }
        }
    }
    modified
}

/// Marker prefix the STANDALONE finalize path writes into a re-blocked
/// task's [`ecaa_workflow_core::dag::BlockedRecord`] reason on a genuine
/// observed-read divergence. Carries the JSON-serialized
/// `BlockerKind::ProvenanceDivergence` payload after the prefix (mirroring
/// the atom-safety dispatch markers in
/// `ecaa_workflow_core::blocker::format_safety_policy_marker`) so the typed
/// payload round-trips for any consumer that promotes the reason.
const PROVENANCE_DIVERGENCE_MARKER: &str = "[provenance_divergence]";

/// Render the `BlockedRecord.reason` for one divergent read. The prefix +
/// serialized [`ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence`]
/// mirror the emit-path blocker (`apply_provenance_divergence_blockers`)
/// byte-for-byte on the payload fields, so the standalone and session paths
/// surface the identical typed divergence.
fn provenance_divergence_reason(d: &DivergenceRecord) -> String {
    let kind = ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence {
        task_id: d.task_id.clone(),
        read_path: d.read_path.clone(),
        declared_producer: d.declared_producer.clone(),
    };
    match serde_json::to_string(&kind) {
        Ok(payload) => format!("{PROVENANCE_DIVERGENCE_MARKER} {payload}"),
        Err(_) => format!(
            "{PROVENANCE_DIVERGENCE_MARKER} task={} read {} matches no declared producer",
            d.task_id, d.read_path
        ),
    }
}

/// Standalone analog of the conversation emit path's
/// `apply_provenance_divergence_blockers`: flip the offending task(s) to
/// `TaskState::Blocked` in `WORKFLOW.json` on a genuine observed-read
/// divergence, so a real undeclared-read divergence cannot ship on the
/// no-session path.
///
/// A task already `Blocked` for another reason is left untouched (its prior
/// blocker is more actionable and must not be clobbered). Best-effort and
/// never a gate — a read/parse/serialize/write error logs and returns
/// `false`. Returns `true` only when it rewrote `WORKFLOW.json`, so the
/// caller re-seals the BagIt manifest (the DAG is hashed into the payload
/// manifest on reseal).
fn block_divergent_reads_in_dag(package_root: &Path, divergences: &[DivergenceRecord]) -> bool {
    use ecaa_workflow_core::dag::{BlockedRecord, TaskState, DAG};

    if divergences.is_empty() {
        return false;
    }
    let wf_path = package_root.join("WORKFLOW.json");
    let bytes = match std::fs::read(&wf_path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "provenance-divergence block: WORKFLOW.json unreadable — divergence recorded on the RO-Crate but the DAG was not re-blocked"
            );
            return false;
        }
    };
    let mut dag: DAG = match serde_json::from_slice(&bytes) {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "provenance-divergence block: WORKFLOW.json unparseable — divergence recorded on the RO-Crate but the DAG was not re-blocked"
            );
            return false;
        }
    };

    let mut changed = false;
    for d in divergences {
        let Some(task) = dag.tasks.get_mut(d.task_id.as_str()) else {
            continue;
        };
        // Don't clobber a task already blocked for another (more actionable)
        // reason — an existing blocker is at least as informative.
        if matches!(task.state, TaskState::Blocked { .. }) {
            continue;
        }
        task.state = TaskState::Blocked {
            record: BlockedRecord {
                reason: provenance_divergence_reason(d),
                attempts: Vec::new(),
            },
        };
        changed = true;
        tracing::warn!(
            target: "harness-finalize",
            task_id = %d.task_id,
            read_path = %d.read_path,
            "observed-read provenance divergence — re-blocking task on the standalone finalize path (undeclared read cannot ship)"
        );
    }

    if !changed {
        return false;
    }
    match serde_json::to_string_pretty(&dag) {
        Ok(s) => {
            if let Err(e) =
                ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&wf_path, s.as_bytes())
            {
                tracing::warn!(
                    target: "harness-finalize",
                    error = %e,
                    "provenance-divergence block: writing re-blocked WORKFLOW.json failed"
                );
                return false;
            }
            true
        }
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "provenance-divergence block: serializing re-blocked WORKFLOW.json failed"
            );
            false
        }
    }
}

/// Fold what the run ACTUALLY read back into the emitted RO-Crate's
/// observed-provenance graph (design §5.2 C5), strictly best-effort.
///
/// Reads the three provenance sidecars the package carries after a run —
/// the declared per-edge graph (`runtime/proofs.jsonl`), the
/// harness-observed reads (`runtime/invocations.jsonl`, folded across ALL
/// lines per task — the shape is two lines per dispatch, pre-dispatch +
/// enriched), and the per-task `read_allowance` facets
/// (`runtime/task-nodes.json`) — through the shared CORE parsers
/// (`ecaa_workflow_core::provenance`), then calls
/// `reconcile_ro_crate_edges_with_allowances` to stamp `ParameterConnection`
/// nodes authoritative/candidate_unused and record divergences /
/// read-allowances on the root Dataset. The observed provenance then
/// reflects what actually ran, not merely what the composer declared
/// possible — resolving, e.g., which member of the differential-expression
/// `raw_counts` / `normalized_counts` one-of group the run consumed.
///
/// This is the missing post-exec re-reconcile: the conversation emit path
/// runs the same reconcile, but only at INITIAL emit time when
/// `runtime/invocations.jsonl` does not exist yet (a documented no-op), so
/// without this hook the reconciliation never fires in a real run.
///
/// Best-effort and never a gate (matching this module's contract): any
/// read/parse/serialize/write error logs and returns `false`. Returns
/// `true` only when it rewrote `ro-crate-metadata.json`, so the caller can
/// re-seal the BagIt manifest (the descriptor is a manifested file). A no-op
/// (returns `false`, no write) before any harness dispatch — both inputs are
/// presence-gated and `reconcile_ro_crate_edges_with_allowances` itself
/// no-ops on empty inputs.
///
/// The divergence → `BlockerKind::ProvenanceDivergence` transition is
/// applied on EVERY execution path (§G-B2). `main.rs` calls this at the
/// harness loop-exit convergence point on BOTH the standalone/CLI run
/// (`progress.is_none()`) AND the session/web-UI run (`progress.is_some()`) —
/// so re-blocking the offending task in `WORKFLOW.json` here closes the gap
/// where the session path (the path that actually MINTS deposits) recorded a
/// genuine divergence on the RO-Crate but never blocked the DAG (the standalone
/// [`finalize_completed_package`] is skipped when `progress.is_some()`). The
/// divergences are also recorded durably on the RO-Crate root Dataset's
/// `ecaax:provenanceDivergence` array by the reconcile, and each is logged.
///
/// Returns `true` when it rewrote `ro-crate-metadata.json` OR `WORKFLOW.json`
/// (a genuine divergence re-block), so the caller re-seals the BagIt manifest
/// over BOTH mutated, manifested files.
pub fn reconcile_observed_reads_into_ro_crate(package_root: &Path) -> bool {
    let (wrote_descriptor, divergences) = reconcile_observed_reads_inner(package_root);
    // §G-B2 — a genuine observed-read divergence must NOT ship unblocked on
    // ANY path. A no-session run has no session to transition, and the
    // session/web-UI run's server finalize never reconciles observed reads, so
    // re-block the offending task(s) in WORKFLOW.json directly here — the same
    // block `finalize_completed_package` applies on the standalone path, now
    // fired on both because `main.rs` calls this function on both.
    let blocked_a_task = block_divergent_reads_in_dag(package_root, &divergences);
    wrote_descriptor || blocked_a_task
}

/// Shared implementation of [`reconcile_observed_reads_into_ro_crate`] that
/// ALSO surfaces the genuine (allowance-uncovered) divergences the core
/// reconciler computed. The public wrapper preserves the `-> bool`
/// signature the both-run-paths call site (`main.rs`) uses, while the
/// standalone finalize path consumes the divergence list to re-block.
///
/// Returns `(wrote_descriptor, divergences)`: `wrote_descriptor` is `true`
/// only when `ro-crate-metadata.json` was rewritten (so the caller re-seals
/// the manifest); `divergences` is the typed
/// [`DivergenceRecord`] list — empty when the inputs are empty, unreadable,
/// or every observed read matched a declared producer / a sanctioned
/// read-allowance.
fn reconcile_observed_reads_inner(package_root: &Path) -> (bool, Vec<DivergenceRecord>) {
    use ecaa_workflow_core::provenance;

    let declared_edges = provenance::read_declared_edges(package_root);
    let observed_reads = provenance::read_observed_reads(package_root);
    // Presence-gate before touching the descriptor: reconcile is a no-op on
    // either empty input, so there is nothing to stamp and nothing to write.
    if declared_edges.is_empty() || observed_reads.is_empty() {
        return (false, Vec::new());
    }
    let read_allowances = provenance::read_task_read_allowances(package_root);

    let path = package_root.join("ro-crate-metadata.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "observed-read reconcile: ro-crate-metadata.json unreadable — skipping"
            );
            return (false, Vec::new());
        }
    };
    let mut metadata: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "observed-read reconcile: ro-crate-metadata.json unparseable — skipping"
            );
            return (false, Vec::new());
        }
    };

    let divergences = ecaa_workflow_core::ro_crate::reconcile_ro_crate_edges_with_allowances(
        &mut metadata,
        &declared_edges,
        &observed_reads,
        &read_allowances,
    );

    let new_bytes = match serde_json::to_vec_pretty(&metadata) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "harness-finalize",
                error = %e,
                "observed-read reconcile: serializing reconciled ro-crate-metadata.json failed — skipping"
            );
            return (false, divergences);
        }
    };
    if let Err(e) = ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(&path, &new_bytes) {
        tracing::warn!(
            target: "harness-finalize",
            error = %e,
            "observed-read reconcile: writing reconciled ro-crate-metadata.json failed — skipping"
        );
        return (false, divergences);
    }

    for d in &divergences {
        tracing::warn!(
            target: "harness-finalize",
            task_id = %d.task_id,
            read_path = %d.read_path,
            "observed-read provenance divergence recorded in ro-crate-metadata.json \
             (ecaax:provenanceDivergence)"
        );
    }
    (true, divergences)
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
    // Observed-read post-exec reconcile (design §5.2 C5)
    // -----------------------------------------------------------------------

    /// Build a minimal DE package under `pkg`: a `ro-crate-metadata.json`
    /// carrying the two DE one-of `ParameterConnection` nodes, a
    /// `runtime/proofs.jsonl` declaring the raw + normalized count edges into
    /// DE (both tagged into the `counts` mutually-exclusive group), and a
    /// `runtime/invocations.jsonl` whose enriched line records an observed
    /// read of the RAW producer's output. This is exactly the on-disk state
    /// a real harness run leaves behind for the reconcile to fold.
    fn make_de_reconcile_pkg(pkg: &std::path::Path) {
        use crate::invocation_log::{append_invocation, InvocationRecord};
        use ecaa_workflow_core::atom::SafetyPolicy;
        use ecaa_workflow_core::provenance::ObservedRead;
        use ecaa_workflow_core::workflow_contracts::edge::{
            CompatibilityProof, EdgeContract, EdgeKind,
        };

        std::fs::create_dir_all(pkg.join("runtime")).unwrap();

        // ro-crate-metadata.json — root Dataset + the two ParameterConnection
        // nodes reconcile stamps (matched by @id).
        let metadata = serde_json::json!({
            "@graph": [
                {"@id": "./", "@type": "Dataset", "hasPart": []},
                {
                    "@id": "#parameter-connection/quantification__to__differential_expression",
                    "@type": "ParameterConnection"
                },
                {
                    "@id": "#parameter-connection/normalisation__to__differential_expression",
                    "@type": "ParameterConnection"
                }
            ]
        });
        std::fs::write(
            pkg.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        // proofs.jsonl — the declared per-edge graph.
        let edges = [
            EdgeContract {
                from_node: "quantification".into(),
                from_port: "count_matrix".into(),
                to_node: "differential_expression".into(),
                to_port: "raw_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
            EdgeContract {
                from_node: "normalisation".into(),
                from_port: "normalized_counts".into(),
                to_node: "differential_expression".into(),
                to_port: "normalized_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
        ];
        let mut proofs = String::new();
        for e in &edges {
            proofs.push_str(&serde_json::to_string(e).unwrap());
            proofs.push('\n');
        }
        std::fs::write(pkg.join("runtime/proofs.jsonl"), proofs).unwrap();

        // invocations.jsonl — pre-dispatch line + enriched follow-up carrying
        // the observed read of the RAW producer's output.
        let base = InvocationRecord::new(
            "differential_expression",
            Some("differential_expression"),
            1,
            "run-recon",
            "2026-07-17T00:00:00Z",
            &["quantification".to_string(), "normalisation".to_string()],
            true,
            &SafetyPolicy::default(),
            None,
        );
        append_invocation(pkg, &base).unwrap();
        let enriched = base.with_observed_reads(vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }]);
        append_invocation(pkg, &enriched).unwrap();
    }

    fn read_metadata_graph(pkg: &std::path::Path) -> Vec<serde_json::Value> {
        let raw = std::fs::read_to_string(pkg.join("ro-crate-metadata.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["@graph"].as_array().unwrap().clone()
    }

    fn provenance_status<'a>(graph: &'a [serde_json::Value], id: &str) -> Option<&'a str> {
        graph
            .iter()
            .find(|e| e["@id"] == id)
            .and_then(|e| e.get("ecaax:provenanceStatus"))
            .and_then(|s| s.as_str())
    }

    /// End-to-end: after `finalize_completed_package`, the DE package's
    /// RO-Crate must stamp the RAW edge authoritative and DROP the NORMALIZED
    /// edge from the standard graph (recording it only in the ecaax
    /// `unusedCandidateEdge` side channel, per §G-B1), driven by the observed
    /// read of the raw producer's output. Before the post-exec hook is wired,
    /// `finalize_completed_package` leaves both nodes unstamped.
    #[test]
    fn finalize_completed_package_reconciles_observed_reads_into_ro_crate() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        make_de_reconcile_pkg(pkg);
        // A config_dir with no policies is fine: finalize_package warns and
        // returns without touching ro-crate-metadata.json, then the reconcile
        // runs.
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();

        finalize_completed_package(pkg, &config_dir);

        let graph = read_metadata_graph(pkg);
        assert_eq!(
            provenance_status(
                &graph,
                "#parameter-connection/quantification__to__differential_expression"
            ),
            Some("authoritative"),
            "the raw-counts edge the run actually read must be stamped authoritative"
        );
        // §G-B1 (FixU-T62): the unread normalized-counts sibling is now DROPPED
        // from the standard graph (not stamped) so a generic RO-Crate/WRROC
        // consumer never sees it as a data flow; it survives only in the ecaax
        // side channel on the root Dataset.
        assert_eq!(
            provenance_status(
                &graph,
                "#parameter-connection/normalisation__to__differential_expression"
            ),
            None,
            "the unread normalized-counts sibling must be DROPPED from the standard graph"
        );
        let root = graph
            .iter()
            .find(|e| e["@id"] == "./")
            .expect("root Dataset node present");
        // The side channel now references a first-class @graph node by `@id`
        // (the RO-Crate/runcrate `@id` fix), so resolve each reference before
        // reading its fields.
        let unused = root["ecaax:unusedCandidateEdge"]
            .as_array()
            .expect("unused-candidate side channel recorded on root Dataset");
        assert!(
            unused.iter().any(|r| {
                graph
                    .iter()
                    .find(|e| e["@id"] == r["@id"])
                    .is_some_and(|u| {
                        u["to_node"] == "differential_expression"
                            && u["to_port"] == "normalized_counts"
                            && u["ecaax:provenanceStatus"] == "candidate_unused"
                    })
            }),
            "the unread normalized-counts edge must be recorded candidate_unused in the ecaax side channel"
        );
    }

    /// The reconcile helper is a no-op (no write, returns false) on a package
    /// with no invocations — the common pre-dispatch case.
    #[test]
    fn reconcile_observed_reads_is_noop_without_invocations() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        std::fs::write(
            pkg.join("ro-crate-metadata.json"),
            r#"{"@graph":[{"@id":"./","@type":"Dataset"}]}"#,
        )
        .unwrap();
        let before = std::fs::read(pkg.join("ro-crate-metadata.json")).unwrap();
        assert!(!reconcile_observed_reads_into_ro_crate(pkg));
        let after = std::fs::read(pkg.join("ro-crate-metadata.json")).unwrap();
        assert_eq!(before, after, "no invocations → descriptor left untouched");
    }

    // -----------------------------------------------------------------------
    // T6.1 — standalone finalize BLOCKS on a genuine Divergent observed read
    // -----------------------------------------------------------------------

    /// Write a minimal `WORKFLOW.json` under `pkg` with `differential_expression`
    /// in `Completed` state (the shape a finished run leaves on disk), so the
    /// standalone divergence-block can flip it to `Blocked`.
    fn write_de_workflow_json(pkg: &std::path::Path) {
        let wf = serde_json::json!({
            "version": "1",
            "workflow_id": "de-test",
            "current_task": null,
            "tasks": {
                "differential_expression": {
                    "kind": "computation",
                    "state": {"status": "completed", "result": null},
                    "depends_on": ["quantification", "normalisation"],
                    "assignee": "agent",
                    "description": "differential expression"
                }
            }
        });
        std::fs::write(
            pkg.join("WORKFLOW.json"),
            serde_json::to_vec_pretty(&wf).unwrap(),
        )
        .unwrap();
    }

    /// Build a DE package like [`make_de_reconcile_pkg`] but with an observed
    /// read at `read_path` — pass a path OUTSIDE any declared producer's
    /// output dir to plant a genuine `Divergent` verdict.
    fn make_de_pkg_with_read(pkg: &std::path::Path, read_path: &str) {
        use crate::invocation_log::{append_invocation, InvocationRecord};
        use ecaa_workflow_core::atom::SafetyPolicy;
        use ecaa_workflow_core::provenance::ObservedRead;
        use ecaa_workflow_core::workflow_contracts::edge::{
            CompatibilityProof, EdgeContract, EdgeKind,
        };

        std::fs::create_dir_all(pkg.join("runtime")).unwrap();

        let metadata = serde_json::json!({
            "@graph": [
                {"@id": "./", "@type": "Dataset", "hasPart": []},
                {
                    "@id": "#parameter-connection/quantification__to__differential_expression",
                    "@type": "ParameterConnection"
                },
                {
                    "@id": "#parameter-connection/normalisation__to__differential_expression",
                    "@type": "ParameterConnection"
                }
            ]
        });
        std::fs::write(
            pkg.join("ro-crate-metadata.json"),
            serde_json::to_vec_pretty(&metadata).unwrap(),
        )
        .unwrap();

        let edges = [
            EdgeContract {
                from_node: "quantification".into(),
                from_port: "count_matrix".into(),
                to_node: "differential_expression".into(),
                to_port: "raw_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
            EdgeContract {
                from_node: "normalisation".into(),
                from_port: "normalized_counts".into(),
                to_node: "differential_expression".into(),
                to_port: "normalized_counts".into(),
                proof: CompatibilityProof::default(),
                kind: EdgeKind::TypedDataFlow,
                chain_of_custody: None,
                mutually_exclusive_group: Some("counts".into()),
            },
        ];
        let mut proofs = String::new();
        for e in &edges {
            proofs.push_str(&serde_json::to_string(e).unwrap());
            proofs.push('\n');
        }
        std::fs::write(pkg.join("runtime/proofs.jsonl"), proofs).unwrap();

        let base = InvocationRecord::new(
            "differential_expression",
            Some("differential_expression"),
            1,
            "run-recon",
            "2026-07-17T00:00:00Z",
            &["quantification".to_string(), "normalisation".to_string()],
            true,
            &SafetyPolicy::default(),
            None,
        );
        append_invocation(pkg, &base).unwrap();
        let enriched = base.with_observed_reads(vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: read_path.to_string(),
        }]);
        append_invocation(pkg, &enriched).unwrap();
    }

    fn task_status(pkg: &std::path::Path, task_id: &str) -> String {
        let raw = std::fs::read_to_string(pkg.join("WORKFLOW.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["tasks"][task_id]["state"]["status"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    fn task_block_reason(pkg: &std::path::Path, task_id: &str) -> String {
        let raw = std::fs::read_to_string(pkg.join("WORKFLOW.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["tasks"][task_id]["state"]["record"]["reason"]
            .as_str()
            .unwrap_or("")
            .to_string()
    }

    /// A genuine `Divergent` observed read (a path under NO declared
    /// producer's output dir) must flip the offending task to `Blocked` in
    /// WORKFLOW.json on the standalone finalize path — the standalone analog
    /// of the emit-path `provenance_divergence_transitions_task_to_typed_blocker`.
    /// The re-block reason carries the `[provenance_divergence]` marker + the
    /// serialized `BlockerKind::ProvenanceDivergence` payload so it round-trips.
    #[test]
    fn finalize_completed_package_blocks_on_genuine_divergent_read() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Observed read is under data_acquisition/, which is NOT a declared
        // producer of differential_expression (declared: quantification /
        // normalisation) → Divergent.
        make_de_pkg_with_read(pkg, "runtime/outputs/data_acquisition/counts.tsv");
        write_de_workflow_json(pkg);
        let config_dir = pkg.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();

        finalize_completed_package(pkg, &config_dir);

        assert_eq!(
            task_status(pkg, "differential_expression"),
            "blocked",
            "a genuine undeclared-read divergence must re-block the task on the standalone path"
        );
        let reason = task_block_reason(pkg, "differential_expression");
        assert!(
            reason.starts_with(PROVENANCE_DIVERGENCE_MARKER),
            "re-block reason must carry the provenance-divergence marker; got: {reason}"
        );
        // The serialized payload round-trips into the typed blocker.
        let payload = reason
            .strip_prefix(PROVENANCE_DIVERGENCE_MARKER)
            .unwrap()
            .trim();
        let kind: ecaa_workflow_core::blocker::BlockerKind =
            serde_json::from_str(payload).expect("payload must deserialize to a BlockerKind");
        match kind {
            ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence {
                task_id,
                read_path,
                declared_producer,
            } => {
                assert_eq!(task_id, "differential_expression");
                assert_eq!(read_path, "runtime/outputs/data_acquisition/counts.tsv");
                // declared_port raw_counts → declared producer quantification.
                assert_eq!(declared_producer.as_deref(), Some("quantification"));
            }
            other => panic!("expected ProvenanceDivergence, got {other:?}"),
        }
    }

    /// A clean run — every observed read matches a declared producer's output
    /// — must NOT re-block: the DE task stays `completed`.
    #[test]
    fn finalize_completed_package_clean_run_does_not_block() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Read of the RAW producer's own output dir → Match, not Divergent.
        make_de_pkg_with_read(pkg, "runtime/outputs/quantification/count_matrix.tsv");
        write_de_workflow_json(pkg);
        let config_dir = pkg.join("config");
        std::fs::create_dir_all(&config_dir).unwrap();

        finalize_completed_package(pkg, &config_dir);

        assert_eq!(
            task_status(pkg, "differential_expression"),
            "completed",
            "a clean run (every read matches a declared producer) must not re-block"
        );
    }

    // -----------------------------------------------------------------------
    // §G-B2 — the SESSION/web-UI path (progress.is_some()) blocks a genuine
    // divergence too. main.rs calls reconcile_observed_reads_into_ro_crate on
    // BOTH paths, and the session path never reaches finalize_completed_package
    // (that is progress.is_none()-gated), so this function is the ONLY blocking
    // entry point on the deposit-minting path — it must block there.
    // -----------------------------------------------------------------------

    /// A genuine `Divergent` observed read must flip the offending task to
    /// `Blocked` in WORKFLOW.json via `reconcile_observed_reads_into_ro_crate`
    /// — the both-paths entry point main.rs uses on the session/web-UI path.
    /// Mirrors `finalize_completed_package_blocks_on_genuine_divergent_read`
    /// but exercises the session-path function directly.
    #[test]
    fn reconcile_observed_reads_into_ro_crate_blocks_on_genuine_divergent_read() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Read under data_acquisition/ — NOT a declared producer of DE
        // (declared: quantification / normalisation) → Divergent.
        make_de_pkg_with_read(pkg, "runtime/outputs/data_acquisition/counts.tsv");
        write_de_workflow_json(pkg);

        let wrote = reconcile_observed_reads_into_ro_crate(pkg);
        assert!(
            wrote,
            "a genuine divergence re-blocks WORKFLOW.json → the fn must report a mutation to reseal"
        );

        assert_eq!(
            task_status(pkg, "differential_expression"),
            "blocked",
            "the session/web-UI path must re-block a genuine undeclared-read divergence"
        );
        let reason = task_block_reason(pkg, "differential_expression");
        assert!(
            reason.starts_with(PROVENANCE_DIVERGENCE_MARKER),
            "re-block reason must carry the provenance-divergence marker; got: {reason}"
        );
        let payload = reason
            .strip_prefix(PROVENANCE_DIVERGENCE_MARKER)
            .unwrap()
            .trim();
        let kind: ecaa_workflow_core::blocker::BlockerKind =
            serde_json::from_str(payload).expect("payload must deserialize to a BlockerKind");
        assert!(matches!(
            kind,
            ecaa_workflow_core::blocker::BlockerKind::ProvenanceDivergence { .. }
        ));
    }

    /// A clean run — every observed read matches a declared producer — must
    /// NOT re-block via the session-path function: DE stays `completed`.
    #[test]
    fn reconcile_observed_reads_into_ro_crate_clean_run_does_not_block() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        // Read of the RAW producer's own output dir → Match, not Divergent.
        make_de_pkg_with_read(pkg, "runtime/outputs/quantification/count_matrix.tsv");
        write_de_workflow_json(pkg);

        reconcile_observed_reads_into_ro_crate(pkg);

        assert_eq!(
            task_status(pkg, "differential_expression"),
            "completed",
            "a clean run must not re-block on the session/web-UI path"
        );
    }

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
        // Set ECAA_AGENT_CACHE_DIR so resolve_cache_dir() returns Some and
        // build_snapshot_opts reaches the enabled check rather than returning
        // None vacuously.
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }

        let opts = build_snapshot_opts(&pkg);
        assert!(opts.is_some(), "ECAA_AGENT_CACHE_DIR set + task present → opts must be Some");
        assert!(!opts.unwrap().enabled, "ECAA_ENV_SNAPSHOT=0 must produce enabled=false");

        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
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

    #[allow(unsafe_code)]
    #[test]
    fn maybe_snapshot_with_skipped_leaves_container_json_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);

        // JUSTIFICATION: safe under nextest (process-per-test isolation).
        // Set ECAA_AGENT_CACHE_DIR so build_snapshot_opts returns Some and the
        // SkippedNoInstalls arm is genuinely exercised (not short-circuited).
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        maybe_snapshot_with(&pkg, |_opts| SnapshotOutcome::SkippedNoInstalls, |_p| Ok(()));

        let cj = read_container_json(&pkg);
        assert!(
            cj.get("digest").map(|v| v.is_null()).unwrap_or(true),
            "digest must not be written on SkippedNoInstalls; got: {:?}",
            cj.get("digest")
        );
        assert_eq!(cj["image"], serde_json::Value::Null,
            "image must remain null on SkippedNoInstalls");

        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
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

        maybe_snapshot_with(
            &pkg,
            |_opts| SnapshotOutcome::Captured {
                digest: "sha256:new".to_owned(),
                location: StoreLocation::LocalCas(std::path::PathBuf::from("/tmp/snap.tar")),
                note: None,
            },
            |_p| Ok(()),
        );

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

    #[allow(unsafe_code)]
    #[test]
    fn maybe_snapshot_with_failed_does_not_panic_and_leaves_container_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);

        // JUSTIFICATION: safe under nextest (process-per-test isolation).
        // Set ECAA_AGENT_CACHE_DIR so build_snapshot_opts returns Some and the
        // Failed-returning stub is ACTUALLY invoked (not short-circuited at the
        // None-return from build_snapshot_opts).
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        let called = std::sync::atomic::AtomicBool::new(false);
        maybe_snapshot_with(
            &pkg,
            |_opts| {
                called.store(true, std::sync::atomic::Ordering::Relaxed);
                SnapshotOutcome::Failed { reason: "boom".to_owned() }
            },
            |_p| Ok(()),
        );
        assert!(called.load(std::sync::atomic::Ordering::Relaxed),
            "snapshot_fn must be invoked when ECAA_AGENT_CACHE_DIR is set and a task is present");

        // container.json must be untouched — no digest written on Failed.
        let cj = read_container_json(&pkg);
        let digest_written = cj.get("digest").map(|v| !v.is_null()).unwrap_or(false);
        assert!(!digest_written, "digest must not be written on Failed; got: {cj:?}");

        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
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

    // -----------------------------------------------------------------------
    // maybe_snapshot_with: reseal_fn invocation contract
    // -----------------------------------------------------------------------

    /// `reseal_fn` IS called on `Captured` (after record_digest).
    #[test]
    fn maybe_snapshot_with_captured_calls_reseal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);
        #[allow(unsafe_code)]
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        let reseal_called = std::cell::Cell::new(0u32);
        maybe_snapshot_with(
            &pkg,
            |_opts| SnapshotOutcome::Captured {
                digest: "sha256:captured".to_owned(),
                location: StoreLocation::LocalCas(std::path::PathBuf::from("/tmp/snap.tar")),
                note: None,
            },
            |_p| { reseal_called.set(reseal_called.get() + 1); Ok(()) },
        );
        assert_eq!(reseal_called.get(), 1, "reseal_fn must be called exactly once on Captured");

        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
    }

    /// `reseal_fn` is NOT called on `SkippedNoInstalls` (no files mutated).
    #[test]
    fn maybe_snapshot_with_skipped_does_not_call_reseal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);
        #[allow(unsafe_code)]
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        let reseal_called = std::cell::Cell::new(0u32);
        maybe_snapshot_with(
            &pkg,
            |_opts| SnapshotOutcome::SkippedNoInstalls,
            |_p| { reseal_called.set(reseal_called.get() + 1); Ok(()) },
        );
        assert_eq!(reseal_called.get(), 0, "reseal_fn must NOT be called on SkippedNoInstalls");

        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
    }

    /// `reseal_fn` is NOT called on `Failed` (no files mutated).
    #[test]
    fn maybe_snapshot_with_failed_does_not_call_reseal() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = make_snapshot_pkg(&tmp, &["task_a"], "sha256:base", 0);
        #[allow(unsafe_code)]
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }
        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }

        let reseal_called = std::cell::Cell::new(0u32);
        maybe_snapshot_with(
            &pkg,
            |_opts| SnapshotOutcome::Failed { reason: "boom".to_owned() },
            |_p| { reseal_called.set(reseal_called.get() + 1); Ok(()) },
        );
        assert_eq!(reseal_called.get(), 0, "reseal_fn must NOT be called on Failed");

        #[allow(unsafe_code)]
        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
    }

    // -----------------------------------------------------------------------
    // build_snapshot_opts: string source_date_epoch tolerated
    // -----------------------------------------------------------------------

    /// A `determinism-env.json` with `"source_date_epoch"` as a QUOTED STRING
    /// must be parsed correctly (the canonical producer writes it as a string).
    #[allow(unsafe_code)]
    #[test]
    fn build_snapshot_opts_tolerates_string_source_date_epoch() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path().to_path_buf();

        // Write container.json
        let policies = pkg.join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        std::fs::write(policies.join("container.json"), r#"{"image": null}"#).unwrap();

        // Write determinism-env.json with source_date_epoch as a STRING
        let dir = pkg.join("runtime").join("outputs").join("task_str_epoch");
        std::fs::create_dir_all(&dir).unwrap();
        let content = serde_json::json!({
            "task_container_digest": "sha256:base",
            "source_date_epoch": "1700000000",
            "lang": "R"
        });
        std::fs::write(
            dir.join("determinism-env.json"),
            serde_json::to_string_pretty(&content).unwrap(),
        )
        .unwrap();

        // JUSTIFICATION: safe under nextest (process-per-test isolation).
        unsafe { std::env::remove_var("ECAA_ENV_SNAPSHOT"); }
        unsafe { std::env::set_var("ECAA_AGENT_CACHE_DIR", tmp.path()); }
        unsafe { std::env::remove_var("ECAA_CHAT_SESSION_ID"); }

        let opts = build_snapshot_opts(&pkg);
        assert!(opts.is_some(), "opts must be Some when task+cache present");
        assert_eq!(
            opts.unwrap().source_date_epoch,
            1_700_000_000,
            "string source_date_epoch '1700000000' must parse to i64 1700000000"
        );

        unsafe { std::env::remove_var("ECAA_AGENT_CACHE_DIR"); }
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

    // -----------------------------------------------------------------------
    // ensure_full_significant_tables — deterministic complete-table injection
    // -----------------------------------------------------------------------

    #[test]
    fn ensure_full_tables_makes_a_truncated_report_pass_rc_table() {
        use ecaa_workflow_core::reporting_invariants::check_reporting_invariants;
        let tmp = tempfile::TempDir::new().unwrap();
        let outputs = tmp.path().join("runtime").join("outputs");
        std::fs::create_dir_all(outputs.join("reporting")).unwrap();
        std::fs::create_dir_all(outputs.join("final_reporting")).unwrap();

        // report-data.json: 3 significant entities, not spilled.
        let rd = serde_json::json!({
            "artifacts": [{
                "stage_id": "differential_expression", "artifact": "de_results.tsv",
                "n_total": 100, "n_significant": 3, "direction_split": null,
                "effect_distribution": null,
                "significant_entities": [
                    {"entity":"ENSG_A","effect":1.0,"significance":0.001,"literature":{"status":"novel"}},
                    {"entity":"ENSG_B","effect":-2.0,"significance":0.0004,"literature":{"status":"novel"}},
                    {"entity":"ENSG_C","effect":0.5,"significance":0.02,"literature":{"status":"novel"}}
                ],
                "significant_table_path":"runtime/outputs/differential_expression/de_results.significant.tsv",
                "full_table_path":"runtime/outputs/differential_expression/de_results.full.tsv",
                "spilled_to_attachment_only": false
            }],
            "literature": null
        });
        std::fs::write(outputs.join("reporting/report-data.json"), rd.to_string()).unwrap();
        // Terminal report renders only ONE of the three (the agent-truncation bug).
        std::fs::write(
            outputs.join("final_reporting/final_report.md"),
            "# Final\n## Primary Results\n| ENSG_A |\n",
        )
        .unwrap();

        // Precondition: RC-TABLE fails on the truncated report.
        let before = check_reporting_invariants(tmp.path());
        assert!(
            before
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-TABLE")),
            "precondition: truncated report must fail RC-TABLE: {before:?}"
        );

        let modified = ensure_full_significant_tables(tmp.path());
        assert!(modified, "the truncated report must have been rewritten");

        // After: every entity present → RC-TABLE passes.
        let after = check_reporting_invariants(tmp.path());
        assert!(
            !after
                .required_failures()
                .iter()
                .any(|f| f.contains("RC-TABLE")),
            "after ensure_full_significant_tables the terminal report must satisfy RC-TABLE: {after:?}"
        );
        let text =
            std::fs::read_to_string(outputs.join("final_reporting/final_report.md")).unwrap();
        for e in ["ENSG_A", "ENSG_B", "ENSG_C"] {
            assert!(text.contains(e), "entity {e} must be in the rewritten report");
        }

        // Idempotent: a second pass makes no change.
        let again = ensure_full_significant_tables(tmp.path());
        assert!(!again, "re-running on an already-injected report is a no-op");
    }
}
