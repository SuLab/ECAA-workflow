//! Harness-side
//! validator orchestration.
//!
//! When a task completes, the harness consults the task's
//! `ValidationBundle` (loaded from `validation_obligations.rs` in
//! crates/core) and runs every obligation against the task's
//! emitted artifacts. Validators are sync, side-effect-free
//! functions that return a typed `ValidatorResult`; the harness
//! aggregates the results into a `ValidationReport` that the
//! verify endpoint surfaces.
//!
//! The trait + a starter implementation (`p_value_in_unit_interval`) are
//! included here; additional obligations (gene_id_in_annotation,
//! coordinate_in_contig, barcode_matrix_dim_consistency,
//! no_train_test_leakage, deterministic_or_bounded_variance) are wired
//! in as the harness grows file-shape-aware tooling.
//!
//! Failure modes:
//! - Validator crashes (panics, IO errors): treated as
//!   `ValidatorOutcome::Errored` so the harness can still aggregate
//!   the surviving validators' results without blocking the entire
//!   bundle.
//! - Validator returns `ValidatorOutcome::Failed`: the harness
//!   transitions the task to `Blocked { ValidationFailed }`,
//!   matching the existing `claim_extractor` / `claim_verifier` path.

use std::path::Path;

/// Outcome of running a single validator over a task's artifacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorOutcome {
    /// Validator passed; no issues found.
    Passed,
    /// Validator found a violation. Carries a typed message the
    /// UI / verify endpoint surfaces.
    Failed {
        /// Human-readable violation message surfaced to the SME.
        message: String,
    },
    /// Validator could not run (file missing, parse error). Treated
    /// as soft-skip rather than hard fail; the harness reports the
    /// reason and continues with the surviving validators.
    Errored {
        /// Reason the validator could not execute (e.g. missing file).
        reason: String,
    },
    /// Obligation is not implemented by this harness build.
    /// Recorded so the validation report names which obligations weren't run.
    Unimplemented {
        /// Obligation id that has no runner registered.
        obligation_id: String,
    },
}

/// One row in the per-task ValidationReport emitted by the
/// harness. Surfaces in `runtime/validation-reports.jsonl` and feeds
/// the UI's validation-status card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorRow {
    /// Stable obligation id string (e.g. "pmid_resolves").
    pub obligation_id: String,
    /// Outcome of running this obligation.
    pub outcome: ValidatorOutcome,
}

/// Pluggable validator. Each implementation owns one obligation id;
/// the harness routes obligations to the right runner via
/// `obligation_id` lookup. The function takes the artifact path
/// rather than a deserialized struct so the runner can pick its own
/// parser (no shared parser dependency between validators).
pub trait ValidatorRunner: Send + Sync {
    /// Stable obligation id this runner implements.
    fn obligation_id(&self) -> &'static str;
    /// Run the obligation against the given artifact path. The
    /// path is the task's `result_ref` directory (per
    /// crates/core/src/dag.rs). Validators inspect specific files
    /// inside.
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome;
}

/// Validator that asserts every adjusted p-value emitted by the task
/// lives in `[0, 1]`. Reads
/// `<artifact_path>/result.json` and inspects any `padj` /
/// `adjusted_pvalue` / `q_value` fields. Soft-skips when the file
/// is absent so the validator doesn't block tasks that don't
/// produce a result.json.
pub struct PValueInUnitIntervalRunner;

impl ValidatorRunner for PValueInUnitIntervalRunner {
    fn obligation_id(&self) -> &'static str {
        "p_value_in_unit_interval"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let path = artifact_path.join("result.json");
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return ValidatorOutcome::Errored {
                    reason: format!("result.json missing at {}", path.display()),
                };
            }
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("read error at {}: {}", path.display(), e),
                };
            }
        };
        let value: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                return ValidatorOutcome::Errored {
                    reason: format!("result.json parse error: {}", e),
                };
            }
        };
        // Walk the JSON looking for any field named padj /
        // adjusted_pvalue / q_value with a numeric value out of
        // range.
        let mut bad: Vec<(String, f64)> = Vec::new();
        walk_for_pvalues(&value, "", &mut bad);
        if bad.is_empty() {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: format!(
                    "p-value out of [0, 1]: {}",
                    bad.iter()
                        .take(3)
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    }
}

fn walk_for_pvalues(value: &serde_json::Value, path: &str, out: &mut Vec<(String, f64)>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let next_path = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                if matches!(k.as_str(), "padj" | "adjusted_pvalue" | "q_value") {
                    if let Some(n) = v.as_f64() {
                        if !(0.0..=1.0).contains(&n) {
                            out.push((next_path.clone(), n));
                        }
                    }
                } else {
                    walk_for_pvalues(v, &next_path, out);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, v) in arr.iter().enumerate() {
                let next_path = format!("{path}[{i}]");
                walk_for_pvalues(v, &next_path, out);
            }
        }
        _ => {}
    }
}

/// Run a list of obligation ids against the task's artifacts using
/// the supplied runner registry. Obligations with no matching
/// runner produce `ValidatorOutcome::Unimplemented` rows so the
/// report names what wasn't run.
pub fn run_validators(
    obligations: &[String],
    runners: &[Box<dyn ValidatorRunner>],
    artifact_path: &Path,
) -> Vec<ValidatorRow> {
    obligations
        .iter()
        .map(|id| {
            let runner = runners.iter().find(|r| r.obligation_id() == id);
            let outcome = match runner {
                Some(r) => r.run(artifact_path),
                None => ValidatorOutcome::Unimplemented {
                    obligation_id: id.clone(),
                },
            };
            ValidatorRow {
                obligation_id: id.clone(),
                outcome,
            }
        })
        .collect()
}

/// `gene_id_in_annotation`. Reads
/// `<artifact_path>/result.json::genes` (a JSON array of gene id
/// strings) and `<artifact_path>/annotation_index.json` (the gene
/// annotation index emitted by the upstream annotation task) and
/// asserts every emitted gene id is in the annotation index. Errors
/// when either file is missing or unparseable.
pub struct GeneIdInAnnotationRunner;

impl ValidatorRunner for GeneIdInAnnotationRunner {
    fn obligation_id(&self) -> &'static str {
        "gene_id_in_annotation"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let results = match read_json(&artifact_path.join("result.json")) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let annotation = match read_json(&artifact_path.join("annotation_index.json")) {
            Ok(v) => v,
            Err(_e) => {
                // Soft-skip: tasks without annotation index can't be
                // validated.
                return ValidatorOutcome::Errored {
                    reason: "annotation_index.json not present in artifact dir".into(),
                };
            }
        };
        let annotated: std::collections::BTreeSet<String> = match &annotation {
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect(),
            serde_json::Value::Object(obj) => obj.keys().cloned().collect(),
            _ => {
                return ValidatorOutcome::Errored {
                    reason: "annotation_index.json must be array or object".into(),
                }
            }
        };
        let mut missing: Vec<String> = Vec::new();
        let genes = results
            .get("genes")
            .or_else(|| results.get("gene_ids"))
            .and_then(|v| v.as_array());
        if let Some(arr) = genes {
            for v in arr {
                if let Some(g) = v.as_str() {
                    if !annotated.contains(g) {
                        missing.push(g.to_string());
                    }
                }
            }
        }
        if missing.is_empty() {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: format!(
                    "{} gene id(s) not in annotation: {}",
                    missing.len(),
                    missing
                        .iter()
                        .take(5)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    }
}

/// `coordinate_in_contig`. Reads
/// `<artifact_path>/result.json::variants` (a JSON array of
/// `{contig, pos}` records) and `contigs.json` (a list of
/// `{name, length}` records). Asserts every position falls within
/// the named contig's length.
pub struct CoordinateInContigRunner;

impl ValidatorRunner for CoordinateInContigRunner {
    fn obligation_id(&self) -> &'static str {
        "coordinate_in_contig"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let results = match read_json(&artifact_path.join("result.json")) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let contigs = match read_json(&artifact_path.join("contigs.json")) {
            Ok(v) => v,
            Err(_) => {
                return ValidatorOutcome::Errored {
                    reason: "contigs.json not present in artifact dir".into(),
                }
            }
        };
        let lengths: std::collections::BTreeMap<String, u64> = match &contigs {
            serde_json::Value::Array(arr) => arr
                .iter()
                .filter_map(|v| {
                    let name = v.get("name")?.as_str()?.to_string();
                    let length = v.get("length")?.as_u64()?;
                    Some((name, length))
                })
                .collect(),
            _ => {
                return ValidatorOutcome::Errored {
                    reason: "contigs.json must be an array of {name, length}".into(),
                }
            }
        };
        let variants = results
            .get("variants")
            .or_else(|| results.get("records"))
            .and_then(|v| v.as_array());
        let Some(arr) = variants else {
            return ValidatorOutcome::Passed;
        };
        let mut bad: Vec<String> = Vec::new();
        for v in arr {
            let Some(contig) = v.get("contig").and_then(|c| c.as_str()) else {
                continue;
            };
            let Some(pos) = v.get("pos").and_then(|p| p.as_u64()) else {
                continue;
            };
            match lengths.get(contig) {
                Some(&len) if pos > 0 && pos <= len => {}
                Some(&len) => bad.push(format!("{contig}:{pos} > length {len}")),
                None => bad.push(format!("unknown contig {contig}:{pos}")),
            }
        }
        if bad.is_empty() {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: format!(
                    "{} coordinate violation(s): {}",
                    bad.len(),
                    bad.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
                ),
            }
        }
    }
}

/// `barcode_matrix_dim_consistency`.
/// Reads `<artifact_path>/result.json::matrix_shape` and
/// `<artifact_path>/result.json::n_barcodes` and asserts the matrix
/// row count equals the barcode count (or, for column-major matrices,
/// `n_features` equals the gene count).
pub struct CellBarcodeMatrixDimensionConsistencyRunner;

impl ValidatorRunner for CellBarcodeMatrixDimensionConsistencyRunner {
    fn obligation_id(&self) -> &'static str {
        "barcode_matrix_dim_consistency"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let results = match read_json(&artifact_path.join("result.json")) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let shape = results
            .get("matrix_shape")
            .and_then(|v| v.as_array())
            .map(|arr| {
                let rows = arr.first().and_then(|v| v.as_u64()).unwrap_or(0);
                let cols = arr.get(1).and_then(|v| v.as_u64()).unwrap_or(0);
                (rows, cols)
            });
        let barcodes = results.get("n_barcodes").and_then(|v| v.as_u64());
        let features = results.get("n_features").and_then(|v| v.as_u64());
        let layout = results
            .get("matrix_layout")
            .and_then(|v| v.as_str())
            .unwrap_or("rows_are_cells");
        match (shape, barcodes, features) {
            (Some((rows, cols)), Some(bc), Some(feat)) => {
                let (expected_rows, expected_cols) = if layout == "rows_are_cells" {
                    (bc, feat)
                } else {
                    (feat, bc)
                };
                if rows == expected_rows && cols == expected_cols {
                    ValidatorOutcome::Passed
                } else {
                    ValidatorOutcome::Failed {
                        message: format!(
                            "matrix shape {}x{} != expected {}x{} ({} layout)",
                            rows, cols, expected_rows, expected_cols, layout
                        ),
                    }
                }
            }
            _ => ValidatorOutcome::Errored {
                reason: "result.json missing matrix_shape / n_barcodes / n_features".into(),
            },
        }
    }
}

/// `no_train_test_leakage`. Reads
/// `<artifact_path>/result.json::splits` (a JSON object with
/// `train` and `test` arrays of sample ids) and asserts the
/// intersection is empty.
pub struct TrainTestLeakageCheckRunner;

impl ValidatorRunner for TrainTestLeakageCheckRunner {
    fn obligation_id(&self) -> &'static str {
        "no_train_test_leakage"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let results = match read_json(&artifact_path.join("result.json")) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let splits = match results.get("splits") {
            Some(v) => v,
            None => {
                return ValidatorOutcome::Errored {
                    reason: "result.json::splits missing".into(),
                };
            }
        };
        let train: std::collections::BTreeSet<String> = splits
            .get("train")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let test: std::collections::BTreeSet<String> = splits
            .get("test")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let overlap: Vec<&String> = train.intersection(&test).collect();
        if overlap.is_empty() {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: format!(
                    "{} sample(s) in both train and test: {}",
                    overlap.len(),
                    overlap
                        .iter()
                        .take(5)
                        .map(|s| (*s).clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }
        }
    }
}

/// `deterministic_or_bounded_variance`. Compares
/// `<artifact_path>/result.json` to `<artifact_path>/result.rerun.json`
/// (produced by an opt-in re-run pass when
/// `ECAA_DETERMINISM_RERUN=1`). Asserts byte-equality of the two
/// JSON values. Soft-skips when the rerun file is absent.
pub struct DeterminismRerunRunner;

impl ValidatorRunner for DeterminismRerunRunner {
    fn obligation_id(&self) -> &'static str {
        "deterministic_or_bounded_variance"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let primary_path = artifact_path.join("result.json");
        let rerun_path = artifact_path.join("result.rerun.json");
        if !rerun_path.exists() {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "rerun artifact absent at {}; set ECAA_DETERMINISM_RERUN=1 to enable",
                    rerun_path.display()
                ),
            };
        }
        let primary = match read_json(&primary_path) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let rerun = match read_json(&rerun_path) {
            Ok(v) => v,
            Err(e) => return e,
        };
        if primary == rerun {
            ValidatorOutcome::Passed
        } else {
            ValidatorOutcome::Failed {
                message: "result.json and result.rerun.json diverge — task is not \
                          deterministic"
                    .into(),
            }
        }
    }
}

fn read_json(path: &Path) -> Result<serde_json::Value, ValidatorOutcome> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ValidatorOutcome::Errored {
                reason: format!("{} missing", path.display()),
            });
        }
        Err(e) => {
            return Err(ValidatorOutcome::Errored {
                reason: format!("read error at {}: {}", path.display(), e),
            });
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| ValidatorOutcome::Errored {
        reason: format!("{} parse error: {}", path.display(), e),
    })
}

/// Aggregate report shape — one entry per task's validator run.
/// Serialized to `runtime/validation-reports.jsonl` and consulted by
/// the harness post-task wiring (`evaluate_validation`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationReportSummary {
    /// Task identifier this report covers.
    pub task_id: String,
    /// One `ValidatorRow` per obligation in the task's `ValidationBundle`.
    pub rows: Vec<ValidatorRow>,
}

impl ValidationReportSummary {
    /// True when at least one row is `Failed`. The harness
    /// `evaluate_validation` consults this to decide whether to
    /// transition the task to `Blocked { ValidationFailed }`.
    pub fn has_failures(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.outcome, ValidatorOutcome::Failed { .. }))
    }

    /// Human summary the verify endpoint surfaces.
    pub fn human_summary(&self) -> String {
        let total = self.rows.len();
        let passed = self
            .rows
            .iter()
            .filter(|r| matches!(r.outcome, ValidatorOutcome::Passed))
            .count();
        let failed = self
            .rows
            .iter()
            .filter(|r| matches!(r.outcome, ValidatorOutcome::Failed { .. }))
            .count();
        let errored = self
            .rows
            .iter()
            .filter(|r| matches!(r.outcome, ValidatorOutcome::Errored { .. }))
            .count();
        let unimpl = self
            .rows
            .iter()
            .filter(|r| matches!(r.outcome, ValidatorOutcome::Unimplemented { .. }))
            .count();
        format!(
            "task {}: {}/{} passed, {} failed, {} errored, {} unimplemented",
            self.task_id, passed, total, failed, errored, unimpl
        )
    }

    /// Serialize one row per line as JSONL for the
    /// `runtime/validation-reports.jsonl` sidecar. Stable ordering
    /// (sorted by obligation_id) for byte-stability.
    pub fn to_jsonl(&self) -> String {
        let mut sorted: Vec<&ValidatorRow> = self.rows.iter().collect();
        sorted.sort_by(|a, b| a.obligation_id.cmp(&b.obligation_id));
        let mut out = String::new();
        for row in sorted {
            let outcome_str = match &row.outcome {
                ValidatorOutcome::Passed => "passed".to_string(),
                ValidatorOutcome::Failed { message } => format!("failed:{message}"),
                ValidatorOutcome::Errored { reason } => format!("errored:{reason}"),
                ValidatorOutcome::Unimplemented { obligation_id } => {
                    format!("unimplemented:{obligation_id}")
                }
            };
            let entry = serde_json::json!({
                "task_id": self.task_id,
                "obligation_id": row.obligation_id,
                "outcome": outcome_str,
            });
            if let Ok(line) = serde_json::to_string(&entry) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        out
    }
}

/// Run the bundle on a task's artifacts and produce a summary. The
/// harness uses this in its post-task-completion path: failures
/// transition the task to `Blocked { ValidationFailed }`.
pub fn evaluate_validation(
    task_id: &str,
    obligations: &[String],
    runners: &[Box<dyn ValidatorRunner>],
    artifact_path: &Path,
) -> ValidationReportSummary {
    let rows = run_validators(obligations, runners, artifact_path);
    ValidationReportSummary {
        task_id: task_id.to_string(),
        rows,
    }
}

/// Default registry — starter runners plus the literature runners
/// registered by `crate::literature_validators::literature_runners`.
pub fn default_runners() -> Vec<Box<dyn ValidatorRunner>> {
    let mut runners: Vec<Box<dyn ValidatorRunner>> = vec![
        Box::new(PValueInUnitIntervalRunner) as Box<dyn ValidatorRunner>,
        Box::new(GeneIdInAnnotationRunner),
        Box::new(CoordinateInContigRunner),
        Box::new(CellBarcodeMatrixDimensionConsistencyRunner),
        Box::new(TrainTestLeakageCheckRunner),
        Box::new(DeterminismRerunRunner),
    ];
    runners.extend(crate::literature_validators::literature_runners());
    runners
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_result_json(dir: &Path, json: serde_json::Value) {
        fs::write(dir.join("result.json"), json.to_string()).unwrap();
    }

    #[test]
    fn missing_result_json_is_errored_not_failed() {
        let tmp = TempDir::new().unwrap();
        let runner = PValueInUnitIntervalRunner;
        let outcome = runner.run(tmp.path());
        assert!(matches!(outcome, ValidatorOutcome::Errored { .. }));
    }

    #[test]
    fn p_values_in_range_pass() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({
                "summary": "OK",
                "padj": 0.05,
                "adjusted_pvalue": 0.001,
                "q_value": 0.5,
            }),
        );
        let runner = PValueInUnitIntervalRunner;
        let outcome = runner.run(tmp.path());
        assert_eq!(outcome, ValidatorOutcome::Passed);
    }

    #[test]
    fn p_value_above_one_fails() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({
                "padj": 1.5,
            }),
        );
        let runner = PValueInUnitIntervalRunner;
        match runner.run(tmp.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(message.contains("padj=1.5"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn p_value_below_zero_fails() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({
                "results": [
                    { "gene": "X", "q_value": -0.1 }
                ]
            }),
        );
        let runner = PValueInUnitIntervalRunner;
        match runner.run(tmp.path()) {
            ValidatorOutcome::Failed { message } => assert!(message.contains("q_value")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn run_validators_routes_to_the_right_runner() {
        let tmp = TempDir::new().unwrap();
        write_result_json(tmp.path(), serde_json::json!({"padj": 0.5}));
        let runners = default_runners();
        let rows = run_validators(&["p_value_in_unit_interval".into()], &runners, tmp.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].outcome, ValidatorOutcome::Passed);
    }

    #[test]
    fn unknown_obligation_returns_unimplemented_row() {
        let tmp = TempDir::new().unwrap();
        let runners = default_runners();
        let rows = run_validators(&["unknown_obligation".into()], &runners, tmp.path());
        assert_eq!(rows.len(), 1);
        match &rows[0].outcome {
            ValidatorOutcome::Unimplemented { obligation_id } => {
                assert_eq!(obligation_id, "unknown_obligation")
            }
            other => panic!("expected Unimplemented, got {other:?}"),
        }
    }

    /// Regression: every runner in `default_runners()` must return an
    /// `obligation_id` that exists in the canonical starter obligation
    /// set declared by `crates/core::validation_obligations`. The
    /// harness's `run_validators` looks up runners by id-equality, so a
    /// runner returning a non-canonical string falls through to
    /// `ValidatorOutcome::Unimplemented` at runtime — the exact bug
    /// fixed by this commit (three of six starter runners drifted).
    ///
    /// `starter_obligations()` is private to `crates/core`; we
    /// reconstruct the canonical starter id set as
    /// `ValidationRegistry::with_starters()` minus the public
    /// `renderer_validation_bundle()` obligation ids (the only other
    /// obligation source registered by `with_starters`).
    #[test]
    fn default_runners_cover_starter_obligations() {
        use ecaa_workflow_core::validation_obligations::{
            renderer_validation_bundle, ValidationRegistry,
        };

        let renderer_ids: std::collections::BTreeSet<String> = renderer_validation_bundle()
            .obligations
            .iter()
            .map(|o| o.id.clone())
            .collect();
        let canonical_starter_ids: std::collections::BTreeSet<String> =
            ValidationRegistry::with_starters()
                .obligations()
                .map(|(id, _)| id.clone())
                .filter(|id| !renderer_ids.contains(id))
                .collect();
        let drifted: Vec<&'static str> = default_runners()
            .iter()
            .map(|r| r.obligation_id())
            .filter(|id| !canonical_starter_ids.contains(*id))
            .collect();
        assert!(
            drifted.is_empty(),
            "runner obligation_ids not in canonical starter set: {drifted:?}; \
             canonical_starter_ids={canonical_starter_ids:?}"
        );
    }
}
