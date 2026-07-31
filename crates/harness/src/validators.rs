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

/// Harness-local obligation applied to completed `discover_*` tasks.
/// Agent-authored evidence flags must agree with the exact
/// `(axis, candidate_method)` rows retained in `method_landscape.csv`.
pub const DISCOVERY_EVIDENCE_OBLIGATION: &str = "discovery_evidence_consistent";

pub struct DiscoveryEvidenceConsistencyRunner;

/// Whether a discovery artifact declares the method-landscape contract that
/// [`DiscoveryEvidenceConsistencyRunner`] validates.
///
/// Some synthetic or legacy `discover_*` tasks are ordinary computation
/// fixtures and carry neither a canonical `spec.stage_class` nor an upstream
/// landscape. Their name alone is not enough to assert this obligation.
pub fn discovery_evidence_is_applicable(artifact_path: &Path) -> bool {
    if !artifact_path.join("decision.json").is_file() {
        return false;
    }
    let has_stage_class = std::fs::read(artifact_path.join("task-spec.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|spec| {
            spec.pointer("/spec/stage_class")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|value| !value.is_empty());
    if !has_stage_class {
        return false;
    }
    artifact_path
        .ancestors()
        .find(|path| path.join("runtime").join("outputs").is_dir())
        .is_some_and(|package_root| {
            package_root
                .join("runtime/outputs/survey_method_landscape/method_landscape.csv")
                .is_file()
        })
}

impl ValidatorRunner for DiscoveryEvidenceConsistencyRunner {
    fn obligation_id(&self) -> &'static str {
        DISCOVERY_EVIDENCE_OBLIGATION
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let Some(task_id) = artifact_path.file_name().and_then(|n| n.to_str()) else {
            return ValidatorOutcome::Failed {
                message: "discovery artifact path has no UTF-8 task id".into(),
            };
        };
        let Some(fallback_axis) = task_id.strip_prefix("discover_") else {
            return ValidatorOutcome::Errored {
                reason: format!("{task_id} is not a discover_* task"),
            };
        };
        // Aliased tasks retain their canonical method-choice axis in
        // task-spec.json::spec.stage_class. Prefer that value so a task such
        // as discover_rnaseq_differential_expression is checked against the
        // differential_expression landscape rows instead of an alias that
        // does not exist in the matrix.
        let task_spec_path = artifact_path.join("task-spec.json");
        let axis = std::fs::read(&task_spec_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .and_then(|spec| {
                spec.pointer("/spec/stage_class")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            })
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| fallback_axis.to_string());
        let decision_path = artifact_path.join("decision.json");
        let decision: serde_json::Value = match std::fs::read(&decision_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(v) => v,
            None => {
                return ValidatorOutcome::Failed {
                    message: format!(
                        "missing or malformed discovery decision at {}",
                        decision_path.display()
                    ),
                };
            }
        };
        let Some(package_root) = artifact_path
            .ancestors()
            .find(|p| p.join("runtime").join("outputs").is_dir())
        else {
            return ValidatorOutcome::Failed {
                message: format!(
                    "cannot locate package root from discovery artifact {}",
                    artifact_path.display()
                ),
            };
        };
        let landscape_path =
            package_root.join("runtime/outputs/survey_method_landscape/method_landscape.csv");
        let landscape = match std::fs::read_to_string(&landscape_path) {
            Ok(csv) => csv,
            Err(e) => {
                return ValidatorOutcome::Failed {
                    message: format!(
                        "cannot read method landscape at {}: {}",
                        landscape_path.display(),
                        e
                    ),
                };
            }
        };
        let by_axis = match ecaa_workflow_core::method_landscape::load_candidate_metadata_from_str(
            &landscape,
        ) {
            Ok(v) => v,
            Err(e) => {
                return ValidatorOutcome::Failed {
                    message: format!("method landscape parse failed: {e}"),
                };
            }
        };
        let expected: std::collections::BTreeMap<
            &str,
            &ecaa_workflow_core::composite_score::CandidateMetadata,
        > = by_axis
            .get(&axis)
            .into_iter()
            .flatten()
            .map(|(method, metadata)| (method.as_str(), metadata))
            .collect();
        let Some(candidates) = decision
            .get("candidate_pool_full")
            .and_then(serde_json::Value::as_array)
        else {
            return ValidatorOutcome::Failed {
                message: "decision.json lacks candidate_pool_full".into(),
            };
        };

        for candidate in candidates {
            let method = candidate
                .get("method_id")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            if method.is_empty() {
                return ValidatorOutcome::Failed {
                    message: "candidate_pool_full row lacks method_id".into(),
                };
            }
            let expected_meta = expected.get(method).copied();
            let expected_eligible = expected_meta
                .map(|m| m.literature_eligible)
                .unwrap_or(false);
            let expected_support = expected_meta
                .map(|m| m.supporting_evidence_count)
                .unwrap_or(0);
            let expected_high_quality = expected_meta
                .map(|m| m.high_quality_evidence_count)
                .unwrap_or(0);
            let actual_eligible = candidate
                .get("literature_eligible")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let passes = candidate
                .get("passes_default_eligibility_criteria")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let tier = candidate
                .get("recommended_tier")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");

            if actual_eligible != expected_eligible {
                return ValidatorOutcome::Failed {
                    message: format!(
                        "{axis}/{method}: literature_eligible={actual_eligible} but exact-axis evidence requires {expected_eligible}"
                    ),
                };
            }
            if passes && (!expected_eligible || expected_support == 0 || expected_high_quality == 0)
            {
                return ValidatorOutcome::Failed {
                    message: format!(
                        "{axis}/{method}: passes_default_eligibility_criteria=true with literature_eligible={expected_eligible}, supporting={expected_support}, high_quality={expected_high_quality}"
                    ),
                };
            }
            if tier == "defaultRecommended" && (!expected_eligible || !passes) {
                return ValidatorOutcome::Failed {
                    message: format!(
                        "{axis}/{method}: defaultRecommended requires exact-axis literature eligibility and all default criteria"
                    ),
                };
            }
        }
        ValidatorOutcome::Passed
    }
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

/// `variant_af_spectrum_plausible`. Reads
/// `<artifact_path>/result.json::af_values` (a JSON array of allele
/// frequencies) and asserts the spectrum is well-formed for a
/// heteroplasmy-dominated mtDNA call set: every AF lies in `[0, 1]` and
/// the median sits at the low end (right-skewed). This is the
/// obligation-keyed companion to the goal-driven
/// `numeric_distribution` assertion arm — it guards the *shape* of the
/// AF column rather than a specific operator bound, so it never hands
/// the agent a threshold. Soft-skips when `af_values` is absent.
pub struct VariantAfSpectrumPlausibleRunner;

impl ValidatorRunner for VariantAfSpectrumPlausibleRunner {
    fn obligation_id(&self) -> &'static str {
        "variant_af_spectrum_plausible"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let result_path = artifact_path.join("result.json");
        // Fail closed when the measurement input is entirely absent: this is a
        // goal-required check (the AF-spectrum measurement step did not run),
        // so a missing result.json must surface as Failed (which has_failures
        // counts) rather than the soft-skip Errored — otherwise a wrong call
        // set is silently accepted. A present-but-malformed result.json is a
        // genuine internal error and stays Errored (see read_json below).
        if !result_path.exists() {
            return ValidatorOutcome::Failed {
                message: "AF-spectrum input (result.json) is absent — the measurement step did not run; failing closed so the wrong call set is not silently accepted".to_string(),
            };
        }
        let results = match read_json(&result_path) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let Some(arr) = results.get("af_values").and_then(|v| v.as_array()) else {
            return ValidatorOutcome::Errored {
                reason: "result.json::af_values not present".into(),
            };
        };
        let values: Vec<f64> = arr.iter().filter_map(|v| v.as_f64()).collect();
        if values.is_empty() {
            return ValidatorOutcome::Errored {
                reason: "result.json::af_values is empty or non-numeric".into(),
            };
        }
        // Every AF must be a fraction in [0, 1].
        let out_of_unit: Vec<f64> = values
            .iter()
            .copied()
            .filter(|&v| !(0.0..=1.0).contains(&v))
            .collect();
        if !out_of_unit.is_empty() {
            return ValidatorOutcome::Failed {
                message: format!(
                    "{} allele frequency value(s) outside [0, 1]: {}",
                    out_of_unit.len(),
                    out_of_unit
                        .iter()
                        .take(3)
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            };
        }
        // NOTE: a median-AF bound is NOT a valid mtDNA plausibility criterion and
        // was removed. mtDNA called against the rCRS reference is
        // HOMOPLASMY-DOMINATED — most variants are fixed differences from rCRS at
        // AF≈1.0, with heteroplasmic sites the low-AF MINORITY. The Nekrutenko
        // ground-truth answer key itself has a pooled AF median of 0.9972 (88.9%
        // of calls > 0.5), so the former `median <= 0.5` ("right-skewed")
        // assertion FAILED the benchmark's own truth set — a biologically wrong
        // rule that blocked correct call sets. Both heteroplasmy-rich (low median)
        // and homoplasmy-dominated (high median) spectra are valid; the median
        // does not discriminate a correct call set from a reference/orientation
        // error. The real shape guard is AF ∈ [0, 1] above (an inverted/garbled AF
        // column or wrong-reference call still has to land in the unit interval,
        // and gross reference errors surface via coordinate_in_contig + the
        // per-sample-count reference-range assertion). Keep this validator to the
        // unit-interval invariant; do not reinstate a median bound.
        ValidatorOutcome::Passed
    }
}

/// `variant_filtered_count_consistency`. Reads the filtered stage's
/// `<artifact_path>/result.json::variant_count` and the upstream called
/// count recorded as `result.json::called_variant_count` (the agent
/// copies the upstream total forward at filter time). Asserts the
/// filtered count does not exceed the called count — filtering removes,
/// never adds. This is the obligation-keyed companion to the
/// `cross_stage_output_comparison` assertion arm for harness installs
/// that route via the ValidatorRunner registry instead of the
/// validation contract. Soft-skips when either field is absent.
pub struct VariantFilteredCountConsistencyRunner;

impl ValidatorRunner for VariantFilteredCountConsistencyRunner {
    fn obligation_id(&self) -> &'static str {
        "variant_filtered_count_consistency"
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        let results = match read_json(&artifact_path.join("result.json")) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let filtered = results.get("variant_count").and_then(|v| v.as_f64());
        let called = results.get("called_variant_count").and_then(|v| v.as_f64());
        match (filtered, called) {
            (Some(f), Some(c)) => {
                if f <= c {
                    ValidatorOutcome::Passed
                } else {
                    ValidatorOutcome::Failed {
                        message: format!(
                            "filtered variant count {f} exceeds called count {c} — filtering must not add records"
                        ),
                    }
                }
            }
            _ => ValidatorOutcome::Errored {
                reason: "result.json missing variant_count / called_variant_count".into(),
            },
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

// ── source-deviation provenance obligation ─────────────────────────
//
// A task's agent can be forced to read a DIFFERENT data source than
// intake requested (the SME's local directory is absent, an accession
// is embargoed, a mirror is down). That substitution happens at
// EXECUTION time, after emission, so no intake-side tool can capture
// it — and the agent is forbidden from writing
// `runtime/decisions.jsonl`. The contract is therefore split in two:
//
//   1. the agent RECORDS the substitution in its own
//      `result.json::source_deviation` block (the RECORD-WHAT-YOU-DID
//      clause on ingestion atoms);
//   2. the HARNESS promotes that block into one typed
//      `DecisionType::DataSourceDeviation` record.
//
// This obligation is the enforcement half: a declared deviation with no
// matching decision record — or one whose named source contradicts the
// package's own `per_accession_summary.json` — is a REQUIRED failure.
// Keyed on the data (the `source_deviation` block), never on an atom id,
// so it holds for ANY ingestion atom in any modality.

/// Stable obligation id for the source-deviation provenance check.
/// Harness-local (no entry in core's starter registry); the harness
/// unions it into a completed task's obligation bundle whenever that
/// task's `result.json` declares a `source_deviation` block, and an atom
/// may additionally opt in by naming it in its `validators:` list.
pub const SOURCE_DEVIATION_OBLIGATION: &str = "source_deviation_recorded";

/// Keys whose value names a DATA SOURCE, matched case-insensitively at
/// any depth of `per_accession_summary.json`. Deliberately excludes
/// `accession` / `study` / `pmid`: a summary legitimately records the
/// accession the data CORRESPONDS to even when the bytes came from a
/// redistribution of it, so treating an accession as a source claim
/// would false-flag every honest substitution.
const SOURCE_DESIGNATING_KEYS: &[&str] = &[
    "source",
    "sources",
    "source_package",
    "source_name",
    "source_root",
    "source_uri",
    "source_url",
    "data_source",
    "data_sources",
    "origin",
    "provenance",
    "provenance_note",
    "repository",
    "retrieved_from",
];

/// Tokens too generic to identify a source. A `used` string whose
/// tokens are ALL generic (e.g. "local counts directory") carries no
/// discriminating signal, so the contradiction cross-check skips it
/// rather than guessing.
const GENERIC_SOURCE_TOKENS: &[&str] = &[
    "the", "and", "for", "from", "via", "with", "was", "were", "data", "dataset", "datasets",
    "file", "files", "path", "local", "raw", "count", "counts", "matrix", "source", "sources",
    "study", "package", "version", "object", "repo", "input", "inputs", "output", "outputs",
    "folder", "archive", "table", "tables", "home", "user", "tmp", "var", "opt",
];

/// Parse the `source_deviation` block out of a task's `result.json`.
///
/// Accepts the block at the result-root (`source_deviation`, the
/// RECORD-WHAT-YOU-DID convention every other atom key follows) or
/// nested under `attributes` (`attributes.source_deviation`), so an
/// agent that mirrors the atom's `attributes:` layout is not silently
/// ignored. Returns `None` when there is no result.json, no block, or
/// the block is not a JSON object — "nothing declared", which is the
/// overwhelmingly common case and must stay free.
pub fn read_source_deviation(
    artifact_path: &Path,
) -> Option<ecaa_workflow_core::decision_log::SourceDeviation> {
    let value = read_json(&artifact_path.join("result.json")).ok()?;
    let block = value.get("source_deviation").or_else(|| {
        value
            .get("attributes")
            .and_then(|a| a.get("source_deviation"))
    })?;
    if !block.is_object() {
        return None;
    }
    // Every field is `#[serde(default)]`, so any JSON object parses;
    // a half-filled block surfaces as empty required fields below
    // rather than as a silent parse failure.
    serde_json::from_value(block.clone()).ok()
}

/// Walk up from a task's artifact dir to the package root by locating
/// the `runtime/outputs` boundary. Pure path arithmetic — no filesystem
/// reads — so it behaves identically under a tempdir fixture and a real
/// package. Falls back to the dir's great-grandparent when the layout
/// is non-canonical.
fn package_root_from_artifact_path(artifact_path: &Path) -> std::path::PathBuf {
    let mut anc = Some(artifact_path);
    while let Some(dir) = anc {
        let is_outputs = dir.file_name().map(|f| f == "outputs").unwrap_or(false)
            && dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|f| f == "runtime")
                .unwrap_or(false);
        if is_outputs {
            if let Some(pkg) = dir.parent().and_then(|p| p.parent()) {
                return pkg.to_path_buf();
            }
        }
        anc = dir.parent();
    }
    artifact_path
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// The `DataSourceDeviation` payload already recorded for `task_id` in
/// `<package_root>/runtime/decisions.jsonl`, if any. Malformed lines are
/// skipped (the log is append-only and may carry rows written by an
/// older schema); this is a presence probe, not a log validator.
pub fn recorded_source_deviation(
    package_root: &Path,
    task_id: &str,
) -> Option<ecaa_workflow_core::decision_log::SourceDeviation> {
    use ecaa_workflow_core::decision_log::{DecisionRecord, DecisionType};
    let raw = std::fs::read_to_string(package_root.join("runtime").join("decisions.jsonl")).ok()?;
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<DecisionRecord>(line) else {
            continue;
        };
        if let DecisionType::DataSourceDeviation {
            task_id: recorded_task,
            deviation,
        } = rec.decision
        {
            if recorded_task.as_str() == task_id {
                return Some(deviation);
            }
        }
    }
    None
}

/// Lowercase alphanumeric tokens of `s` that could identify a source
/// (length ≥ 3, not in [`GENERIC_SOURCE_TOKENS`]).
fn distinctive_tokens(s: &str) -> std::collections::BTreeSet<String> {
    s.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|t| t.len() >= 3 && !GENERIC_SOURCE_TOKENS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

/// Collect every string value that sits under a
/// [`SOURCE_DESIGNATING_KEYS`] key, at any depth of `value`. Arrays are
/// traversed; a source key holding an array of strings contributes each
/// element.
fn collect_source_claims(value: &serde_json::Value, under_source_key: bool, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                let key_is_source =
                    SOURCE_DESIGNATING_KEYS.contains(&k.to_ascii_lowercase().as_str());
                collect_source_claims(v, key_is_source, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_source_claims(v, under_source_key, out);
            }
        }
        serde_json::Value::String(s) if under_source_key => {
            if !s.trim().is_empty() {
                out.push(s.clone());
            }
        }
        _ => {}
    }
}

/// `source_deviation_recorded` — the REQUIRED provenance obligation.
///
/// Fails when a task's `result.json` declares a `source_deviation` and
/// either
///   (a) `runtime/decisions.jsonl` carries no `DataSourceDeviation`
///       record for the task (the substitution never reached the typed
///       audit trail), or
///   (b) the recorded decision names a different substitute than
///       result.json does (the two halves of the contract disagree), or
///   (c) the substitute contradicts the package's own
///       `per_accession_summary.json` — the summary makes at least one
///       source claim and NONE of them shares a distinctive token with
///       the source result.json says was used.
///
/// Passes when no deviation is declared (the common case). Soft-skips
/// (`Errored`) only when result.json itself is missing or unparseable —
/// that is the generic missing-artifact guard's job, not this one's.
pub fn source_deviation_recorded(artifact_path: &Path) -> ValidatorOutcome {
    let result_path = artifact_path.join("result.json");
    if let Err(e) = read_json(&result_path) {
        return e;
    }
    let Some(declared) = read_source_deviation(artifact_path) else {
        // No substitution claimed — nothing to reconcile.
        return ValidatorOutcome::Passed;
    };

    let task_id = artifact_path
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or_default()
        .to_string();

    // A declared deviation that does not say what was used is
    // unauditable — treat it as the failure it is rather than letting
    // the empty string match everything downstream.
    if declared.used.trim().is_empty() || declared.requested.trim().is_empty() {
        return ValidatorOutcome::Failed {
            message: format!(
                "task {task_id} declares result.json::source_deviation but leaves \
                 requested/used empty (requested={:?}, used={:?}) — an unauditable \
                 substitution record",
                declared.requested, declared.used
            ),
        };
    }

    // (a)/(b) — the typed decision record must exist and agree.
    let package_root = package_root_from_artifact_path(artifact_path);
    match recorded_source_deviation(&package_root, &task_id) {
        None => {
            return ValidatorOutcome::Failed {
                message: format!(
                    "task {task_id} substituted its data source (requested={:?}, used={:?}) \
                     but runtime/decisions.jsonl carries no data_source_deviation record — \
                     the substitution exists only in agent free text",
                    declared.requested, declared.used
                ),
            };
        }
        Some(recorded) if recorded.used.trim() != declared.used.trim() => {
            return ValidatorOutcome::Failed {
                message: format!(
                    "task {task_id} source-deviation mismatch: decisions.jsonl records \
                     used={:?} but result.json records used={:?}",
                    recorded.used, declared.used
                ),
            };
        }
        Some(_) => {}
    }

    // (c) — cross-check against the package's own per-accession summary.
    let summary_path = artifact_path.join("per_accession_summary.json");
    if let Ok(summary) = read_json(&summary_path) {
        let mut claims: Vec<String> = Vec::new();
        collect_source_claims(&summary, false, &mut claims);
        let used_tokens = distinctive_tokens(&declared.used);
        // No distinctive token in `used` (e.g. "local counts directory")
        // means the cross-check has nothing to key on — skip rather than
        // guess. Likewise when the summary makes no source claim at all.
        if !claims.is_empty() && !used_tokens.is_empty() {
            let corroborated = claims
                .iter()
                .any(|c| !distinctive_tokens(c).is_disjoint(&used_tokens));
            if !corroborated {
                claims.sort();
                return ValidatorOutcome::Failed {
                    message: format!(
                        "task {task_id} records used={:?} in result.json::source_deviation, \
                         but per_accession_summary.json names only {:?} as its source — the \
                         package contradicts its own deviation record",
                        declared.used,
                        claims.iter().take(3).collect::<Vec<_>>()
                    ),
                };
            }
        }
    }

    ValidatorOutcome::Passed
}

/// Promote a completed task's `result.json::source_deviation` block into
/// one typed `DecisionType::DataSourceDeviation` row in
/// `<package_root>/runtime/decisions.jsonl`, and return what the task
/// declared (`None` when it declared nothing).
///
/// The HARNESS calls this, never the agent: agents are forbidden from
/// touching `runtime/decisions.jsonl`
/// (`scripts/agent-prompts/task-execution.md`), and the substitution
/// happens at execution time — post-emission — so no intake-side tool
/// can capture it either.
///
/// Lives beside [`source_deviation_recorded`] rather than in the harness
/// binary deliberately: the writer and the obligation that enforces it
/// must agree on the record shape AND on the dedup predicate
/// ([`recorded_source_deviation`]). Splitting them across a bin and a
/// lib guarantees they drift. This is the one write in this module;
/// every `ValidatorRunner` here stays side-effect-free.
///
/// Idempotent by construction — the on-disk log is probed first, so the
/// harness re-entering its completion loop on every pass (and a
/// standalone re-run over a finished package) both leave exactly one row
/// per (task, substitution). Best-effort: an append failure is logged
/// and the declared block is still returned, so the obligation below
/// turns the missing record into a blocking failure instead of the
/// harness silently swallowing it.
pub fn promote_source_deviation(
    package_root: &Path,
    task_id: &str,
    session_id: &str,
    clock: &dyn ecaa_workflow_core::clock::Clock,
) -> Option<ecaa_workflow_core::decision_log::SourceDeviation> {
    use ecaa_workflow_core::decision_log::{
        DecisionActor, DecisionAuthority, DecisionRecord, DecisionType,
    };

    let artifact_path = package_root.join("runtime").join("outputs").join(task_id);
    let deviation = read_source_deviation(&artifact_path)?;

    if recorded_source_deviation(package_root, task_id).is_some() {
        return Some(deviation);
    }

    let mut record = DecisionRecord::new(
        session_id,
        DecisionType::DataSourceDeviation {
            task_id: task_id.into(),
            deviation: deviation.clone(),
        },
        DecisionActor::Harness,
        Some(format!(
            "harness-promoted source substitution: requested={:?} (available={}) used={:?} — {}",
            deviation.requested, deviation.requested_available, deviation.used, deviation.reason
        )),
    );
    // Harness actor → SchemaValidated: the record is derived
    // deterministically from the task's own artifact, not inferred by an
    // LLM (mirrors `scheduler::promote_auto_advance_decisions`).
    record.authority = DecisionAuthority::SchemaValidated;
    // C6 — timestamp from the injected clock, never `SystemTime::now()`,
    // so a FrozenClock run stays reproducible.
    record.timestamp = clock.now();

    if let Err(e) = append_decision_record(package_root, &record) {
        tracing::warn!(
            target: "harness-provenance",
            task_id,
            error = format!("{e}"),
            "failed to append the source-deviation decision record"
        );
    }
    Some(deviation)
}

/// Append one `DecisionRecord` to `<package_root>/runtime/decisions.jsonl`.
///
/// Mirrors the append + fdatasync discipline of the private
/// `scheduler::append_decision` (itself a mirror of
/// `conversation::session::decision_helpers::record_decision_with_ip`).
fn append_decision_record(
    package_root: &Path,
    record: &ecaa_workflow_core::decision_log::DecisionRecord,
) -> std::io::Result<()> {
    use std::io::Write as _;
    let path = package_root.join("runtime").join("decisions.jsonl");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line =
        serde_json::to_string(record).expect("DecisionRecord always serializes to valid JSON");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(f, "{line}")?;
    // fdatasync: the record must survive a kernel crash between the
    // write and a later read of `runtime/decisions.jsonl`.
    f.sync_data()?;
    Ok(())
}

/// Registry adapter for [`source_deviation_recorded`].
pub struct SourceDeviationRecordedRunner;

impl ValidatorRunner for SourceDeviationRecordedRunner {
    fn obligation_id(&self) -> &'static str {
        SOURCE_DEVIATION_OBLIGATION
    }

    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        source_deviation_recorded(artifact_path)
    }
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
        // Variant-domain runners. These obligation ids are harness-local
        // (not yet mirrored in core's starter registry); they are the
        // ValidatorRunner companions to the goal-driven variant
        // assertion arms and are exempted in
        // `default_runners_cover_starter_obligations`.
        Box::new(VariantAfSpectrumPlausibleRunner),
        Box::new(VariantFilteredCountConsistencyRunner),
        // Provenance obligation. Harness-local like the variant pair
        // above; unioned into a task's bundle by the harness whenever
        // that task's result.json declares a source_deviation block, so
        // it holds for every ingestion atom without per-atom wiring.
        Box::new(SourceDeviationRecordedRunner),
        // Discovery evidence flags are checked from retained artifacts rather
        // than declared by any one atom.
        Box::new(DiscoveryEvidenceConsistencyRunner),
    ];
    runners.extend(crate::literature_validators::literature_runners());
    runners
}

/// Harness-local variant-domain obligation ids that intentionally have
/// no entry in core's `validation_obligations` starter registry. They
/// are the ValidatorRunner companions to the goal-driven variant
/// assertion arms (driven by `validation-contract-variants.json`) and
/// are exempted from the starter-coverage drift check below. If the
/// integrated build later mirrors these into core's starter set, remove
/// them from this list so the drift check re-tightens.
#[cfg(test)]
const HARNESS_LOCAL_VARIANT_OBLIGATIONS: &[&str] = &[
    "variant_af_spectrum_plausible",
    "variant_filtered_count_consistency",
];

/// Harness-local provenance obligation ids with no entry in core's
/// starter registry. Unlike the variant pair above these are not
/// contract-driven: the harness unions them into a task's bundle from
/// the SHAPE of the task's own result.json, so they never need an atom
/// to name them. Exempted from the starter-coverage drift check for the
/// same reason.
#[cfg(test)]
const HARNESS_LOCAL_PROVENANCE_OBLIGATIONS: &[&str] = &[SOURCE_DEVIATION_OBLIGATION];

#[cfg(test)]
const HARNESS_LOCAL_DISCOVERY_OBLIGATIONS: &[&str] = &[DISCOVERY_EVIDENCE_OBLIGATION];

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

    fn discovery_fixture(
        landscape_rows: &str,
        candidate: serde_json::Value,
    ) -> (TempDir, std::path::PathBuf) {
        let pkg = TempDir::new().unwrap();
        let survey = pkg.path().join("runtime/outputs/survey_method_landscape");
        let discover = pkg.path().join("runtime/outputs/discover_normalisation");
        fs::create_dir_all(&survey).unwrap();
        fs::create_dir_all(&discover).unwrap();
        fs::write(
            survey.join("method_landscape.csv"),
            format!(
                "axis,candidate_method,source_class,verified\n{}",
                landscape_rows
            ),
        )
        .unwrap();
        fs::write(
            discover.join("decision.json"),
            serde_json::json!({"candidate_pool_full": [candidate]}).to_string(),
        )
        .unwrap();
        (pkg, discover)
    }

    #[test]
    fn discovery_evidence_rejects_cross_axis_borrowing() {
        let (pkg, discover) = discovery_fixture(
            "differential_expression,deseq2,primary_literature,true\n\
             normalisation,deseq2_vst,curated_baseline,false\n",
            serde_json::json!({
                "method_id": "deseq2_vst",
                "literature_eligible": true,
                "passes_default_eligibility_criteria": true,
                "recommended_tier": "defaultRecommended"
            }),
        );
        let outcome = DiscoveryEvidenceConsistencyRunner.run(&discover);
        assert!(matches!(outcome, ValidatorOutcome::Failed { .. }));
        drop(pkg);
    }

    #[test]
    fn discovery_evidence_allows_explicit_ineligible_alternative() {
        let (pkg, discover) = discovery_fixture(
            "normalisation,deseq2_vst,curated_baseline,false\n",
            serde_json::json!({
                "method_id": "deseq2_vst",
                "literature_eligible": false,
                "passes_default_eligibility_criteria": false,
                "recommended_tier": "alternative"
            }),
        );
        assert_eq!(
            DiscoveryEvidenceConsistencyRunner.run(&discover),
            ValidatorOutcome::Passed
        );
        drop(pkg);
    }

    #[test]
    fn discovery_evidence_rejects_passing_ineligible_alternative() {
        let (pkg, discover) = discovery_fixture(
            "normalisation,deseq2_vst,curated_baseline,false\n",
            serde_json::json!({
                "method_id": "deseq2_vst",
                "literature_eligible": false,
                "passes_default_eligibility_criteria": true,
                "recommended_tier": "alternative"
            }),
        );
        let outcome = DiscoveryEvidenceConsistencyRunner.run(&discover);
        assert!(matches!(outcome, ValidatorOutcome::Failed { .. }));
        drop(pkg);
    }

    #[test]
    fn discovery_evidence_accepts_supported_default() {
        let (pkg, discover) = discovery_fixture(
            "normalisation,deseq2_vst,primary_literature,true\n",
            serde_json::json!({
                "method_id": "deseq2_vst",
                "literature_eligible": true,
                "passes_default_eligibility_criteria": true,
                "recommended_tier": "defaultRecommended"
            }),
        );
        assert_eq!(
            DiscoveryEvidenceConsistencyRunner.run(&discover),
            ValidatorOutcome::Passed
        );
        drop(pkg);
    }

    #[test]
    fn discovery_evidence_uses_canonical_stage_class_for_aliased_task() {
        let pkg = TempDir::new().unwrap();
        let survey = pkg.path().join("runtime/outputs/survey_method_landscape");
        let discover = pkg
            .path()
            .join("runtime/outputs/discover_rnaseq_differential_expression");
        fs::create_dir_all(&survey).unwrap();
        fs::create_dir_all(&discover).unwrap();
        fs::write(
            survey.join("method_landscape.csv"),
            "axis,candidate_method,source_class,verified\n\
             differential_expression,deseq2,primary_literature,true\n",
        )
        .unwrap();
        fs::write(
            discover.join("task-spec.json"),
            serde_json::json!({
                "spec": {"stage_class": "differential_expression"}
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            discover.join("decision.json"),
            serde_json::json!({
                "candidate_pool_full": [{
                    "method_id": "deseq2",
                    "literature_eligible": true,
                    "passes_default_eligibility_criteria": true,
                    "recommended_tier": "defaultRecommended"
                }]
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(
            DiscoveryEvidenceConsistencyRunner.run(&discover),
            ValidatorOutcome::Passed
        );
        assert!(discovery_evidence_is_applicable(&discover));
    }

    #[test]
    fn discovery_evidence_does_not_apply_by_task_name_alone() {
        let pkg = TempDir::new().unwrap();
        let survey = pkg.path().join("runtime/outputs/survey_method_landscape");
        let discover = pkg.path().join("runtime/outputs/discover_fixture_method");
        fs::create_dir_all(&survey).unwrap();
        fs::create_dir_all(&discover).unwrap();
        fs::write(
            survey.join("method_landscape.csv"),
            "axis,candidate_method,source_class,verified\n",
        )
        .unwrap();
        fs::write(discover.join("decision.json"), "{}").unwrap();
        fs::write(discover.join("task-spec.json"), r#"{"spec":{}}"#).unwrap();
        assert!(!discovery_evidence_is_applicable(&discover));
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

    #[test]
    fn variant_af_spectrum_plausible_passes_right_skewed_unit_set() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({ "af_values": [0.01, 0.02, 0.05, 0.10, 0.40, 0.80] }),
        );
        let runner = VariantAfSpectrumPlausibleRunner;
        assert_eq!(runner.run(tmp.path()), ValidatorOutcome::Passed);
    }

    #[test]
    fn variant_af_spectrum_plausible_fails_out_of_unit_af() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({ "af_values": [0.1, 1.5, 0.2] }),
        );
        let runner = VariantAfSpectrumPlausibleRunner;
        match runner.run(tmp.path()) {
            ValidatorOutcome::Failed { message } => assert!(message.contains("1.5"), "{message}"),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn variant_af_spectrum_plausible_passes_homoplasmy_dominated_set() {
        // A high-median, homoplasmy-dominated AF spectrum is VALID for mtDNA
        // called against rCRS (most variants are fixed differences from the
        // reference at AF≈1.0). The Nekrutenko ground-truth key itself has a
        // pooled median of 0.9972, so the former `median <= 0.5` rule failed the
        // benchmark's own answer set. The validator now only guards AF ∈ [0, 1].
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({ "af_values": [0.90, 0.95, 0.99, 0.92] }),
        );
        let runner = VariantAfSpectrumPlausibleRunner;
        assert_eq!(runner.run(tmp.path()), ValidatorOutcome::Passed);
    }

    #[test]
    fn variant_af_spectrum_missing_field_is_errored() {
        let tmp = TempDir::new().unwrap();
        write_result_json(tmp.path(), serde_json::json!({ "summary": "ok" }));
        let runner = VariantAfSpectrumPlausibleRunner;
        assert!(matches!(
            runner.run(tmp.path()),
            ValidatorOutcome::Errored { .. }
        ));
    }

    #[test]
    fn variant_af_spectrum_runner_fails_closed_when_result_json_absent() {
        let tmp = TempDir::new().unwrap();
        // No result.json written -> required input absent.
        let runner = VariantAfSpectrumPlausibleRunner;
        let outcome = runner.run(tmp.path());
        // has_failures() counts only Failed, not Errored, so a missing
        // required input must surface as Failed to block. A present-but-
        // malformed result.json stays Errored (asserted above) — only the
        // entirely-absent measurement input fails closed.
        assert!(
            matches!(outcome, ValidatorOutcome::Failed { .. }),
            "missing required input must fail closed (Failed), got {outcome:?}"
        );
    }

    #[test]
    fn variant_filtered_count_consistency_passes_when_filtered_le_called() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({ "variant_count": 80, "called_variant_count": 100 }),
        );
        let runner = VariantFilteredCountConsistencyRunner;
        assert_eq!(runner.run(tmp.path()), ValidatorOutcome::Passed);
    }

    #[test]
    fn variant_filtered_count_consistency_fails_when_filtered_gt_called() {
        let tmp = TempDir::new().unwrap();
        write_result_json(
            tmp.path(),
            serde_json::json!({ "variant_count": 120, "called_variant_count": 100 }),
        );
        let runner = VariantFilteredCountConsistencyRunner;
        match runner.run(tmp.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(message.contains("exceeds"), "{message}")
            }
            other => panic!("expected Failed, got {other:?}"),
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
            // Harness-local variant-domain obligations are intentionally
            // not in core's starter registry (companions to the
            // goal-driven variant assertion arms).
            .filter(|id| !HARNESS_LOCAL_VARIANT_OBLIGATIONS.contains(id))
            // Harness-local provenance obligations are data-driven: the
            // harness unions them in from the result.json shape, so they
            // have no atom-declared entry to mirror into core.
            .filter(|id| !HARNESS_LOCAL_PROVENANCE_OBLIGATIONS.contains(id))
            // Discovery evidence consistency is likewise data-driven from
            // discover_*/decision.json plus the retained method landscape.
            .filter(|id| !HARNESS_LOCAL_DISCOVERY_OBLIGATIONS.contains(id))
            .collect();
        assert!(
            drifted.is_empty(),
            "runner obligation_ids not in canonical starter set: {drifted:?}; \
             canonical_starter_ids={canonical_starter_ids:?}"
        );
    }
}
