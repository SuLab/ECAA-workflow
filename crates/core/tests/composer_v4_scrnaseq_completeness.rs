//! v4 scrnaseq dispatch must include the load-bearing canonical
//! atoms `cell_type_annotation` and `differential_expression`. The
//! planner biases scoring toward the archetype seed when its match
//! evidence is "definitive" (modality_hint + goal_data + goal_format
//! all matched), and uses the atom's rich `inputs:` / `outputs:`
//! ports during lift so the archetype lift's edge-proofs reflect
//! real port-typed compatibility instead of stale
//! `edam_data`-synthesized mismatches that would flip
//! `score.required_contract_unsatisfied` to `Reject`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
use ecaa_workflow_core::goal_spec::GoalSpec;

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// scrnaseq goal mirrored from
/// `testdata/v4-parity/scrnaseq/request.txt` and the keyword
/// classifier's `scrnaseq_annotation` goal pattern in
/// `config/modality-keywords.yaml`. The deliverable is "annotated
/// AnnData with celltype identification" — `data:3917` (Count matrix)
/// in `format:3590` (HDF5), modifier `kind = scrnaseq_annotation`.
fn scrnaseq_annotation_goal() -> GoalSpec {
    let mut modifiers = BTreeMap::new();
    modifiers.insert("kind".into(), "scrnaseq_annotation".into());
    GoalSpec {
        edam_data: "data:3917".into(),
        edam_format: Some("format:3590".into()),
        modifiers,
        source_prose: Some(
            "Single-cell RNA-seq clustering and cell type annotation across public \
             intervertebral disc datasets."
                .into(),
        ),
        confidence: 0.9,
    }
}

/// Drive the v4 production dispatch for scrnaseq and return the set
/// of stage ids in the resulting composition. Mirrors the parity
/// corpus's `emit_v4` production-path attempt so the regression covers
/// the same surface SMEs hit through the conversation crate.
fn task_ids_for_scrnaseq() -> BTreeSet<String> {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let goal = scrnaseq_annotation_goal();
    let modalities: Vec<&str> = vec!["single_cell_rnaseq"];
    let output = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4,
        &modalities,
        None,
        None,
        None,
    )
    .expect("v4 dispatch must produce an executable composition for scrnaseq");
    output
        .composition
        .atoms
        .iter()
        .map(|c| c.stage_id.to_string())
        .collect()
}

#[test]
fn scrnaseq_includes_cell_type_annotation() {
    let task_ids = task_ids_for_scrnaseq();
    assert!(
        task_ids.contains("cell_type_annotation"),
        "v4 scrnaseq dispatch must include cell_type_annotation (the canonical \
         goal atom for scrnaseq_annotation against data:3917 / format:3590); \
         got {task_ids:?}"
    );
}

#[test]
fn scrnaseq_includes_differential_expression() {
    let task_ids = task_ids_for_scrnaseq();
    assert!(
        task_ids.contains("differential_expression"),
        "v4 scrnaseq dispatch must include differential_expression (cluster-vs-rest \
         comparisons declared as required by the single_cell_de archetype); \
         got {task_ids:?}"
    );
}

/// The synthesized `validate_*` companions for the load-bearing
/// scrnaseq atoms must surface in the v4 composition alongside the
/// operation atoms; the parity check requires this to leave GAPS.
#[test]
fn scrnaseq_includes_validate_companions_for_goal_atoms() {
    let task_ids = task_ids_for_scrnaseq();
    assert!(
        task_ids.contains("validate_cell_type_annotation"),
        "v4 scrnaseq dispatch must synthesize validate_cell_type_annotation; \
         got {task_ids:?}"
    );
    assert!(
        task_ids.contains("validate_differential_expression"),
        "v4 scrnaseq dispatch must synthesize validate_differential_expression; \
         got {task_ids:?}"
    );
}

/// The canonical scrnaseq pipeline includes the scaffolding atoms
/// (raw_qc, reporting, final_reporting, pathway_enrichment) declared
/// by `single_cell_de`. Asserting them here ensures the archetype
/// seed is being used as primary for definitive
/// modality+goal+format matches; without the seed bias the search
/// seed beats the archetype seed and these atoms go missing.
#[test]
fn scrnaseq_includes_canonical_scaffolding_atoms() {
    let task_ids = task_ids_for_scrnaseq();
    for required in [
        "raw_qc",
        "reporting",
        "final_reporting",
        "pathway_enrichment",
    ] {
        assert!(
            task_ids.contains(required),
            "v4 scrnaseq dispatch must include scaffolding atom {required} declared \
             by the single_cell_de archetype; got {task_ids:?}"
        );
    }
}

/// The archetype seed must NOT pull in modality-orthogonal atoms
/// (proteomics) when the intent specifies single_cell_rnaseq. The
/// search seed would otherwise bleed in `peptide_search` +
/// `protein_quantification` because their inputs unify with
/// `data_acquisition` outputs at depth 1; the archetype seed
/// declares only the modality-relevant atoms so a definitive
/// archetype match cleanly excludes the pollution.
#[test]
fn scrnaseq_excludes_proteomics_pollution() {
    let task_ids = task_ids_for_scrnaseq();
    for forbidden in ["peptide_search", "protein_quantification", "data_import"] {
        assert!(
            !task_ids.contains(forbidden),
            "v4 scrnaseq dispatch must not include modality-orthogonal atom \
             {forbidden} when intent specifies single_cell_rnaseq; got {task_ids:?}"
        );
    }
}
