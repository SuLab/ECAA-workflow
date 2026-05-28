//! Regression: `compose_v4_dispatch_full` must populate
//! `PlanningContext.intent` from its args so the v4 forward/backward
//! search has data to walk. Without this, every v4 dispatch returns
//! `PartialDag` with no producer ("no producer atom in Registry produces
//! data:0951...").
//!
//! Two regressions:
//!
//! 1. **Public dispatch** — `compose_with_version_and_modalities_full`
//! at `composer_version=4` must propagate the caller's modality slice
//! into the v4 PlanningContext. We assert this by inspecting the
//! `compose_outcome` returned through `CompositionError::
//! ComposerV4OutcomeNotExecutable` for non-Validated outcomes, OR by
//! direct success when the lowering pipeline holds.
//!
//! 2. **Planner-level regression** — running the v4 planner with a
//! properly threaded context must produce a non-empty DAG containing
//! canonical bulk-RNA-seq operation atoms (`raw_qc` and
//! `differential_expression`). This is the "Task A unblocks the
//! search" surface; cycle / atom-selection / validate-companion
//! issues are deferred to Tasks B-D.

use std::collections::BTreeMap;

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
use scripps_workflow_core::composer_v4::{plan as v4_plan, planning_context_for_goal_with_intake};
use scripps_workflow_core::goal_spec::GoalSpec;
use scripps_workflow_core::workflow_contracts::data_product::DataProductContract;

/// Workspace-relative paths so the test runs under `cargo test
/// -p scripps-workflow-core`.
const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Bulk-RNA-seq DE goal mirrored from the parity-corpus fixture
/// (`testdata/v4-parity/bulk-rnaseq/request.txt`): IBD cohort, salmon
/// quant, DESeq2 DE on responder vs non-responder. The composer reads
/// `edam_data` + `edam_format` for goal seeding and `modifiers` for kind
/// disambiguation. Source prose is the SME utterance.
fn bulk_rnaseq_de_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Bulk RNA-seq differential expression analysis on an IBD cohort, \
             responder vs non-responder."
                .into(),
        ),
        confidence: 0.9,
    }
}

/// Planner-level regression: when
/// the v4 planner is given a context with `modality` + `project_class`
/// + `available_data` + `desired_outputs` populated, it must produce a
/// non-empty `WorkflowDag` containing the canonical goal-producing
/// atom (`differential_expression`) and an archetype-seed alternative
/// that contains the canonical bulk-RNA-seq scaffold (raw_qc through
/// pathway_enrichment).
///
/// The planner produces a 2-alternative slate: archetype seed
/// (modality match) + search seed (forward/backward). This test
/// asserts the slate shape. Without a populated intent the
/// dispatcher would otherwise return `PartialDag` with "no producer
/// atom in registry produces data:0951" — the search side has no
/// available_data to walk from and the archetype seed needs
/// `intent.modality` to disambiguate.
#[test]
fn planner_with_threaded_intent_produces_search_and_archetype_alternatives() {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let goal = bulk_rnaseq_de_goal();
    let available_data = vec![DataProductContract::sample_paired_fastq()];
    let mut ctx = planning_context_for_goal_with_intake(
        "v4_dispatch_intent_test",
        &goal,
        Some("bulk_rnaseq"),
        Some("bioinformatics"),
        &available_data,
    );
    // Make sure both alternatives stay in the slate so we can assert
    // on each. Default `max_alternatives = 3` is sufficient, but pin
    // explicitly so a future default change doesn't silently truncate.
    ctx.max_alternatives = 5;

    let result = v4_plan(&ctx, &goal, "bioinformatics", &atom_reg, &archetype_reg);

    // Primary must be DAG-bearing — Task A's whole job is to prevent
    // the no-alternative `PartialDag { unresolved_gaps: [no producer
    //...] }` outcome.
    use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
    match &result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. }
        | ComposeOutcome::PartialDag { dag, .. } => {
            assert!(
                !dag.nodes.is_empty(),
                "primary outcome carries an empty DAG; intent threading not active. \
                 primary={:?}",
                result.primary
            );
        }
        other => {
            panic!("v4 planner produced a non-DAG outcome with intent threaded; got: {other:?}")
        }
    }

    // The slate must include both an archetype-seed alternative (modality
    // matched bulk_rnaseq_de) AND a search-seed alternative (forward /
    // backward / meet-in-middle reached the goal). Both must exist to
    // confirm Task A wired the *full* intent (modality drives archetype
    // seed; available_data + desired_outputs drive search).
    let sources: std::collections::BTreeSet<&str> = result
        .alternatives
        .iter()
        .map(|a| a.source.as_str())
        .collect();
    assert!(
        sources.contains("archetype"),
        "expected an `archetype`-source alternative (proves intent.modality threaded); \
         got sources={sources:?}"
    );
    assert!(
        sources.contains("search"),
        "expected a `search`-source alternative (proves intent.available_data + \
         desired_outputs threaded); got sources={sources:?}"
    );

    // The archetype seed alternative must contain raw_qc +
    // differential_expression — that's the canonical bulk_rnaseq_de
    // archetype's required atoms, so its lifted DAG must include them.
    let archetype_alt = result
        .alternatives
        .iter()
        .find(|a| a.source == "archetype")
        .expect("archetype alternative");
    let archetype_ids: std::collections::BTreeSet<&str> = archetype_alt
        .dag
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .collect();
    assert!(
        archetype_ids.contains("raw_qc"),
        "archetype alternative must contain raw_qc (declared required by bulk_rnaseq_de \
         archetype); got {archetype_ids:?}"
    );
    assert!(
        archetype_ids.contains("differential_expression"),
        "archetype alternative must contain differential_expression; got {archetype_ids:?}"
    );

    // The search alternative must contain differential_expression
    // (backward search reaches it from the data:0951 desired output).
    let search_alt = result
        .alternatives
        .iter()
        .find(|a| a.source == "search")
        .expect("search alternative");
    let search_ids: std::collections::BTreeSet<&str> =
        search_alt.dag.nodes.iter().map(|n| n.id.as_str()).collect();
    assert!(
        search_ids.contains("differential_expression"),
        "search alternative must contain differential_expression (backward search target); \
         got {search_ids:?}"
    );
}

/// The public dispatcher
/// (`compose_with_version_and_modalities_full`) must thread the
/// modality slice through to the v4 PlanningContext. This is the
/// integration-level regression: pre-fix every production v4 dispatch
/// returned `PartialDag, no producer`; post-fix the planner walks the
/// registry from the modality-derived available_data seed.
///
/// The meet-in-middle edge construction
/// now respects topological rank computed from the atom registry's
/// `depends_on` field, so cyclic edges no longer surface in the
/// lowered DAG. The remaining production-path failure modes are
/// orthogonal:
///
/// - `MethodChoiceUnresolved` — atoms like `differential_transcript_usage`
/// declare `method_choice.deferred_to: discover_dtu_method`, but
/// that discovery atom doesn't exist in the registry. The v4
/// search reaches DTU-style atoms via shared count-matrix ports
/// even when the SME's intent is bulk-rnaseq DE. Task C
/// territory: tighten the search frontier so atoms whose
/// `method_choice` references missing discovery atoms get
/// filtered out (or auto-add the missing discovery atoms).
///
/// We assert that the failure mode advanced past CycleDetected.
/// The pre-Task-B "no producer" gap (Task A) and the immediately-
/// post-A `CycleDetected` (Task B input) must both be absent —
/// otherwise the prior tasks regressed.
#[test]
fn dispatch_v4_threads_modality_through_public_entry_point() {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let goal = bulk_rnaseq_de_goal();
    let modalities: Vec<&str> = vec!["bulk_rnaseq"];

    let result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4, // v4
        &modalities,
        None,
        None,
        None,
    );

    use scripps_workflow_core::composer::CompositionError;
    match result {
        Ok(output) => {
            // Tasks A + B + C unblocked the production dispatch path:
            // intent threaded → search produces a DAG → cycle filter +
            // atom-selection narrowing keep validate_composition happy.
            // The composition's atom set is scoring-dependent (the
            // planner picks the lowest-scored alternative; search seed
            // and archetype seed compete). What's load-bearing is that
            // the composition is non-empty and contains the canonical
            // goal producer (`differential_expression`). The exact
            // pre-stages (`raw_qc` vs `qc_preprocessing` etc.) are
            // alternative-dependent and exercised by the parity corpus.
            let stage_ids: std::collections::BTreeSet<&str> = output
                .composition
                .atoms
                .iter()
                .map(|c| c.stage_id.as_str())
                .collect();
            assert!(
                !stage_ids.is_empty(),
                "v4 dispatch returned an empty composition — search regressed"
            );
            assert!(
                stage_ids.iter().any(|id| id.contains("differential")),
                "expected differential_expression atom (goal producer); got {stage_ids:?}"
            );
            // Post-Task-C: agentic + unresolvable-method-choice atoms
            // must be filtered out. Spot-check the two highest-leak
            // offenders the parity corpus surfaced.
            assert!(
                !stage_ids.contains("differential_transcript_usage"),
                "differential_transcript_usage (unresolvable method_choice) leaked into \
                 v4 production dispatch composition: {stage_ids:?}"
            );
            assert!(
                !stage_ids.contains("bio_mystery_query"),
                "bio_mystery_query (agentic) leaked into modality-routed v4 composition: \
                 {stage_ids:?}"
            );
        }
        Err(CompositionError::ComposerV4OutcomeNotExecutable {
            outcome_kind, gaps, ..
        }) if outcome_kind == "PartialDag"
            && gaps
                .iter()
                .any(|g| g.contains("no producer atom in registry")) =>
        {
            // Pre-Task-A failure mode: search found nothing. Persistence
            // here means Task A's intent threading regressed.
            panic!(
                "v4 dispatch still returns PartialDag with 'no producer' — Task A intent \
                 threading regressed. gaps={gaps:?}"
            );
        }
        Err(CompositionError::CycleDetected { cycle }) => {
            // Pre-Task-B failure mode. Persistence here means the
            // role-rank edge filter regressed.
            panic!(
                "v4 dispatch still returns CycleDetected — Task B role-rank filter \
                 regressed. cycle={cycle:?}"
            );
        }
        Err(CompositionError::MethodChoiceUnresolved { atom, deferred_to }) => {
            // Pre-Task-C failure mode. Persistence here means Task C's
            // atom-eligibility filter regressed: the search is reaching
            // atoms whose `method_choice.deferred_to` references a
            // missing discovery atom.
            panic!(
                "v4 dispatch still returns MethodChoiceUnresolved — Task C atom-selection \
                 narrowing regressed. atom={atom} deferred_to={deferred_to}"
            );
        }
        Err(other) => {
            panic!(
                "unexpected error from v4 dispatch — Tasks A + B + C should produce Ok; \
                 got {other:?}"
            );
        }
    }
}
