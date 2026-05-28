//! Low-confidence atypical-shape prompts must fall through to
//! `generic_omics` archetype which emits the universal
//! `raw_qc → generic_summary` terminals. Without this, prompts like
//! survival-only on bulk RNA, scATAC-only, microbiome strain-SNP
//! bind to a modality archetype whose terminals (`reporting`,
//! `final_reporting`) don't satisfy the corpus's universal-terminal
//! requirement.

use anyhow::Result;
use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::classify::Classifier;
use scripps_workflow_core::composer::compose_with_version_and_modality;
use std::path::Path;

fn registries_and_classifier() -> Result<(AtomRegistry, ArchetypeRegistry, Classifier)> {
    let config = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config");
    let atom_reg = AtomRegistry::load_from_dir(&config.join("stage-atoms"))?;
    let archetype_reg = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))?;
    let classifier = Classifier::load(&config.join("modality-keywords.yaml"))?;
    Ok((atom_reg, archetype_reg, classifier))
}

#[test]
fn survival_genomics_emits_generic_summary() -> Result<()> {
    let (atom_reg, archetype_reg, classifier) = registries_and_classifier()?;
    let prompt = "bulk RNA-seq gene-expression matrix for ~200 tumor samples \
                  plus per-patient overall-survival times and event indicators. \
                  Kaplan-Meier curves stratified by expression quantiles, log-rank \
                  tests, and a multivariable Cox proportional-hazards model.";
    let cls = classifier.classify(prompt);
    let goal = classifier
        .extract_goal(prompt)
        .unwrap_or_else(|| scripps_workflow_core::goal_spec::GoalSpec::default());
    let result = compose_with_version_and_modality(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4,
        Some(cls.modality.as_str()),
    )?;
    let atom_ids: std::collections::BTreeSet<&str> =
        result.atoms.iter().map(|a| a.atom.id.as_str()).collect();
    assert!(
        atom_ids.contains("raw_qc") && atom_ids.contains("generic_summary"),
        "survival-genomics prompt must emit raw_qc + generic_summary universal terminals; got {:?}",
        atom_ids
    );
    Ok(())
}

#[test]
fn microbiome_strain_snp_emits_generic_summary() -> Result<()> {
    let (atom_reg, archetype_reg, classifier) = registries_and_classifier()?;
    let prompt = "Metagenomic shotgun data from a healthy gut microbiome cohort. \
                  We want strain-level resolution — strain SNP calling, per-sample \
                  strain abundance estimates, summary tables.";
    let cls = classifier.classify(prompt);
    let goal = classifier.extract_goal(prompt).unwrap_or_default();
    let result = compose_with_version_and_modality(
        &goal,
        "bioinformatics",
        &atom_reg,
        &archetype_reg,
        4,
        Some(cls.modality.as_str()),
    )?;
    let atom_ids: std::collections::BTreeSet<&str> =
        result.atoms.iter().map(|a| a.atom.id.as_str()).collect();
    assert!(
        atom_ids.contains("generic_summary"),
        "microbiome strain-SNP prompt must emit generic_summary terminal; got {:?}",
        atom_ids
    );
    Ok(())
}
