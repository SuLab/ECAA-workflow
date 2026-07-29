// crates/core/src/replay/select.rs
//
// Deterministic compute-task selector for the replay path.
//
// Given a package directory, identifies which tasks are deterministic compute
// (eligible for re-execution) and which to skip with a reason.

use crate::replay::report::SkippedStage;
use std::path::{Path, PathBuf};

/// A task eligible for deterministic re-execution.
pub struct ComputeTask {
    pub task_id: String,
    pub scripts_dir: PathBuf,
    pub result_tables: Vec<String>,
}

/// Literature-retrieval task-id prefixes: a SECONDARY exclusion net behind the
/// primary [`stage_requires_egress`] capability check, for packages whose
/// recorded task-spec omits/malforms its network policy (older, hand-authored,
/// or mangled deposits) — `stage_requires_egress` fails OPEN there, so this
/// name net guarantees a literature fetcher is never run offline. Does NOT
/// include `discover_*` (hermetic — they score from the staged survey output)
/// nor the `validate_*` companions, which are meant to run.
const LITERATURE_PREFIX: &[&str] = &["review_", "survey_", "contextualize_"];

/// Returns `Some(reason)` when a stage should be EXCLUDED from re-execution.
///
/// Capability predicate: a stage is offline-reproducible (eligible to run)
/// UNLESS ANY of the following holds:
/// 1. It is declared non-deterministic in `runtime/determinism-shim.json`.
/// 2. It is the `data_acquisition` ingestion stage (a name-based exact-match):
///    it reads an external data source a hermetic offline replay cannot reach.
///    (The atom now also declares `Bridge`, so a fresh emit is caught by rule 3
///    too; this exact-match keeps older deposits — whose emitted policy is a
///    stale `none{[]}` — correct.)
/// 3. PRIMARY: its recorded network policy requires egress (see
///    [`stage_requires_egress`]) — well-formed `review_*`/`survey_*`/
///    `contextualize_*` carry non-empty host allowlists and are caught here with
///    an accurate reason.
/// 4. SECONDARY belt-and-suspenders: it matches [`LITERATURE_PREFIX`] (catches a
///    literature fetcher even when rule 3 fails open on a missing/malformed
///    task-spec). Fully-isolated stages (`discover_*`, `reporting`,
///    `final_reporting`, `validate_*`: `kind none`, EMPTY allowlist) match none
///    of these rules and therefore run.
fn is_excluded(pkg: &Path, id: &str, shim_excludes: &[String]) -> Option<&'static str> {
    if shim_excludes.iter().any(|s| s == id) {
        return Some("declared non-deterministic in determinism-shim.json");
    }
    // Data ingestion reads the original external source (a host path / public
    // repository outside the package); an offline hermetic replay cannot reach
    // it, so a re-run cannot reproduce it. Its recorded output is NOT
    // asserted-by-copy: `run_replay::unstage_non_run_outputs` removes the staged
    // copy before the comparator runs, so its tables classify Unavailable.
    if id == "data_acquisition" {
        return Some("data-ingestion stage (external source not reproducible offline)");
    }
    // PRIMARY capability check.
    if stage_requires_egress(pkg, id) {
        return Some(
            "network egress required by recorded safety policy (not offline-reproducible)",
        );
    }
    // SECONDARY belt-and-suspenders net for literature fetchers whose recorded
    // task-spec is missing/malformed (rule 3 fails open there). Does not match
    // `discover_*` (hermetic) or the `validate_*` companions.
    if LITERATURE_PREFIX.iter().any(|p| id.starts_with(p)) {
        return Some("literature-retrieval stage (network egress by construction)");
    }
    None
}

/// Read `runtime/outputs/<id>/task-spec.json` and decide whether the stage's
/// recorded network policy requires egress.
///
/// The serialized `Task.safety.network` is an internally-tagged enum:
/// `{"kind":"none","allowlist":[...]}` or `{"kind":"bridge"}`. A stage is
/// offline-reproducible on network grounds ONLY when `kind == "none"` AND the
/// allowlist is empty; `kind == "bridge"` OR a non-empty allowlist means egress
/// is required.
///
/// Returns `false` (does NOT require egress) when the task-spec is absent,
/// unreadable, unparseable, or lacks a recognizable network policy — the
/// explicit external-source set is relied on in that case.
fn stage_requires_egress(pkg: &Path, id: &str) -> bool {
    let spec_path = pkg.join("runtime/outputs").join(id).join("task-spec.json");
    let Ok(raw) = std::fs::read_to_string(&spec_path) else {
        return false;
    };
    let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    let Some(net) = val.get("safety").and_then(|s| s.get("network")) else {
        return false;
    };
    match net.get("kind").and_then(|k| k.as_str()) {
        // Offline-reproducible iff the allowlist is empty; a non-empty
        // allowlist means the entrypoint resolves egress hosts at launch.
        Some("none") => net
            .get("allowlist")
            .and_then(|a| a.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false),
        Some("bridge") => true,
        // Unknown/absent kind — do not exclude on network grounds.
        _ => false,
    }
}

/// Select deterministic compute tasks from a downloaded ECAA package.
///
/// A stage is selected (run-eligible) when BOTH of the following hold:
/// - It is not excluded by the capability predicate ([`is_excluded`]): not
///   shim-declared non-deterministic, not the `data_acquisition` exact-match,
///   and its recorded network policy does not require egress.
/// - `runtime/outputs/<id>/scripts/` exists and contains ≥1 `.R`/`.py`/`.sh`
///   file.
///
/// A run-eligible stage is returned as a `ComputeTask` EVEN IF it has zero
/// `.tsv`/`.csv` result tables (reporting/validate stages produce
/// figures/reports/markdown, not tables); its `result_tables` is then empty.
/// The downstream comparator derives its own table list from disk, so an empty
/// `result_tables` is harmless.
///
/// Excluded stages → `SkippedStage` with the exclusion reason. A non-excluded
/// stage that has NO runnable script → `SkippedStage` with reason
/// "no runnable script" (it can never be re-executed). A run-eligible stage is
/// NEVER skipped merely for lacking a result table.
///
/// Results are returned in deterministic (lexicographic) order.
pub fn select_compute_tasks(pkg: &Path) -> std::io::Result<(Vec<ComputeTask>, Vec<SkippedStage>)> {
    // Read optional determinism shim.
    let shim_excludes: Vec<String> =
        std::fs::read_to_string(pkg.join("runtime/determinism-shim.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("non_deterministic_stages").cloned())
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

    let outputs = pkg.join("runtime/outputs");
    let mut sel: Vec<ComputeTask> = vec![];
    let mut skipped: Vec<SkippedStage> = vec![];

    if !outputs.is_dir() {
        return Ok((sel, skipped));
    }

    // Collect + sort for deterministic ordering.
    let mut dirs: Vec<_> = std::fs::read_dir(&outputs)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    dirs.sort_by_key(|e| e.file_name());

    for e in dirs {
        let id = e.file_name().to_string_lossy().to_string();
        let task_path = e.path();
        let scripts = task_path.join("scripts");

        // Capability exclusion first, so an excluded stage always carries its
        // exclusion reason (independent of whether it happens to have a script).
        if let Some(reason) = is_excluded(pkg, &id, &shim_excludes) {
            skipped.push(SkippedStage {
                task: id,
                reason: reason.into(),
            });
            continue;
        }

        // Run-eligibility requires ≥1 runnable `.R`/`.py`/`.sh` script.
        let has_script = scripts.is_dir()
            && std::fs::read_dir(&scripts)
                .map(|mut r| {
                    r.any(|f| {
                        f.as_ref()
                            .map(|f| {
                                let n = f.file_name();
                                let n = n.to_string_lossy();
                                n.ends_with(".R") || n.ends_with(".py") || n.ends_with(".sh")
                            })
                            .unwrap_or(false)
                    })
                })
                .unwrap_or(false);

        if !has_script {
            // A non-excluded stage with no runnable script can never be
            // re-executed — record it explicitly rather than silently dropping.
            skipped.push(SkippedStage {
                task: id,
                reason: "no runnable script".into(),
            });
            continue;
        }

        // Gather result tables directly under the stage dir. This may be empty
        // (reporting/validate stages emit figures/reports, not tables) — a
        // run-eligible stage is STILL returned and run in that case. We
        // propagate a `read_dir` error via `?` so an unreadable outputs tree
        // surfaces rather than being silently treated as "no table".
        let mut tables: Vec<String> = std::fs::read_dir(&task_path)?
            .filter_map(|f| f.ok())
            .map(|f| f.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".tsv") || n.ends_with(".csv"))
            .collect();

        // Sort for reproducible downstream iteration.
        tables.sort();

        sel.push(ComputeTask {
            task_id: id,
            scripts_dir: scripts,
            result_tables: tables,
        });
    }

    Ok((sel, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: create a task directory under `root/runtime/outputs/<id>`.
    /// If `script` is true, adds `scripts/01.R`. If `table` is Some, writes
    /// that filename directly under the task dir.
    fn mk(root: &Path, id: &str, script: bool, table: Option<&str>) {
        let d = root.join("runtime/outputs").join(id);
        if script {
            std::fs::create_dir_all(d.join("scripts")).unwrap();
            std::fs::write(d.join("scripts/01.R"), "1\n").unwrap();
        }
        std::fs::create_dir_all(&d).unwrap();
        if let Some(t) = table {
            std::fs::write(d.join(t), "a\tb\n").unwrap();
        }
    }

    /// Write a `runtime/outputs/<id>/task-spec.json` declaring the serialized
    /// `Task.safety.network` policy. `kind` is `"none"` or `"bridge"`; for
    /// `"none"` the `allowlist` is written verbatim.
    fn write_task_spec_net(root: &Path, id: &str, kind: &str, allowlist: &[&str]) {
        let d = root.join("runtime/outputs").join(id);
        std::fs::create_dir_all(&d).unwrap();
        let network = if kind == "bridge" {
            serde_json::json!({ "kind": "bridge" })
        } else {
            serde_json::json!({ "kind": "none", "allowlist": allowlist })
        };
        let spec = serde_json::json!({
            "safety": {
                "level": "compute",
                "network": network,
                "code_execution": "none",
                "sandbox": "none",
                "provisioning": "declared_only"
            }
        });
        std::fs::write(
            d.join("task-spec.json"),
            serde_json::to_string_pretty(&spec).unwrap(),
        )
        .unwrap();
    }

    /// Isolated (offline-reproducible) task-spec: `kind == "none"`, empty allowlist.
    fn write_isolated_spec(root: &Path, id: &str) {
        write_task_spec_net(root, id, "none", &[]);
    }

    /// Reporting, final_reporting, and a `validate_*` stage are now RUN-eligible
    /// (capability-based selector) when their recorded network policy is
    /// offline-reproducible (kind none, empty allowlist) and a script exists —
    /// even though they produce figures/reports/markdown, not `.tsv`/`.csv`
    /// tables.
    #[test]
    fn selects_reporting_final_reporting_and_validate_when_isolated() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        for id in ["reporting", "final_reporting", "validate_normalisation"] {
            mk(root, id, true, None); // script present, NO result table
            write_isolated_spec(root, id);
        }

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        assert!(
            sel_ids.contains(&"reporting"),
            "reporting must now be selected; skipped={skipped:?}"
        );
        assert!(
            sel_ids.contains(&"final_reporting"),
            "final_reporting must now be selected; skipped={skipped:?}"
        );
        assert!(
            sel_ids.contains(&"validate_normalisation"),
            "validate_* must now be selected; skipped={skipped:?}"
        );
        // A run-eligible stage with no table carries an empty result_tables.
        let rep = sel.iter().find(|t| t.task_id == "reporting").unwrap();
        assert!(
            rep.result_tables.is_empty(),
            "reporting produces no table → empty result_tables"
        );
    }

    /// The name-based exclusion is now a SINGLE exact-match: `data_acquisition`
    /// (excluded even with an isolated spec, since it reads an external source).
    /// Everything else is decided by the network-egress capability check:
    /// literature stages (`review_*`/`survey_*`/`contextualize_*`) carry
    /// non-empty allowlists → excluded; a fully-isolated `discover_*` stage is
    /// hermetic → SELECTED (it is NOT excluded by name).
    #[test]
    fn excludes_data_acquisition_and_egress_stages_but_runs_isolated_discover() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // data_acquisition: isolated spec, but still excluded by the exact-match.
        mk(root, "data_acquisition", true, Some("cohort.tsv"));
        write_isolated_spec(root, "data_acquisition");

        // Literature stages carry a real 9-host allowlist → excluded (egress).
        let hosts = [
            "eutils.ncbi.nlm.nih.gov",
            "api.openalex.org",
            "api.crossref.org",
        ];
        for id in [
            "review_prior_work",
            "survey_method_landscape",
            "contextualize_findings_with_literature",
        ] {
            mk(root, id, true, Some("t.tsv"));
            write_task_spec_net(root, id, "none", &hosts);
        }

        // Fully-isolated hermetic stages → SELECTED (run).
        for id in ["discover_normalisation", "differential_expression"] {
            mk(root, id, true, Some("out.tsv"));
            write_isolated_spec(root, id);
        }

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let mut sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        sel_ids.sort();
        assert_eq!(
            sel_ids,
            ["differential_expression", "discover_normalisation"],
            "isolated discover_* and compute stages run; sel={sel_ids:?}"
        );
        let sk: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        for id in [
            "data_acquisition",
            "review_prior_work",
            "survey_method_landscape",
            "contextualize_findings_with_literature",
        ] {
            assert!(sk.contains(&id), "{id} must be excluded; skipped={sk:?}");
        }
    }

    /// A stage whose recorded network policy requires egress — `kind: bridge`
    /// OR a non-empty allowlist — is excluded (cannot reproduce offline). Only
    /// `kind: none` with an empty allowlist is offline-reproducible.
    #[test]
    fn excludes_stage_that_needs_egress() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Bridge → egress → excluded.
        mk(root, "compute_bridge", true, Some("t.tsv"));
        write_task_spec_net(root, "compute_bridge", "bridge", &[]);
        // kind none + non-empty allowlist → egress → excluded.
        mk(root, "compute_allowlist", true, Some("t.tsv"));
        write_task_spec_net(root, "compute_allowlist", "none", &["api.example.org"]);
        // kind none + empty allowlist → offline → selected.
        mk(root, "compute_isolated", true, Some("t.tsv"));
        write_isolated_spec(root, "compute_isolated");

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(
            sel_ids,
            ["compute_isolated"],
            "only the isolated stage runs; sel={sel_ids:?}"
        );
        let sk: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        assert!(
            sk.contains(&"compute_bridge"),
            "bridge stage must be excluded"
        );
        assert!(
            sk.contains(&"compute_allowlist"),
            "non-empty-allowlist stage must be excluded"
        );
    }

    /// SECONDARY belt-and-suspenders net: a literature fetcher whose recorded
    /// task-spec is ABSENT (older / hand-authored / mangled deposit) makes the
    /// primary `stage_requires_egress` check fail OPEN, so the `LITERATURE_PREFIX`
    /// name net must still exclude it — a literature stage must never be run
    /// offline. A hermetic `discover_*` stage with no task-spec still runs.
    #[test]
    fn excludes_literature_stage_without_task_spec_via_secondary_net() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // Literature fetchers: script + table, but NO task-spec written.
        mk(
            root,
            "review_prior_work",
            true,
            Some("prior_claims_matrix.csv"),
        );
        mk(
            root,
            "survey_method_landscape",
            true,
            Some("method_landscape.csv"),
        );
        mk(
            root,
            "contextualize_findings_with_literature",
            true,
            Some("claims.csv"),
        );
        // Hermetic discover_ with no task-spec must still be selected.
        mk(root, "discover_normalisation", true, Some("score.tsv"));

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        let sk: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        for lit in [
            "review_prior_work",
            "survey_method_landscape",
            "contextualize_findings_with_literature",
        ] {
            assert!(
                sk.contains(&lit),
                "{lit} must be excluded via the literature secondary net even with no task-spec; sel={sel_ids:?}"
            );
        }
        assert!(
            sel_ids.contains(&"discover_normalisation"),
            "hermetic discover_ must still run without a task-spec; sel={sel_ids:?}"
        );
    }

    /// A run-eligible, non-excluded stage that has a script but ZERO result
    /// tables must be RETURNED as a ComputeTask (with empty `result_tables`),
    /// NOT skipped. Reporting/validate stages produce figures/reports, not
    /// tables; the downstream comparator derives its table list from disk, so
    /// an empty `result_tables` is fine.
    #[test]
    fn run_eligible_stage_without_tables_is_selected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "compute_something", true, None); // script, no table
        write_isolated_spec(root, "compute_something");

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        assert_eq!(
            sel_ids,
            ["compute_something"],
            "run-eligible zero-table stage must be selected, not skipped; skipped={skipped:?}"
        );
        assert!(
            sel[0].result_tables.is_empty(),
            "zero-table stage has empty result_tables"
        );
        assert!(
            !skipped.iter().any(|s| s.task == "compute_something"),
            "must not appear in skipped"
        );
    }

    /// `data_acquisition` is a data-INGESTION stage: its script reads the
    /// original external SME inputs (a host path outside the package) and
    /// stages them in. Offline replay cannot reproduce that — the source is
    /// absent and not mounted into the hermetic container — so it must be
    /// SKIPPED, not run (running it fails with FileNotFoundError and
    /// spuriously marks the package's re-execution FAILED). Its recorded output
    /// is NOT asserted-by-copy: `run_replay::unstage_non_run_outputs` removes the
    /// staged skipped-stage dir before comparison, so its tables classify
    /// `Unavailable` (honest — not re-executed), not `ByteIdentical`.
    #[test]
    fn excludes_data_acquisition_ingestion_stage() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(root, "data_acquisition", true, Some("cohort_manifest.tsv"));
        mk(
            root,
            "differential_expression",
            true,
            Some("de_results.tsv"),
        );

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert_eq!(
            sel.iter().map(|t| t.task_id.as_str()).collect::<Vec<_>>(),
            ["differential_expression"],
            "data_acquisition must not be selected for re-execution"
        );
        let da = skipped
            .iter()
            .find(|s| s.task == "data_acquisition")
            .expect("data_acquisition must be skipped");
        assert!(
            da.reason.contains("ingestion"),
            "skip reason should identify it as a data-ingestion stage; got: '{}'",
            da.reason
        );
    }

    /// Capability-based selection: `validate_*` and reporting stages now RUN
    /// (no name-based exclusion, and — lacking an egress task-spec here — not
    /// excluded on network grounds). The literature `contextualize_*` stage is
    /// excluded ONLY because its recorded task-spec declares a non-empty host
    /// allowlist (egress), NOT because of its name.
    #[test]
    fn selects_compute_and_validate_and_reporting_excludes_literature() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(
            root,
            "differential_expression",
            true,
            Some("de_results.tsv"),
        );
        mk(
            root,
            "validate_differential_expression",
            true,
            Some("checks.tsv"),
        );
        mk(
            root,
            "contextualize_findings_with_literature",
            true,
            Some("matrix.csv"),
        );
        // The literature stage is excluded by its egress allowlist, not its name.
        write_task_spec_net(
            root,
            "contextualize_findings_with_literature",
            "none",
            &["api.openalex.org", "eutils.ncbi.nlm.nih.gov"],
        );
        mk(root, "reporting", true, None);

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        let sel_ids: Vec<_> = sel.iter().map(|t| t.task_id.as_str()).collect();
        // differential_expression, reporting, validate_differential_expression
        // are all run-eligible; contextualize_* is excluded.
        assert!(
            sel_ids.contains(&"differential_expression"),
            "sel={sel_ids:?}"
        );
        assert!(
            sel_ids.contains(&"validate_differential_expression"),
            "sel={sel_ids:?}"
        );
        assert!(sel_ids.contains(&"reporting"), "sel={sel_ids:?}");
        assert!(
            !sel_ids.contains(&"contextualize_findings_with_literature"),
            "literature stage must not be selected; sel={sel_ids:?}"
        );

        let sk_ids: Vec<_> = skipped.iter().map(|s| s.task.as_str()).collect();
        assert!(
            sk_ids.contains(&"contextualize_findings_with_literature"),
            "literature stage must be excluded; skipped={sk_ids:?}"
        );
        assert!(
            !sk_ids.contains(&"validate_differential_expression"),
            "validate_* must no longer be excluded; skipped={sk_ids:?}"
        );
        assert!(
            !sk_ids.contains(&"reporting"),
            "reporting must no longer be excluded; skipped={sk_ids:?}"
        );
    }

    // Finding 1: shim exclusion path must be exercised.
    // Creates a task that would otherwise be selected (script + de_results.tsv)
    // and declares it in a determinism-shim.json; asserts it is excluded with
    // a reason that mentions the shim file.
    #[test]
    fn shim_exclusion_takes_priority() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mk(
            root,
            "differential_expression",
            true,
            Some("de_results.tsv"),
        );

        // Write the shim declaring differential_expression non-deterministic.
        let shim_dir = root.join("runtime");
        std::fs::create_dir_all(&shim_dir).unwrap();
        std::fs::write(
            shim_dir.join("determinism-shim.json"),
            r#"{"non_deterministic_stages":["differential_expression"]}"#,
        )
        .unwrap();

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert!(sel.is_empty(), "shim-excluded task must not be selected");
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].task, "differential_expression");
        assert!(
            skipped[0].reason.contains("determinism-shim"),
            "skip reason should mention the shim file, got: '{}'",
            skipped[0].reason
        );
    }

    /// A stage that has NO runnable script is skipped with a "no runnable
    /// script" reason (it can never be re-executed). This is the only
    /// table-independent skip reason for a non-excluded stage — a run-eligible
    /// stage is NEVER skipped merely for lacking a table.
    #[test]
    fn stage_without_script_is_skipped_no_runnable_script() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        // A task dir with a table but NO scripts/ subdir.
        let d = root.join("runtime/outputs/data_only_stage");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("results.tsv"), "x\n").unwrap();
        write_isolated_spec(root, "data_only_stage");

        let (sel, skipped) = select_compute_tasks(root).unwrap();
        assert!(sel.is_empty(), "no-script stage must not be selected");
        let sk = skipped
            .iter()
            .find(|s| s.task == "data_only_stage")
            .expect("no-script stage must be skipped");
        assert_eq!(
            sk.reason, "no runnable script",
            "reason should be 'no runnable script', got '{}'",
            sk.reason
        );
    }
}
