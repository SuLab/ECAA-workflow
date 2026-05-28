//! Regression: bare "prespecified" used to flip bioinformatics requests
//! into project_class=clinical_trial, which then routed the v4 composer
//! to the clinical_trial_analysis archetype (incompatible inputs) and
//! threw GoalUnreachable on every paper-recreation prompt that
//! Mentioned a "prespecified" SAP/floor/endpoint. See
//! remediation plan task 1.
//!
//! API note: the plan called this entry point `classify_with_modality`
//! and `ProjectClass::Research`, but the actual public surface in
//! `crates/core/src/classify.rs` exposes `classify_project_class`
//! against `ProjectClassKeywordsConfig`, and the default bio class is
//! `ProjectClass::Bioinformatics` (no `Research` variant exists). This
//! mirrors the in-crate regression tests around L1160-L1244.

use scripps_workflow_core::{
    classify::{classify_project_class, load_project_class_keywords, ProjectClassKeywordsConfig},
    project_class::ProjectClass,
};

fn load_cfg() -> ProjectClassKeywordsConfig {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/project-class-keywords.yaml");
    load_project_class_keywords(&path).expect("should load project-class-keywords.yaml")
}

#[test]
fn prespecified_alone_does_not_route_to_clinical_trial() {
    let cfg = load_cfg();
    let text = "I want bulk RNA-seq differential expression with prespecified analysis.";
    let class = classify_project_class(text, &cfg);
    assert_eq!(
        class,
        ProjectClass::Bioinformatics,
        "bare 'prespecified' must not flip a bioinformatics request into clinical_trial — got {:?}",
        class
    );
}

#[test]
fn prespecified_in_multi_word_form_still_routes_to_clinical_trial() {
    // The narrowed keywords ("prespecified analysis plan", "prespecified SAP",
    // "prespecified primary endpoint") must still catch real clinical-trial
    // intake.
    let cfg = load_cfg();
    let text = "Phase III randomized controlled trial with prespecified statistical analysis plan, intent-to-treat population, primary efficacy endpoint.";
    let class = classify_project_class(text, &cfg);
    assert_eq!(class, ProjectClass::ClinicalTrial);
}
