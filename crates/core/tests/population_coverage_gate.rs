//! v3 P9 §11.X integration tests — population-coverage gating.
//!
//! Tests the planner-side gate in `composer_v4::planner::classify_outcome_with_policy`
//! that consumes `PopulationCoverageStatement`s and emits typed
//! `RefusalKind::PopulationOutOfCoverage` outcomes. Driven by:
//!
//! - `PolicyContext` carrying a clinical-trial bundle (the
//! `ValidatedNodesOnly` check is the canonical clinical signal).
//! - `WorkflowIntent.sample_cohort` set to a `CohortDescriptor`.
//! - `PlanningContext.population_coverage_dir` pointing at a tmpdir of
//! YAML coverage statements.
//!
//! The gate runs against the workflow's coverage statement, not the
//! user's identity (framing constraint per v3 §11.X). A `PopulationWaiver`
//! in any active bundle suppresses the refusal.

use scripps_workflow_core::composer_v4::scoring::{ScoringTuple, ScoringValue};
use scripps_workflow_core::composer_v4::PlanningContext;
use scripps_workflow_core::policy_context::PolicyContext;
use scripps_workflow_core::population_coverage::{CohortDescriptor, PopulationWaiver};
use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use scripps_workflow_core::workflow_contracts::refusal_kind::RefusalKind;
use scripps_workflow_core::workflow_contracts::task_node::WorkflowDag;
use scripps_workflow_core::workflow_contracts::workflow_intent::WorkflowIntent;

/// Build a tmpdir + one coverage statement file inside it. Returns
/// the dir and the workflow_id used in the file.
fn tmpdir_with_coverage(yaml: &str, workflow_id: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join(format!("{workflow_id}.yaml"));
    std::fs::write(&path, yaml).expect("write yaml");
    (dir, path)
}

/// A pediatric-solid-tumor sample cohort that the
/// `rnaseq-de-clinical` workflow has NOT been validated on.
fn uncovered_cohort() -> CohortDescriptor {
    CohortDescriptor {
        label: "Pediatric solid tumor".into(),
        population_code: None,
        age_band: Some("pediatric".into()),
        sample_type: Some("solid_tumor".into()),
        n: Some(20),
    }
}

/// An adult-bulk-tissue sample cohort that matches one of the validated
/// cohorts for `rnaseq-de-clinical` via the age_band + sample_type pair.
fn covered_cohort() -> CohortDescriptor {
    CohortDescriptor {
        label: "GTEx-like cohort".into(),
        population_code: None,
        age_band: Some("adult".into()),
        sample_type: Some("bulk_tissue".into()),
        n: Some(100),
    }
}

const COVERAGE_YAML: &str = r#"
workflow_id: rnaseq-de-clinical
validated_cohorts:
  - label: "GTEx v8 normal tissue"
    age_band: adult
    sample_type: bulk_tissue
    n: 17382
  - label: "TCGA pan-cancer"
    age_band: adult
    sample_type: solid_tumor
    n: 11315
explicitly_untested:
  - label: "Pediatric solid tumor"
    age_band: pediatric
    sample_type: solid_tumor
citations:
  - "doi:10.1038/s41467-019-12873-4"
"#;

fn clinical_policy_context() -> PolicyContext {
    PolicyContext::empty().with_bundle(PolicyContext::clinical_trial_bundle())
}

fn non_clinical_policy_context() -> PolicyContext {
    PolicyContext::empty()
}

/// Build a planning context with the given coverage dir, sample cohort,
/// and DAG source_template (archetype id).
fn build_ctx(
    coverage_dir: std::path::PathBuf,
    cohort: Option<CohortDescriptor>,
) -> PlanningContext {
    let intent = WorkflowIntent {
        id: "test_session".into(),
        sample_cohort: cohort,
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.population_coverage_dir = Some(coverage_dir);
    ctx
}

fn dag_with_archetype(archetype_id: &str) -> WorkflowDag {
    WorkflowDag {
        id: format!("dag_{archetype_id}"),
        nodes: Vec::new(),
        edges: Vec::new(),
        assumptions: Default::default(),
        source_template: Some(archetype_id.into()),
        ..Default::default()
    }
}

/// Neutral scoring tuple — no per-axis rejections so the planner reaches
/// the population-coverage gate (Pass on every axis = default).
fn neutral_score() -> ScoringTuple {
    ScoringTuple {
        hard_policy_violation: ScoringValue::Pass,
        required_contract_unsatisfied: ScoringValue::Pass,
        ..Default::default()
    }
}

#[test]
fn clinical_workflow_refuses_uncovered_cohort() {
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), Some(uncovered_cohort()));
    let dag = dag_with_archetype("rnaseq-de-clinical");
    let policy = clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    match outcome {
        ComposeOutcome::Refusal { report } => match report.kind {
            RefusalKind::PopulationOutOfCoverage {
                workflow_id,
                sample_label,
                validated_labels,
                suggested_waiver_authority,
            } => {
                assert_eq!(workflow_id, "rnaseq-de-clinical");
                assert_eq!(sample_label, "Pediatric solid tumor");
                assert_eq!(suggested_waiver_authority, "clinical_lead");
                assert_eq!(validated_labels.len(), 2);
                assert!(validated_labels.contains(&"GTEx v8 normal tissue".to_string()));
                assert!(validated_labels.contains(&"TCGA pan-cancer".to_string()));
                assert!(!report.unblock_paths.is_empty());
            }
            other => panic!("expected PopulationOutOfCoverage, got {:?}", other),
        },
        other => panic!("expected Refusal, got {:?}", other),
    }
}

#[test]
fn clinical_workflow_admits_covered_cohort() {
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), Some(covered_cohort()));
    let dag = dag_with_archetype("rnaseq-de-clinical");
    let policy = clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "covered cohort should not produce PopulationOutOfCoverage refusal, got {:?}",
        outcome
    );
}

#[test]
fn waiver_overrides_refusal() {
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), Some(uncovered_cohort()));
    let dag = dag_with_archetype("rnaseq-de-clinical");

    // Build a clinical policy WITH an active waiver for rnaseq-de-clinical.
    let mut clinical = PolicyContext::clinical_trial_bundle();
    clinical.population_waivers.push(PopulationWaiver {
        workflow_id: "rnaseq-de-clinical".into(),
        waiving_authority: "clinical_lead".into(),
        rationale: "Pediatric IRB acknowledged off-label use".into(),
        waived_at: "2026-05-11T12:00:00Z".into(),
        policy_rule_id: "population_coverage:rnaseq-de-clinical".into(),
    });
    let policy = PolicyContext::empty().with_bundle(clinical);

    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "waiver should suppress PopulationOutOfCoverage refusal, got {:?}",
        outcome
    );
}

#[test]
fn non_clinical_session_skips_gate() {
    // No clinical bundle = no clinical session = no gate even when the
    // cohort would otherwise fail.
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), Some(uncovered_cohort()));
    let dag = dag_with_archetype("rnaseq-de-clinical");
    let policy = non_clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "non-clinical session should skip the gate entirely, got {:?}",
        outcome
    );
}

#[test]
fn unset_sample_cohort_skips_gate() {
    // Same setup but without a sample_cohort on the intent.
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), None);
    let dag = dag_with_archetype("rnaseq-de-clinical");
    let policy = clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "no sample cohort = no gate, got {:?}",
        outcome
    );
}

#[test]
fn missing_coverage_file_short_circuits() {
    // Archetype id has no matching.yaml file in the coverage dir — fall
    // through to no-gate. (Not every archetype is required to declare a
    // coverage statement; only those that have been validated against
    // specific cohorts.)
    let dir = tempfile::tempdir().expect("tmpdir"); // empty dir
    let ctx = build_ctx(dir.path().to_path_buf(), Some(uncovered_cohort()));
    let dag = dag_with_archetype("rnaseq-de-clinical");
    let policy = clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "missing coverage file should not refuse; got {:?}",
        outcome
    );
}

#[test]
fn search_only_dag_has_no_archetype_id_and_skips_gate() {
    // A DAG with no `source_template` (search-derived only, no archetype
    // seed) has nothing to compare against — the gate short-circuits.
    let (dir, _) = tmpdir_with_coverage(COVERAGE_YAML, "rnaseq-de-clinical");
    let ctx = build_ctx(dir.path().to_path_buf(), Some(uncovered_cohort()));
    let dag = WorkflowDag {
        id: "dag_search".into(),
        nodes: Vec::new(),
        edges: Vec::new(),
        assumptions: Default::default(),
        source_template: None, // no archetype
        ..Default::default()
    };
    let policy = clinical_policy_context();
    let outcome = scripps_workflow_core::composer_v4::planner::classify_outcome_with_policy(
        &dag,
        &neutral_score(),
        &policy,
        &ctx,
    );
    assert!(
        !matches!(
            outcome,
            ComposeOutcome::Refusal {
                report: scripps_workflow_core::workflow_contracts::outcome::RefusalReport {
                    kind: RefusalKind::PopulationOutOfCoverage { .. },
                    ..
                }
            }
        ),
        "search-only DAG should skip the gate, got {:?}",
        outcome
    );
}

#[test]
fn refusal_report_validates_with_unblock_paths() {
    use scripps_workflow_core::workflow_contracts::outcome::RefusalReport;
    // The constructor must produce a report that passes F21 validation.
    let report = RefusalReport::population_out_of_coverage(
        "rnaseq-de-clinical",
        "Pediatric solid tumor",
        vec!["GTEx v8 normal tissue".into(), "TCGA pan-cancer".into()],
        "clinical_lead",
    );
    assert!(
        report.validate().is_ok(),
        "constructed report must pass F21 validate(); got {:?}",
        report.validate()
    );
    assert!(report.unblock_paths.len() >= 2);
    assert!(report.statement.contains("Pediatric solid tumor"));
    assert!(report.statement.contains("rnaseq-de-clinical"));
}
