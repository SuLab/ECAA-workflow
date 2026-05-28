//! `Classifier::extract_goal` must never emit the `data:9999`
//! placeholder. When no EDAM data class matches the SME's prose,
//! return `None` (composer falls back to archetype default) or
//! return a goal with empty `edam_data` + a `kind` modifier so the
//! composer's cross-omics discriminator can still route correctly.

use scripps_workflow_core::classify::Classifier;
use std::path::Path;

fn classifier() -> Classifier {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
        .join("modality-keywords.yaml");
    Classifier::load(&path).expect("load classifier")
}

#[test]
fn diablo_prompt_never_emits_data_9999() {
    let prompt = "Paired bulk RNA-seq and mass-spec proteomics on a \
                  breast-cancer tumor cohort. Supervised cross-omics \
                  integrator (sparse PLS-DA / DIABLO). Surface a sparse \
                  set of mRNA and protein features that jointly \
                  discriminate the four PAM50 subtypes.";
    if let Some(goal) = classifier().extract_goal(prompt) {
        assert_ne!(
            goal.edam_data, "data:9999",
            "extract_goal must never emit `data:9999` placeholder; \
             got modifiers={:?}",
            goal.modifiers
        );
    }
}

#[test]
fn no_match_returns_none_or_wildcard() {
    // Genuinely unstructured prose with no goal-anchored phrase.
    let prompt = "Hi I am a scientist and I have some data, can you \
                  help me figure out what to do with it?";
    let g = classifier().extract_goal(prompt);
    if let Some(g) = g {
        assert_ne!(g.edam_data, "data:9999");
    }
}
