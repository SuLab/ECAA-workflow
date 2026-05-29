//! Cross-omics dispatch must include the per-modality namespaced
//! atoms (`rnaseq_*`, `proteomics_*`) plus the cross-omics
//! integration step (`cross_omics_thematic_comparison`).
//!
//! Contract:
//!
//! 1. `compose_v4_dispatch_full` threads `target_modalities[1..]`
//! into `PlanningContext::additional_modalities`.
//! 2. `try_archetype_seed` calls
//! `archetype_reg.find_match_cross_omics(...)` first when
//! `additional_modalities` is non-empty.
//! 3. The lifted seed honors per-atom `alias` so the namespaced
//! parallel-pipeline scaffold materializes cleanly.
//! 4. The cross-omics match is treated as definitive
//! (`evidence.modality_match = 2`) so the search seed can't
//! silently outrank it on raw node count.
//!
//! Without these, the planner sees only the primary modality, the
//! single-modality archetype matcher filters out cross-omics
//! archetypes, and the load-bearing `cross_omics_thematic_comparison`
//! join atom goes missing.
//!
//! Ground truth read from
//! `tests/conversation-fixtures/fixtures/v4-parity/cross-omics/v2-baseline.json`.

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// Cross-omics RNA-seq + proteomics goal mirrored from the parity
/// corpus fixture (`testdata/v4-parity/cross-omics/request.txt`):
/// matched-donor RNA-seq DE + mass-spec proteomics differential
/// abundance with shared-signal cross-omics comparison. The goal
/// triple `(data:0951, format:3475, bioinformatics)` matches the
/// `cross_omics_rnaseq_proteomics` archetype's declared shape; the
/// `cross_omics_modalities: [bulk_rnaseq, proteomics]` set-equality
/// match drives the seed selection.
fn cross_omics_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Cross-omics differential expression integration of bulk RNA-seq and \
             mass spectrometry proteomics on matched donor samples."
                .into(),
        ),
        confidence: 0.9,
    }
}

fn dispatch_cross_omics() -> BTreeSet<String> {
    let atom_reg = AtomRegistry::load_from_dir(std::path::Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(std::path::Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");

    let goal = cross_omics_goal();
    // Modality slice must include both primary (`bulk_rnaseq`) AND
    // the additional modality (`proteomics`). The dispatcher
    // (`compose_v4_dispatch_full`) reads `target_modalities[0]` as
    // primary and `target_modalities[1..]` as additional; the v4
    // PlanningContext threads both through to the planner.
    let modalities: Vec<&str> = vec!["bulk_rnaseq", "proteomics"];

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

    let output = result.expect("v4 cross-omics dispatch must succeed post-Task-I");
    output
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect()
}

/// The cross-omics archetype synthesizes two parallel namespaced
/// pipelines (RNA-seq + proteomics). The v4 emission must contain
/// representative stages from each branch — without namespacing the
/// emission collapses to a single bare-name pipeline (the
/// pre-Task-I gap documented in
/// `tests/conversation-fixtures/fixtures/v4-parity/cross-omics/v4-emission/TRIAGE.md`).
#[test]
fn cross_omics_includes_per_modality_atoms() {
    let stage_ids = dispatch_cross_omics();

    // Spot-check one stage per branch. The RNA-seq branch must
    // contribute `rnaseq_alignment` (no proteomics analog) and the
    // proteomics branch must contribute `proteomics_peptide_search`
    // (no RNA-seq analog). Together they confirm both
    // namespace-prefixed pipelines materialized.
    assert!(
        stage_ids.contains("rnaseq_alignment"),
        "v4 cross-omics emission missing `rnaseq_alignment` (RNA-seq branch \
         not namespaced); got stage ids: {stage_ids:?}"
    );
    assert!(
        stage_ids.contains("proteomics_peptide_search"),
        "v4 cross-omics emission missing `proteomics_peptide_search` (proteomics \
         branch not namespaced); got stage ids: {stage_ids:?}"
    );

    // Both DE branches must reach their differential-* terminal —
    // these feed the cross-omics integration step.
    assert!(
        stage_ids.contains("rnaseq_differential_expression"),
        "v4 cross-omics emission missing `rnaseq_differential_expression`; \
         got stage ids: {stage_ids:?}"
    );
    assert!(
        stage_ids.contains("proteomics_differential_abundance"),
        "v4 cross-omics emission missing `proteomics_differential_abundance`; \
         got stage ids: {stage_ids:?}"
    );

    // The bare-name pipeline shadows MUST NOT appear — they're the
    // signature of the pre-Task-I gap (search seed produced a single
    // unprefixed pipeline because the cross-omics archetype was
    // never seeded).
    assert!(
        !stage_ids.contains("alignment"),
        "v4 cross-omics emission contains bare `alignment` (pre-Task-I \
         single-pipeline shadow); got stage ids: {stage_ids:?}"
    );
}

/// The cross-omics archetype declares an integration atom
/// (`cross_omics_thematic_comparison`, aliased from `reporting`) that
/// joins the two DE branches. v2 emits this atom; v4 must too. Goes
/// missing if the archetype seed doesn't fire.
#[test]
fn cross_omics_includes_integration_step() {
    let stage_ids = dispatch_cross_omics();

    assert!(
        stage_ids.contains("cross_omics_thematic_comparison"),
        "v4 cross-omics emission missing `cross_omics_thematic_comparison` — \
         the archetype's load-bearing join atom that fuses the RNA-seq + \
         proteomics differential branches. got stage ids: {stage_ids:?}"
    );

    // The terminal reporting stage must also be present — both v2 and
    // the archetype declare `final_reporting` after the integration.
    assert!(
        stage_ids.contains("final_reporting"),
        "v4 cross-omics emission missing `final_reporting` (terminal \
         report); got stage ids: {stage_ids:?}"
    );
}
