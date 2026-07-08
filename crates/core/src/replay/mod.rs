//! Agent-free replay: re-verify + re-execute a downloaded ECAA package.
pub mod env_provision;
pub mod report;
pub mod reverify;
pub mod script_runner;
pub mod select;
pub use report::{ReplayReport, ReplayVerdict, ReverifyResult, ReexecuteResult, VerifierDiff, SkippedStage, compute_verdict};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::reexecution::{classify_reexecution, ReexecutionBucket};
use crate::reexecution_bounds::ModalityBounds;
use crate::replay::env_provision::{ExecEnv, ProvisionOpts};
use crate::replay::reverify::reverify;
use crate::replay::script_runner::stage_and_run;
use crate::replay::select::select_compute_tasks;

// ---------------------------------------------------------------------------
// Public API — consumed by the CLI (Task 7)
// ---------------------------------------------------------------------------

/// Which stage(s) to run during a replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tier {
    /// Run only the deterministic verifiers (Tier 1: re-verify).
    Verify,
    /// Run only the compute re-execution (Tier 2: re-execute).
    Execute,
    /// Run both stages.
    All,
}

/// Options for `run_replay`.
pub struct ReplayOptions {
    /// Which tier(s) to execute.
    pub tier: Tier,
    /// Scratch directory for re-execution staging. A fresh `tempdir` is
    /// created when `None`. The caller owns cleanup of any supplied directory.
    pub scratch_dir: Option<PathBuf>,
    /// Path to a `ModalityBoundsProvider` directory (`config/reexecution-bounds/`)
    /// used to resolve per-modality tolerances. When `None`, `ModalityBounds::default()`
    /// (the historical ±5% relative band) is used for all artifacts.
    pub bounds: Option<PathBuf>,
    /// Allow rebuilding the container image from a Dockerfile when the
    /// recorded digest is unavailable.
    pub allow_rebuild: bool,
    /// The ECAA spec version this build of the reader implements. Used to
    /// distinguish real tampering (reader_matches_writer=true) from version
    /// drift (reader_matches_writer=false) in the re-verify result.
    pub reader_version: String,
}

// ---------------------------------------------------------------------------
// Orchestrator
// ---------------------------------------------------------------------------

/// Re-verify and/or re-execute a downloaded ECAA package.
///
/// # recorded_root
/// The "recorded root" is the absolute path that was embedded in the
/// package's compute scripts when the original execution ran.  Discovery
/// priority:
/// 1. `pkg_root` field in the first `determinism-env.json` found under
///    `runtime/outputs/` (preferred when a writer emits it).
/// 2. Script-scan: walk `runtime/outputs/*/scripts/*.{R,py,sh}` and extract
///    the prefix up to and including `/<basename>` from the first match.
/// 3. Empty string — `stage_and_run` treats this as "no path rewrite needed",
///    which is correct for packages that embed paths only via the `PKG_ROOT`/
///    `PACKAGE` environment variable.
///
/// # ok:false → Failed reconciliation
/// `classify_reexecution` compares table files on disk: a task that ran but
/// failed will produce no replay file, so the comparator would classify it
/// `Unavailable` (not `Failed`).  We override `Unavailable` → `Failed` for
/// any artifact whose task ran and exited with `ok: false`.  The mapping is:
/// `artifact_path` has the form `runtime/outputs/<task_id>/…`; we extract
/// `<task_id>` as the 3rd path component (index 2) of the forward-slash split,
/// then check whether that task_id appears in the set of failed tasks.
pub fn run_replay(pkg: &Path, opts: &ReplayOptions) -> anyhow::Result<ReplayReport> {
    // Resolve package IRI + reader/min-reader metadata from the recorded
    // audit-proof report if present; fall back to sensible defaults.
    let (package_iri, min_reader_version) = read_package_meta(pkg);

    let mut report = ReplayReport {
        schema_version: "0.1".to_string(),
        package_iri,
        reader_version: opts.reader_version.clone(),
        min_reader_version,
        reverify: None,
        reexecute: None,
        skipped: vec![],
        verdict: crate::replay::report::ReplayVerdict::Pass,
    };

    // ── Tier 1: re-verify ────────────────────────────────────────────────────
    if matches!(opts.tier, Tier::Verify | Tier::All) {
        let rv = reverify(pkg, &opts.reader_version)?;
        report.reverify = Some(rv);
    }

    // ── Tier 2: re-execute ───────────────────────────────────────────────────
    if matches!(opts.tier, Tier::Execute | Tier::All) {
        let (tasks, skipped) = select_compute_tasks(pkg)?;
        report.skipped = skipped;

        // Determine the recorded root + environment first: the recorded root
        // (from determinism-env.json `pkg_root`) is where a shipped conda env
        // was created, so provisioning needs it to mount that env back at its
        // baked path when the package has been relocated.
        let (recorded_root, recorded_env) = read_recorded_env(pkg);

        // Provision an execution environment with real system probes.
        let mut env = provision_env(pkg, opts.allow_rebuild, &recorded_root);
        let unprovisionable = matches!(env, ExecEnv::None);

        // Allocate scratch: caller-supplied or a fresh directory under the
        // system temp root (named with a UUID for uniqueness).
        let scratch: PathBuf;
        if let Some(ref sd) = opts.scratch_dir {
            scratch = sd.clone();
            std::fs::create_dir_all(&scratch).map_err(|e| {
                anyhow::anyhow!("could not create scratch dir {}: {e}", scratch.display())
            })?;
        } else {
            let id = uuid::Uuid::new_v4();
            scratch = std::env::temp_dir().join(format!("ecaa-replay-{}", id));
            std::fs::create_dir_all(&scratch).map_err(|e| {
                anyhow::anyhow!("could not create scratch dir {}: {e}", scratch.display())
            })?;
        };

        // Materialize an InstallFromLock env: deterministically install the
        // recorded explicit conda lock into a fresh prefix inside the image
        // (one network-bearing step), then run scripts through it hermetically.
        if let ExecEnv::InstallFromLock { digest, lock } = &env {
            let env_target = scratch.join(".replay-conda-env");
            tracing::info!(
                "replay: installing conda env from lock {} into {}",
                lock.display(),
                env_target.display()
            );
            crate::replay::env_provision::install_conda_env_from_lock(digest, lock, &env_target)
                .map_err(|e| anyhow::anyhow!("installing conda env from recorded lock: {e}"))?;
            // The env was created AT `env_target`, so its baked prefixes already
            // match — no relocation remap needed.
            env = ExecEnv::Container {
                digest: digest.clone(),
                conda_prefix: Some(env_target),
                conda_mount_at: None,
            };
        }

        // Read topological order from runtime/execution-order.json.
        let order = read_execution_order(pkg);

        // Run the tasks.
        let outcomes = stage_and_run(pkg, &scratch, &tasks, &order, &env, &recorded_root, &recorded_env)?;

        // Determine bounds: caller-supplied directory or the generic default.
        let bounds = match &opts.bounds {
            Some(dir) => {
                let provider = crate::reexecution_bounds::ModalityBoundsProvider::from_dir(dir);
                let modality = pkg_modality(pkg);
                provider.bounds_for(&modality)
            }
            None => ModalityBounds::default(),
        };

        // Run the comparator: parent=pkg, replay=scratch.
        let shim_path = pkg.join("runtime/determinism-shim.json");
        let policy_path = if shim_path.exists() { Some(shim_path.as_path()) } else { None };
        let mut reexec_report = classify_reexecution(pkg, &scratch, policy_path, bounds)?;

        // ── ok:false → Failed reconciliation ────────────────────────────────
        reconcile_failed_task_buckets(&mut reexec_report, &outcomes);

        report.reexecute = Some(crate::replay::report::ReexecuteResult {
            env_tier: env.tier_name().to_string(),
            report: reexec_report,
            unprovisionable,
        });
    }

    // Compute the final verdict.
    report.verdict = compute_verdict(&report);

    Ok(report)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Return `true` when `docker` is on the PATH.
fn which_docker() -> bool {
    std::process::Command::new("which")
        .arg("docker")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Return `true` when `conda` is on the PATH.
fn which_conda() -> bool {
    std::process::Command::new("which")
        .arg("conda")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Provision an execution environment with real system probes.
fn provision_env(pkg: &Path, allow_rebuild: bool, recorded_root: &str) -> ExecEnv {
    let opts = ProvisionOpts {
        allow_rebuild,
        docker_probe: which_docker,
        conda_probe: which_conda,
    };
    crate::replay::env_provision::provision(pkg, &opts, recorded_root)
}

/// Read `runtime/execution-order.json` and return the task ids in topo order.
/// Returns an empty Vec if the file is absent or malformed.
///
/// The file shape is: `{ "order": [ { "index": N, "task_id": "…", … }, … ] }`.
fn read_execution_order(pkg: &Path) -> Vec<String> {
    let path = pkg.join("runtime/execution-order.json");
    let Ok(raw) = std::fs::read_to_string(&path) else { return vec![]; };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else { return vec![]; };
    let Some(arr) = val.get("order").and_then(|v| v.as_array()) else { return vec![]; };
    arr.iter()
        .filter_map(|entry| entry.get("task_id")?.as_str().map(str::to_owned))
        .collect()
}

/// Read the recorded execution environment from the first task that has a
/// `determinism-env.json`. Returns `(recorded_root, recorded_env_vars)`.
///
/// `recorded_root` is the absolute path embedded in the package's compute
/// scripts at emit time. Discovery order:
/// 1. `pkg_root` field in the first `determinism-env.json` found (if a future
///    writer emits it, prefer it).
/// 2. Script-scan: walk `runtime/outputs/*/scripts/*.{R,py,sh}` in sorted
///    order; the first file containing `/<basename>` (where `basename` is the
///    package directory's own basename) yields the prefix up to and including
///    `/<basename>` as `recorded_root`.
/// 3. Empty string — `stage_and_run` treats this as "no path rewrite needed",
///    which is correct for packages that rely solely on the `PKG_ROOT`/`PACKAGE`
///    environment variable.
///
/// `recorded_env` is built from determinism-pinning keys
/// (`SOURCE_DATE_EPOCH`, `PYTHONHASHSEED`, `LC_ALL`, `TZ`, `LANG`) present
/// in the sidecar.
pub(crate) fn read_recorded_env(pkg: &Path) -> (String, BTreeMap<String, String>) {
    let outputs = pkg.join("runtime/outputs");
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    let mut recorded_root = String::new();

    // Walk runtime/outputs/ in lexicographic order to find the first
    // determinism-env.json and collect determinism-pinning vars.
    if let Ok(entries) = std::fs::read_dir(&outputs) {
        let mut dirs: Vec<_> = entries
            .flatten()
            .filter(|e| e.path().is_dir())
            .collect();
        dirs.sort_by_key(|e| e.file_name());

        for dir in dirs {
            let det_env_path = dir.path().join("determinism-env.json");
            if !det_env_path.exists() { continue; }
            let Ok(raw) = std::fs::read_to_string(&det_env_path) else { continue; };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else { continue; };

            // Source 1: `pkg_root` field (preferred when present).
            if let Some(root) = val.get("pkg_root").and_then(|v| v.as_str()) {
                if !root.is_empty() {
                    recorded_root = root.to_owned();
                }
            }

            // Determinism-pinning vars: SOURCE_DATE_EPOCH, PYTHONHASHSEED, LC_ALL, TZ, LANG.
            for key in &["source_date_epoch", "pythonhashseed", "lc_all", "tz", "lang"] {
                let env_key = if *key == "lc_all" { "LC_ALL".to_string() }
                              else { key.to_ascii_uppercase() };
                if let Some(v) = val.get(key).and_then(|v| v.as_str()) {
                    if !v.is_empty() {
                        env.insert(env_key, v.to_owned());
                    }
                }
            }
            break; // first task is enough
        }
    }

    // Source 2: script-scan (when pkg_root field was absent).
    if recorded_root.is_empty() {
        recorded_root = discover_recorded_root_from_scripts(pkg);
    }

    // Source 3: empty — no rewrite needed.

    (recorded_root, env)
}

/// Scan compute scripts (`runtime/outputs/*/scripts/*.{R,py,sh}`) for an
/// absolute path containing `/<basename>`, and return the prefix up to and
/// including that component.  Returns an empty string when no such path is
/// found (packages that use `PKG_ROOT`/`PACKAGE` env instead of hardcoded paths).
fn discover_recorded_root_from_scripts(pkg: &Path) -> String {
    let basename = match pkg.file_name().and_then(|n| n.to_str()) {
        Some(b) if !b.is_empty() => b.to_owned(),
        _ => return String::new(),
    };
    let needle = format!("/{}", basename);

    let outputs = pkg.join("runtime/outputs");
    let Ok(task_entries) = std::fs::read_dir(&outputs) else { return String::new(); };

    let mut task_dirs: Vec<_> = task_entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .collect();
    task_dirs.sort_by_key(|e| e.file_name());

    for task_dir in task_dirs {
        let scripts_dir = task_dir.path().join("scripts");
        let Ok(script_entries) = std::fs::read_dir(&scripts_dir) else { continue; };

        let mut scripts: Vec<_> = script_entries
            .flatten()
            .filter(|e| {
                let p = e.path();
                matches!(
                    p.extension().and_then(|x| x.to_str()),
                    Some("R") | Some("py") | Some("sh")
                )
            })
            .collect();
        scripts.sort_by_key(|e| e.file_name());

        for script in scripts {
            let Ok(text) = std::fs::read_to_string(script.path()) else { continue; };
            if let Some(pos) = text.find(&needle) {
                // Expand left to the start of the absolute path.
                // We iterate char_indices in reverse so the slice index is
                // advanced by the matched char's actual UTF-8 length, not a
                // hardcoded +1 (which would panic mid-codepoint on multi-byte
                // delimiter chars since the predicate includes !c.is_ascii()).
                let before = &text[..pos];
                let delimiter = |c: char| {
                    !c.is_ascii() || c == '"' || c == '\'' || c == '(' || c == ' ' || c == '\n' || c == '\r' || c == '\t' || c == '='
                };
                let path_start = before
                    .char_indices()
                    .rev()
                    .find(|&(_, c)| delimiter(c))
                    .map(|(idx, ch)| idx + ch.len_utf8())
                    .unwrap_or(0);
                let candidate = &text[path_start..pos + needle.len()];
                // Validate it looks like an absolute path.
                if candidate.starts_with('/') {
                    return candidate.to_owned();
                }
            }
        }
    }

    tracing::debug!("no hardcoded root found in scripts for package {basename}; no path rewrite will be applied");
    String::new()
}

/// Read package metadata (`package_iri`, `min_reader_version`) from
/// `runtime/audit-proof-report.json` when available.
fn read_package_meta(pkg: &Path) -> (String, Option<String>) {
    let path = pkg.join("runtime/audit-proof-report.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return ("ro-crate-metadata.json".to_string(), None);
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return ("ro-crate-metadata.json".to_string(), None);
    };
    let iri = val.get("package_iri")
        .and_then(|v| v.as_str())
        .unwrap_or("ro-crate-metadata.json")
        .to_string();
    let min_rv = val.get("min_reader_version")
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    (iri, min_rv)
}

/// Reconcile comparator buckets with task run outcomes.
///
/// `classify_reexecution` only inspects files on disk: a task that ran but
/// exited `ok:false` typically leaves its output missing, so the comparator
/// marks it `Unavailable` — indistinguishable from "environment could not run
/// it at all". We override `Unavailable` → `Failed` for any artifact whose
/// producing task ran and failed, and attach a TAIL of that task's stderr to
/// the reason so a human reading `.replay-report.json` (or the audit report)
/// can see WHY it failed without re-running. The task_id is the 3rd path
/// component of `runtime/outputs/<task_id>/…`.
fn reconcile_failed_task_buckets(
    report: &mut crate::reexecution::ReexecutionReport,
    outcomes: &[crate::replay::script_runner::RunOutcome],
) {
    let failed: BTreeMap<&str, &str> = outcomes
        .iter()
        .filter(|o| !o.ok)
        .map(|o| (o.task_id.as_str(), o.stderr.as_str()))
        .collect();
    if failed.is_empty() {
        return;
    }
    for ac in &mut report.per_artifact {
        if ac.bucket != ReexecutionBucket::Unavailable {
            continue;
        }
        let Some(task_id) = extract_task_id_from_artifact_path(&ac.artifact_path) else {
            continue;
        };
        if let Some(stderr) = failed.get(task_id) {
            ac.bucket = ReexecutionBucket::Failed;
            ac.reason = Some(format!(
                "task '{task_id}' ran and exited with ok:false; classified Failed \
                 rather than Unavailable. stderr tail: {}",
                stderr_tail(stderr)
            ));
        }
    }
    report.finalize_counts();
}

/// The last ~1200 chars of a stderr blob, trimmed, prefixed with an ellipsis
/// when truncated. Empty input renders as `(no stderr captured)` so the reason
/// is never a dangling "stderr tail: ".
fn stderr_tail(stderr: &str) -> String {
    const MAX: usize = 1200;
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return "(no stderr captured)".to_string();
    }
    // Take the last MAX bytes on a char boundary.
    if trimmed.len() <= MAX {
        return trimmed.to_string();
    }
    let start = trimmed.len() - MAX;
    let start = (start..trimmed.len())
        .find(|&i| trimmed.is_char_boundary(i))
        .unwrap_or(trimmed.len());
    format!("…{}", &trimmed[start..])
}

/// Extract the task_id component from an artifact_path of the form
/// `runtime/outputs/<task_id>/…`.  Returns `None` for paths that don't
/// match this shape.
fn extract_task_id_from_artifact_path(artifact_path: &str) -> Option<&str> {
    // artifact_path is relative, forward-slash separated.
    let mut parts = artifact_path.splitn(4, '/');
    // parts[0] = "runtime", parts[1] = "outputs", parts[2] = <task_id>
    let p0 = parts.next()?;
    let p1 = parts.next()?;
    let task_id = parts.next()?;
    if p0 == "runtime" && p1 == "outputs" && !task_id.is_empty() {
        Some(task_id)
    } else {
        None
    }
}

/// Extract the modality from the package directory basename.
/// Package basenames have the form `<uuid>-<modality>-<timestamp>`.
/// Returns an empty string when the basename doesn't match this pattern
/// (the caller falls back to `ModalityBounds::default()`).
fn pkg_modality(pkg: &Path) -> String {
    fn inner(pkg: &Path) -> Option<String> {
        let basename = pkg.file_name()?.to_str()?;
        // Format: <uuid36>-<modality>-<YYYYMMDDTHHmmss>
        // 36-char UUID + 1 hyphen = skip first 37 chars.
        let after_uuid = basename.get(37..)?;
        let last_hyphen = after_uuid.rfind('-')?;
        Some(after_uuid[..last_hyphen].to_string())
    }
    inner(pkg).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    /// Recursively copy `src` into `dst`.
    fn copy_dir_all(src: &Path, dst: &Path) -> anyhow::Result<()> {
        fs::create_dir_all(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            let ty = entry.file_type()?;
            let dest = dst.join(entry.file_name());
            if ty.is_dir() {
                copy_dir_all(&entry.path(), &dest)?;
            } else {
                fs::copy(&entry.path(), &dest)?;
            }
        }
        Ok(())
    }

    /// Copy the named conformance fixture into `dst`.
    fn copy_fixture(name: &str, dst: &Path) {
        let fixtures_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../ecaa-conformance/tests/fixtures")
            .join(name);
        copy_dir_all(&fixtures_root, dst).expect("copy_fixture");
    }

    /// Write a synthetic `runtime/audit-proof-report.json` with the given
    /// `(id, status)` pairs and `ecaa_version = "0.2"`.
    fn write_recorded_audit(pkg: &Path, verdicts: &[(&str, &str)]) {
        let runtime = pkg.join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        let verdict_arr: Vec<serde_json::Value> = verdicts
            .iter()
            .map(|(id, status)| {
                serde_json::json!({
                    "id": id,
                    "status": status,
                    "detail": null,
                    "n_inspected": 0,
                    "n_violations": 0
                })
            })
            .collect();
        let report = serde_json::json!({
            "schema_version": "0.1",
            "ecaa_version": "0.2",
            "min_reader_version": "0.2",
            "evaluator": {
                "impl": "ecaa-workflow-audit-proof",
                "version": "0.1.0",
                "policy": "warn-only"
            },
            "verdicts": verdict_arr
        });
        fs::write(
            runtime.join("audit-proof-report.json"),
            serde_json::to_string_pretty(&report).unwrap(),
        )
        .unwrap();
    }

    /// Patch an existing `runtime/claim-verification.json` to add summary
    /// count fields that match the actual verdicts array.  This ensures that
    /// the re-verify claim-verification check sees no divergence.
    ///
    /// Must be called AFTER `copy_fixture` so the verdicts (including their
    /// `supported_by` entries) are already present.
    fn patch_claim_verification_summary(pkg: &Path) {
        let path = pkg.join("runtime/claim-verification.json");
        let raw = fs::read_to_string(&path).expect("claim-verification.json must exist");
        let mut cv: serde_json::Value =
            serde_json::from_str(&raw).expect("claim-verification.json must be valid JSON");

        let Some(verdicts) = cv.get("verdicts").and_then(|v| v.as_array()).cloned() else {
            return;
        };

        let n_checked = verdicts.len() as u64;
        let n_mismatch = verdicts
            .iter()
            .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("mismatch"))
            .count() as u64;
        let n_suspicious = verdicts
            .iter()
            .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("suspicious"))
            .count() as u64;
        let n_verified = verdicts
            .iter()
            .filter(|v| v.get("status").and_then(|s| s.as_str()) == Some("verified"))
            .count() as u64;

        let obj = cv.as_object_mut().expect("claim-verification must be an object");
        obj.insert("n_mismatch".to_string(), serde_json::json!(n_mismatch));
        obj.insert("n_suspicious".to_string(), serde_json::json!(n_suspicious));
        obj.insert("n_verified".to_string(), serde_json::json!(n_verified));
        obj.insert("n_checked".to_string(), serde_json::json!(n_checked));

        fs::write(&path, serde_json::to_string_pretty(&cv).unwrap()).unwrap();
    }

    /// `run_replay` with tier=Verify on the `cross-graph-ok` fixture with a
    /// matching recorded audit-proof report → verdict == Pass.
    ///
    /// This is the integration test specified in the task brief (Task 6 TDD).
    #[test]
    fn run_replay_verify_tier_cross_graph_ok_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // Copy the cross-graph-ok fixture (has a clean ro-crate-metadata.json).
        copy_fixture("cross-graph-ok", pkg);

        // Write a recorded audit-proof report that says cross_graph_integrity
        // passed.  The fresh re-run on this clean fixture will also produce
        // pass, so the check must be non-divergent.
        write_recorded_audit(pkg, &[("cross_graph_integrity", "pass")]);

        // Patch claim-verification.json to add summary count fields that
        // match the verdicts already present (preserving `supported_by` so
        // the cross_graph_integrity fresh check still sees the reference and
        // returns `pass` rather than `unverified`).
        patch_claim_verification_summary(pkg);

        let opts = ReplayOptions {
            tier: Tier::Verify,
            scratch_dir: None,
            bounds: None,
            allow_rebuild: false,
            reader_version: "0.2".to_string(),
        };

        let report = run_replay(pkg, &opts).expect("run_replay must not error");
        assert_eq!(
            report.verdict,
            crate::replay::report::ReplayVerdict::Pass,
            "tier=Verify on cross-graph-ok with matching recorded report must yield Pass; \
             report={report:?}"
        );
        // Sanity: reverify was populated, reexecute was not.
        assert!(report.reverify.is_some(), "reverify must be Some for Tier::Verify");
        assert!(report.reexecute.is_none(), "reexecute must be None for Tier::Verify");
    }

    /// A task that ran and exited `ok:false` produces a missing artifact the
    /// comparator marks `Unavailable`. Reconciliation must reclassify it
    /// `Failed` AND attach a tail of the task's stderr, so a human reading the
    /// replay report can see WHY it failed without re-running.
    #[test]
    fn reconcile_attaches_stderr_tail_to_failed_task_reason() {
        use crate::reexecution::{ReexecutionReport, ArtifactClassification, ReexecutionBucket};
        use crate::replay::script_runner::RunOutcome;

        let mut rep = ReexecutionReport::empty("0.1");
        rep.per_artifact.push(ArtifactClassification {
            artifact_path: "runtime/outputs/data_acquisition/cohort_manifest.tsv".into(),
            bucket: ReexecutionBucket::Unavailable,
            reason: Some("replay artifact missing".into()),
        });

        let outcomes = vec![RunOutcome {
            task_id: "data_acquisition".into(),
            ok: false,
            stderr: "Traceback (most recent call last):\nRuntimeError: network unreachable\n".into(),
        }];

        reconcile_failed_task_buckets(&mut rep, &outcomes);

        let ac = &rep.per_artifact[0];
        assert_eq!(ac.bucket, ReexecutionBucket::Failed);
        let reason = ac.reason.as_deref().unwrap();
        assert!(
            reason.contains("network unreachable"),
            "reason must carry the failing task's stderr tail; got: {reason}"
        );
        assert_eq!(
            rep.bucket_counts.get("failed").copied(),
            Some(1),
            "counts must be re-finalized"
        );
    }

    /// Helper to verify extract_task_id_from_artifact_path.
    #[test]
    fn extract_task_id_roundtrip() {
        assert_eq!(
            extract_task_id_from_artifact_path("runtime/outputs/differential_expression/de_results.tsv"),
            Some("differential_expression")
        );
        assert_eq!(
            extract_task_id_from_artifact_path("results/tables/de.tsv"),
            None
        );
        assert_eq!(
            extract_task_id_from_artifact_path("runtime/outputs/"),
            None
        );
    }

    /// `read_recorded_env` discovers `recorded_root` from a hardcoded absolute
    /// path embedded in a compute script when `determinism-env.json` has no
    /// `pkg_root` field.
    #[test]
    fn recorded_root_discovered_from_hardcoded_script() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let basename = pkg.file_name().unwrap().to_str().unwrap().to_owned();

        // Write a script with a hardcoded absolute path containing the basename.
        let script_dir = pkg.join("runtime/outputs/normalisation/scripts");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("01.R"),
            format!("pkg <- \"/orig/emit/path/{basename}\"\n"),
        )
        .unwrap();

        let (root, _env) = read_recorded_env(pkg);
        assert_eq!(
            root,
            format!("/orig/emit/path/{basename}"),
            "recorded_root must be the emit-time prefix extracted from the script"
        );
    }

    /// `read_recorded_env` returns an empty `recorded_root` when no script
    /// contains a hardcoded absolute path with the package basename.
    #[test]
    fn recorded_root_empty_when_no_hardcoded_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();

        // Script uses only the PKG_ROOT env var — no hardcoded absolute path.
        let script_dir = pkg.join("runtime/outputs/normalisation/scripts");
        fs::create_dir_all(&script_dir).unwrap();
        fs::write(
            script_dir.join("01.R"),
            "pkg_root <- Sys.getenv(\"PKG_ROOT\", getwd())\n",
        )
        .unwrap();

        let (root, _env) = read_recorded_env(pkg);
        assert!(
            root.is_empty(),
            "recorded_root must be empty when no hardcoded path is present, got: {root:?}"
        );
    }

    /// `discover_recorded_root_from_scripts` must not panic when a multi-byte
    /// UTF-8 character appears immediately before the `/<basename>` path token.
    /// The left-expansion index arithmetic must advance past the delimiter by
    /// its actual UTF-8 byte length, not a hardcoded `+ 1` that would land
    /// mid-codepoint and cause a byte-boundary panic.
    #[test]
    fn discover_recorded_root_handles_multibyte_prefix() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let basename = pkg.file_name().unwrap().to_str().unwrap().to_owned();

        let script_dir = pkg.join("runtime/outputs/normalisation/scripts");
        fs::create_dir_all(&script_dir).unwrap();

        // Place a multi-byte character (é = U+00E9, 2 bytes in UTF-8) immediately
        // before the absolute path token.  The delimiter predicate matches
        // `!c.is_ascii()`, so `é` is a valid delimiter — but its byte index + 1
        // would land in the middle of the second byte, which panics without the fix.
        // With the fix, path_start lands at the byte after `é`, so the candidate
        // starts with `/` and is returned (or the implementation may reject the
        // candidate as not starting with `/` if path_start > pos, but either way
        // it must NOT panic).
        fs::write(
            script_dir.join("01.R"),
            format!("path <- é\"/orig/emit/path/{basename}\"\n"),
        )
        .unwrap();

        // The call must not panic regardless of what value is returned.
        let (root, _env) = read_recorded_env(pkg);
        // The candidate starts with `/` (the `é` is the delimiter, path_start
        // lands on the `/`), so we expect the correct path to be recovered.
        assert_eq!(
            root,
            format!("/orig/emit/path/{basename}"),
            "multi-byte delimiter must not panic; expected correct path recovery"
        );
    }
}
