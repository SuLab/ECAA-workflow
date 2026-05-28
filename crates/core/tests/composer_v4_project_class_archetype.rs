//! Regression: project-class scenarios must select
//! project-class-routed archetypes, not modality-routed.
//!
//! The bug: `archetype_reg.find_match_with_evidence_modality_kind` scored
//! `project_class_match = +1`. With a non-bioinformatics target, the
//! matching project-class archetype scored 6 (3 goal_data + 2 goal_format
//! + 1 project_class), and the four bioinformatics archetypes that share
//! the same `(data:0951, format:3475)` triple scored 5 each. The 5%
//! tie-window cutoff is `floor(6 * 0.95) = 5`, so all five archetypes
//! qualified and `try_archetype_seed` skipped the seed entirely (the
//! matcher refused to commit on a tie). v4's forward / backward search
//! then produced the bulk-rnaseq pipeline shape — fundamentally wrong
//! for a SARIMA forecast or a Phase-III RCT endpoint analysis.
//!
//! The fix: project_class is a hard partition, not a tie-breaker. An
//! archetype's `project_class` must equal the target's; otherwise it's
//! filtered out before scoring. This makes `time_series_forecast` and
//! `clinical_trial_analysis` archetypes the only candidates for their
//! respective scenarios, and bioinformatics archetypes the only
//! candidates for bioinformatics scenarios.

use std::collections::BTreeSet;

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::goal_spec::GoalSpec;

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Build the goal the parity-corpus fallback synthesizes for a
/// time-series scenario when no goal-pattern matches the prose.
fn time_series_goal() -> GoalSpec {
    let mut modifiers = std::collections::BTreeMap::new();
    modifiers.insert("kind".into(), "forecast".into());
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("time-series forecast".into()),
        confidence: 0.5,
    }
}

/// Build the goal the parity-corpus fallback synthesizes for a
/// clinical-trial scenario when no goal-pattern matches the prose.
fn clinical_trial_goal() -> GoalSpec {
    let mut modifiers = std::collections::BTreeMap::new();
    modifiers.insert("kind".into(), "clinical_trial_analysis".into());
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("clinical-trial endpoint analysis".into()),
        confidence: 0.5,
    }
}

fn gwas_coloc_goal() -> GoalSpec {
    let mut modifiers = std::collections::BTreeMap::new();
    modifiers.insert("kind".into(), "gwas_coloc".into());
    GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some(
            "PGC3 schizophrenia GWAS plus GTEx v8 coloc, 1000G Phase 3 EUR LD panel".into(),
        ),
        confidence: 0.95,
    }
}

fn load_registries() -> (AtomRegistry, ArchetypeRegistry) {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    (atom_reg, archetype_reg)
}

/// Clinical-trial dispatch must select the
/// `clinical_trial_analysis` archetype, not bulk-rnaseq.
///
/// Update — the clinical-trial archetype now declares
/// clinical-specific atoms `data_import`, `clinical_endpoint_analysis`,
/// `clinical_safety_summary`, `clinical_subgroup_analysis`,
/// `clinical_sensitivity_analysis`, `reporting`, `final_reporting`.
/// The legacy `qc_preprocessing` + `differential_expression` bridge
/// was retired because those atoms expect an RNA-seq count matrix and
/// the v4 planner couldn't bridge the CDISC ADaM tabular shape into
/// them (`GoalUnreachable { goal: "data:0951 (format:3475)" }`).
/// Bulk-rnaseq contributes `alignment`, `quantification`,
/// `normalisation`, `batch_correction`, etc. We assert the archetype's
/// required atoms appear and the bulk-rnaseq scaffolding does not
/// (the planner may add a `validate_*` companion for each
/// result-producing atom — those are allowed, see Task G).
#[test]
fn clinical_trial_uses_clinical_archetype() {
    let (atom_reg, archetype_reg) = load_registries();
    let goal = clinical_trial_goal();

    let output = compose_with_version_and_modalities_full(
        &goal,
        "clinical_trial",
        &atom_reg,
        &archetype_reg,
        4,
        &["generic_omics"],
        None,
        None,
        None,
    )
    .expect("v4 dispatch must succeed for clinical-trial scenario");

    let task_ids: BTreeSet<String> = output
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect();

    // Clinical-trial archetype required atoms must appear.
    for required in [
        "data_import",
        "clinical_endpoint_analysis",
        "reporting",
        "final_reporting",
    ] {
        assert!(
            task_ids.contains(required),
            "expected {required:?} (clinical_trial_analysis archetype atom); got {task_ids:?}"
        );
    }

    // Bulk-rnaseq scaffolding atoms must NOT appear. These are the
    // load-bearing pollution markers — if any of these surface, the
    // planner picked a bulk-rnaseq archetype (or fell back to forward
    // search) instead of `clinical_trial_analysis`. `qc_preprocessing`
    // and `differential_expression` are now also forbidden — they
    // belong to the RNA-seq pipeline and would indicate the planner
    // resurrected the retired bridge.
    for forbidden in [
        "alignment",
        "sequence_trimming",
        "quantification",
        "normalisation",
        "batch_correction",
        "clustering",
        "dimensionality_reduction",
        "integration",
        "peptide_search",
        "protein_quantification",
        "diversity_analysis",
        "long_read_benchmarking",
        "qc_preprocessing",
        "differential_expression",
    ] {
        assert!(
            !task_ids.contains(forbidden),
            "bulk-rnaseq scaffolding atom {forbidden:?} appeared in clinical-trial \
             composition; the planner is selecting a modality-routed archetype \
             instead of clinical_trial_analysis. Got: {task_ids:?}"
        );
    }
}

#[test]
fn gwas_modality_overrides_false_clinical_project_class() {
    let (atom_reg, archetype_reg) = load_registries();
    let goal = gwas_coloc_goal();

    let output = compose_with_version_and_modalities_full(
        &goal,
        "clinical_trial",
        &atom_reg,
        &archetype_reg,
        4,
        &["gwas"],
        None,
        None,
        None,
    )
    .expect("v4 dispatch must succeed for GWAS coloc even if project class was misrouted");

    let task_ids: BTreeSet<String> = output
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.as_str().to_owned())
        .collect();

    for required in [
        "data_acquisition",
        "gwas_summary_harmonization",
        "colocalization",
    ] {
        assert!(
            task_ids.contains(required),
            "expected GWAS coloc atom {required:?}; got {task_ids:?}"
        );
    }

    for forbidden in [
        "data_import",
        "clinical_endpoint_analysis",
        "clinical_safety_summary",
        "clinical_subgroup_analysis",
        "clinical_sensitivity_analysis",
    ] {
        assert!(
            !task_ids.contains(forbidden),
            "clinical atom {forbidden:?} leaked into GWAS composition; got {task_ids:?}"
        );
    }
}

/// Time-series scenarios must NOT select a
/// bulk-rnaseq archetype.
///
/// Updated for closure plan B.1: the `time_series_forecast`
/// archetype now declares its own producer chain
/// (`time_series_decompose` → `time_series_model_fit` →
/// `time_series_forecast_evaluate`) and v4 dispatch returns `Ok`. The
/// `GoalUnreachable` fallback branch is retained as a tolerated outcome
/// in case the archetype is later split or atoms relocate, but the
/// load-bearing assertion is that no bulk-rnaseq scaffolding leaks in.
#[test]
fn time_series_does_not_use_bulk_rnaseq_archetype() {
    use scripps_workflow_core::composer::CompositionError;

    let (atom_reg, archetype_reg) = load_registries();
    let goal = time_series_goal();

    match compose_with_version_and_modalities_full(
        &goal,
        "time_series_forecast",
        &atom_reg,
        &archetype_reg,
        4,
        &["generic_omics"],
        None,
        None,
        None,
    ) {
        Ok(output) => {
            let task_ids: BTreeSet<String> = output
                .composition
                .atoms
                .iter()
                .map(|c| c.stage_id.to_string())
                .collect();

            // When dispatch succeeds, it must be via the
            // time_series_forecast archetype — assert the closure-plan
            // B.1 atoms are present.
            for required in [
                "time_series_decompose",
                "time_series_model_fit",
                "time_series_forecast_evaluate",
            ] {
                assert!(
                    task_ids.contains(required),
                    "expected {required:?} (time_series_forecast archetype atom); got {task_ids:?}"
                );
            }

            // Bulk-rnaseq scaffolding must NOT appear.
            for forbidden in [
                "alignment",
                "sequence_trimming",
                "quantification",
                "normalisation",
                "batch_correction",
                "clustering",
                "dimensionality_reduction",
                "integration",
                "peptide_search",
                "protein_quantification",
                "diversity_analysis",
                "long_read_benchmarking",
                "differential_expression",
            ] {
                assert!(
                    !task_ids.contains(forbidden),
                    "bulk-rnaseq scaffolding atom {forbidden:?} appeared in time-series \
                     composition; the planner is selecting a modality-routed archetype \
                     instead of time_series_forecast. Got: {task_ids:?}"
                );
            }
        }
        Err(CompositionError::GoalUnreachable { goal }) => {
            // Tolerated regression-recovery path: archetype is reachable
            // post-B.1, but if a later edit removes a producer atom the
            // dispatch falls back here. Surface a clear error message so
            // the next failure mode is debuggable.
            panic!(
                "time-series dispatch returned GoalUnreachable for {goal} but closure plan \
                 B.1 should have closed the producer gap; check that time_series_decompose, \
                 time_series_model_fit, and time_series_forecast_evaluate atoms exist and \
                 that the archetype references them."
            );
        }
        Err(other) => {
            panic!(
                "time-series dispatch returned unexpected error {other:?}; expected Ok with \
                 time_series_forecast atoms post-B.1"
            );
        }
    }
}

/// Corollary: bioinformatics scenarios must NOT
/// select project-class-routed archetypes (clinical_trial_analysis,
/// time_series_forecast). The same partition rule that gates project-
/// class targets keeps bioinformatics targets from accidentally picking
/// a clinical archetype when their goal triple happens to align.
///
/// This is a regression guard: when we make project_class a hard
/// partition, we must also confirm bulk-rnaseq dispatch still hits
/// `bulk_rnaseq_de` (and similar) rather than skipping over
/// project-class-tagged archetypes.
#[test]
fn bioinformatics_target_skips_project_class_archetypes() {
    let (atom_reg, archetype_reg) = load_registries();

    // Bulk-rnaseq DE intent. The composer should pick the
    // `bulk_rnaseq_de` archetype (modality_hint=bulk_rnaseq).
    let mut modifiers = std::collections::BTreeMap::new();
    modifiers.insert("kind".into(), "differential_expression".into());
    let goal = GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers,
        source_prose: Some("bulk RNA-seq differential expression".into()),
        confidence: 0.9,
    };
    let output = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("v4 dispatch must succeed for bulk-rnaseq scenario");

    let task_ids: BTreeSet<String> = output
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect();

    // Bulk-rnaseq archetype atoms expected.
    assert!(
        task_ids.contains("differential_expression"),
        "expected differential_expression in bulk-rnaseq composition; got {task_ids:?}"
    );
    // Reporting layer should still appear.
    assert!(
        task_ids.contains("reporting"),
        "expected reporting in bulk-rnaseq composition; got {task_ids:?}"
    );
}

/// Direct archetype-registry test. Independent of the
/// dispatch + lower + validate pipeline, the matcher must return the
/// project-class archetype (and ONLY project-class archetypes whose
/// `project_class` exactly matches the target) for a project-class
/// target.
#[test]
fn matcher_filters_by_project_class() {
    let (_atom_reg, archetype_reg) = load_registries();

    // Target: clinical_trial. Should ONLY match
    // clinical_trial_analysis (the only archetype with that
    // project_class).
    let matches = archetype_reg.find_match_with_evidence_modality_kind(
        "data:0951",
        Some("format:3475"),
        "clinical_trial",
        None,
        None,
    );
    let ids: Vec<&str> = matches.iter().map(|m| m.archetype.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["clinical_trial_analysis"],
        "clinical_trial target must match only clinical_trial_analysis archetype; got {ids:?}"
    );

    // Target: time_series_forecast. Should ONLY match
    // time_series_forecast.
    let matches = archetype_reg.find_match_with_evidence_modality_kind(
        "data:0951",
        Some("format:3475"),
        "time_series_forecast",
        None,
        None,
    );
    let ids: Vec<&str> = matches.iter().map(|m| m.archetype.id.as_str()).collect();
    assert_eq!(
        ids,
        vec!["time_series_forecast"],
        "time_series_forecast target must match only time_series_forecast archetype; got {ids:?}"
    );

    // Target: bioinformatics. Must NOT match clinical_trial_analysis
    // or time_series_forecast even though they share goal_data +
    // goal_format with bulk_rnaseq_de.
    let matches = archetype_reg.find_match_with_evidence_modality_kind(
        "data:0951",
        Some("format:3475"),
        "bioinformatics",
        Some("bulk_rnaseq"),
        None,
    );
    let ids: BTreeSet<&str> = matches.iter().map(|m| m.archetype.id.as_str()).collect();
    assert!(
        !ids.contains("clinical_trial_analysis"),
        "bioinformatics target must NOT include clinical_trial_analysis; got {ids:?}"
    );
    assert!(
        !ids.contains("time_series_forecast"),
        "bioinformatics target must NOT include time_series_forecast; got {ids:?}"
    );
    assert!(
        ids.contains("bulk_rnaseq_de"),
        "bioinformatics target with modality=bulk_rnaseq must include bulk_rnaseq_de; got {ids:?}"
    );
}
