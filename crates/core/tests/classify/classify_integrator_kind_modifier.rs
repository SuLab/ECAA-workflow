//! `Classifier::extract_goal` must set `goal.modifiers["kind"]` based
//! on integrator + protocol names in the source prose so the composer's
//! cross-omics discriminator at `composer/dispatch.rs:412` picks the
//! correct archetype variant (DIABLO vs MOFA vs SNF vs generic).

use ecaa_workflow_core::classify::Classifier;
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
fn diablo_keyword_sets_supervised_cross_omics_kind() {
    let goal = classifier()
        .extract_goal(
            "supervised cross-omics integrator (sparse PLS-DA / DIABLO) \
             jointly discriminating PAM50 subtypes from paired RNA + proteomics",
        )
        .expect("goal extracted");
    assert_eq!(
        goal.modifiers.get("kind").map(String::as_str),
        Some("supervised_cross_omics")
    );
    assert_eq!(
        goal.modifiers.get("integrator").map(String::as_str),
        Some("diablo")
    );
}

#[test]
fn mofa_keyword_sets_unsupervised_latent_factor_kind() {
    let goal = classifier()
        .extract_goal(
            "unsupervised latent-factor decomposition (MOFA) across paired \
             bulk RNA-seq and mass-spec proteomics",
        )
        .expect("goal extracted");
    assert_eq!(
        goal.modifiers.get("kind").map(String::as_str),
        Some("unsupervised_latent_factor")
    );
    assert_eq!(
        goal.modifiers.get("integrator").map(String::as_str),
        Some("mofa")
    );
}

#[test]
fn snf_keyword_sets_network_fusion_kind() {
    let goal = classifier()
        .extract_goal(
            "similarity network fusion (SNF) to discover cancer subtypes \
             across paired bulk RNA-seq and proteomics",
        )
        .expect("goal extracted");
    assert_eq!(
        goal.modifiers.get("kind").map(String::as_str),
        Some("network_fusion")
    );
    assert_eq!(
        goal.modifiers.get("integrator").map(String::as_str),
        Some("snf")
    );
}

#[test]
fn multiome_arc_keyword_sets_arc_demultiplex_kind() {
    let goal = classifier()
        .extract_goal(
            "10x Genomics Multiome ARC data on human PBMCs with WNN-style \
             joint embedding",
        )
        .expect("goal extracted");
    assert_eq!(
        goal.modifiers.get("kind").map(String::as_str),
        Some("arc_demultiplex")
    );
}

#[test]
fn share_seq_keyword_sets_share_seq_barcode_kind() {
    let goal = classifier()
        .extract_goal(
            "SHARE-seq combinatorial split-pool barcoding on mouse skin \
             tissue, joint embedding of paired RNA + ATAC",
        )
        .expect("goal extracted");
    assert_eq!(
        goal.modifiers.get("kind").map(String::as_str),
        Some("share_seq_barcode")
    );
}

#[test]
fn no_integrator_keyword_leaves_kind_unset() {
    // Bare bulk RNA-seq DE prose — must not falsely tag an integrator.
    let g = classifier().extract_goal("bulk RNA-seq differential expression treated vs control");
    // Either no goal extracted, or kind != supervised_cross_omics
    if let Some(g) = g {
        assert_ne!(
            g.modifiers.get("kind").map(String::as_str),
            Some("supervised_cross_omics")
        );
        assert!(g.modifiers.get("integrator").is_none());
    }
}
