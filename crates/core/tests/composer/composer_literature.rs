//! Literature contextualization is unconditional.
//!
//! `compose_with_intake` keeps the `review_prior_work` +
//! `contextualize_findings_with_literature` atoms regardless of
//! `intake.literature_review_included` — there is no longer an opt-in
//! gate that drops them on plain prose. These tests assert the atoms
//! survive composition on the default (opt-out) context as well as the
//! explicit opt-in context.
//!
//! Exercised against the v2 archetype-fast-path because that path
//! includes the archetype's optional atoms in the composition result.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer::{
    compose_with_intake, compose_with_modality, IntakeContext, LITERATURE_OPT_IN_ATOM_IDS,
};
use ecaa_workflow_core::goal_spec::GoalSpec;
use std::collections::BTreeMap;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

fn registries() -> (AtomRegistry, ArchetypeRegistry) {
    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archs = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))
        .expect("load archetype registry");
    (atoms, archs)
}

fn bulk_de_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    }
}

/// Smoke test: the v2 archetype path includes optional atoms from the
/// bulk_rnaseq_de archetype, so the literature atoms are present.
/// Uses a modality hint to break the DE-archetype tie.
#[test]
fn v2_archetype_path_includes_literature_atoms() {
    let (atoms, archs) = registries();
    if atoms.is_empty() || archs.is_empty() {
        return;
    }
    // Supply "bulk_rnaseq" modality hint so the v2 archetype-fast-path
    // picks bulk_rnaseq_de unambiguously (without the hint the DE goal
    // produces a TieRequiresSmeDecision across 8 archetypes).
    let result = compose_with_modality(
        &bulk_de_goal(),
        "bioinformatics",
        &atoms,
        &archs,
        Some("bulk_rnaseq"),
    )
    .expect("v2 compose with bulk_rnaseq hint should succeed for bulk DE goal");
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"review_prior_work"),
        "v2 path should include review_prior_work; got {ids:?}"
    );
    assert!(
        ids.contains(&"contextualize_findings_with_literature"),
        "v2 path should include contextualize_findings_with_literature; got {ids:?}"
    );
}

/// The constant lists both literature atoms — it remains the single
/// source of truth for which atoms are the literature family (used by
/// the literature-context tools), even though the opt-in gate is gone.
#[test]
fn literature_opt_in_atom_ids_constant_covers_both_atoms() {
    assert!(
        LITERATURE_OPT_IN_ATOM_IDS.contains(&"review_prior_work"),
        "LITERATURE_OPT_IN_ATOM_IDS must list review_prior_work"
    );
    assert!(
        LITERATURE_OPT_IN_ATOM_IDS.contains(&"contextualize_findings_with_literature"),
        "LITERATURE_OPT_IN_ATOM_IDS must list contextualize_findings_with_literature"
    );
}

/// `IntakeContext::empty()` defaults `literature_review_included` to
/// false — but the literature atoms survive composition regardless,
/// because contextualization is unconditional.
#[test]
fn compose_with_intake_keeps_literature_on_default_context() {
    let (atoms, archs) = registries();
    if atoms.is_empty() || archs.is_empty() {
        return;
    }
    let intake = IntakeContext {
        literature_review_included: false,
        ..IntakeContext::empty()
    };
    let result = compose_with_intake(&bulk_de_goal(), "bioinformatics", &atoms, &archs, &intake)
        .expect("compose_with_intake should succeed");
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"review_prior_work"),
        "review_prior_work must survive on default context (unconditional literature); got {ids:?}"
    );
    assert!(
        ids.contains(&"contextualize_findings_with_literature"),
        "contextualize_findings_with_literature must survive on default context; got {ids:?}"
    );
}

/// Explicit opt-in (`literature_review_included = true`) also keeps the
/// atoms — the flag is now inert with respect to the literature family.
#[test]
fn compose_with_intake_keeps_literature_when_requested() {
    let (atoms, archs) = registries();
    if atoms.is_empty() || archs.is_empty() {
        return;
    }
    let intake = IntakeContext {
        literature_review_included: true,
        ..IntakeContext::empty()
    };
    let result = compose_with_intake(&bulk_de_goal(), "bioinformatics", &atoms, &archs, &intake)
        .expect("compose_with_intake should succeed");
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"review_prior_work")
            && ids.contains(&"contextualize_findings_with_literature"),
        "literature atoms must be present when requested; got {ids:?}"
    );
}

/// The v2 archetype-fast-path for chip_seq_peaks includes the two
/// literature atoms (declared in the archetype as optional).
#[test]
fn v2_chip_seq_peaks_includes_literature_atoms() {
    let (atoms, archs) = registries();
    if atoms.is_empty() || archs.is_empty() {
        return;
    }
    // chip_seq_peaks goal: data:1255 (Feature record) / format:3003 (BED).
    let goal = GoalSpec {
        edam_data: "data:1255".into(),
        edam_format: Some("format:3003".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    let result = compose_with_modality(&goal, "bioinformatics", &atoms, &archs, Some("chip_seq"))
        .expect("v2 compose with chip_seq hint should succeed for peak calling goal");
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"review_prior_work"),
        "chip_seq_peaks v2 path should include review_prior_work; got {ids:?}"
    );
    assert!(
        ids.contains(&"contextualize_findings_with_literature"),
        "chip_seq_peaks v2 path should include contextualize_findings_with_literature; got {ids:?}"
    );
}

/// The v2 archetype-fast-path for variant_calling_germline includes the
/// two literature atoms.
#[test]
fn v2_variant_calling_includes_literature_atoms() {
    let (atoms, archs) = registries();
    if atoms.is_empty() || archs.is_empty() {
        return;
    }
    // variant_calling_germline goal: data:3498 (Sequence variations) / format:3016 (VCF).
    let goal = GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: BTreeMap::new(),
        source_prose: None,
        confidence: 0.9,
    };
    let result = compose_with_modality(
        &goal,
        "bioinformatics",
        &atoms,
        &archs,
        Some("variant_calling"),
    )
    .expect("v2 compose with variant_calling hint should succeed for variant goal");
    let ids: Vec<&str> = result.atoms.iter().map(|c| c.atom.id.as_str()).collect();
    assert!(
        ids.contains(&"review_prior_work"),
        "variant_calling_germline v2 path should include review_prior_work; got {ids:?}"
    );
    assert!(
        ids.contains(&"contextualize_findings_with_literature"),
        "variant_calling_germline v2 path should include contextualize_findings_with_literature; got {ids:?}"
    );
}
