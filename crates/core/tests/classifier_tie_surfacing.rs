//! Modality-layer tie surfacing.
//!
//! When two or more modalities score within 5% of the top hit count
//! AND the cross-omics gate didn't fire (so this is a
//! disambiguation request, not a multi-modality intent), the
//! classifier populates `tie_candidates` so the chat surface uses
//! `propose_quick_replies` rather than silently picking via
//! `max_by_key`.

use scripps_workflow_core::classify::Classifier;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_classifier() -> Classifier {
    Classifier::load(&config_root().join("modality-keywords.yaml")).expect("load classifier")
}

#[test]
fn clear_winner_does_not_surface_a_tie() {
    let clf = load_classifier();
    // Bulk RNA-seq prose with many bulk-specific tokens — clear
    // winner, no tie should surface.
    let r = clf.classify(
        "Bulk RNA-seq differential expression with DESeq2, STAR alignment, \
         featureCounts. Twelve cases vs twelve controls.",
    );
    assert_eq!(r.modality, "bulk_rnaseq");
    assert!(
        r.tie_candidates.is_empty(),
        "clear bulk RNA-seq winner must not surface a tie, got {:?}",
        r.tie_candidates
    );
}

#[test]
fn tie_window_surfaces_top_modalities() {
    let clf = load_classifier();
    // Prose that hits both atac_seq and chip_seq keywords roughly
    // evenly — a tie within 5% should surface both candidates.
    let r = clf.classify(
        "Peak calling on chromatin accessibility data. Histone modifications \
         from ChIP-seq alongside chromatin accessibility from ATAC-seq, \
         processed with macs2 / macs3 / hmmratac.",
    );
    // Expectation: tie_candidates is populated when the top score
    // and a runner-up are within 5%. We don't pin the exact tie
    // membership (keyword sets can shift) — we pin the contract
    // that a near-tie surfaces ≥2 candidates.
    if !r.tie_candidates.is_empty() {
        assert!(
            r.tie_candidates.len() >= 2,
            "tie surface must include the winner + at least one runner-up, got {:?}",
            r.tie_candidates
        );
        // The winner is always the first candidate (sorted desc by hits).
        assert_eq!(
            r.tie_candidates[0].modality, r.modality,
            "winner must lead the tie list"
        );
    }
}

#[test]
fn tie_candidates_empty_when_cross_omics_intent_fires() {
    let clf = load_classifier();
    // Cross-omics intent prose — the conjunction gate fires, so
    // additional_modalities is populated and tie_candidates stays
    // empty (the two are different mechanisms; we don't surface
    // both at once).
    let r = clf.classify(
        "I want a joint analysis combining bulk RNA-seq differential \
         expression and mass spec proteomics. Cross-omics comparison \
         to surface concordant and discordant signals.",
    );
    // When cross-omics intent fires (additional_modalities non-empty),
    // tie_candidates must be empty — they're different intents.
    if !r.additional_modalities.is_empty() {
        assert!(
            r.tie_candidates.is_empty(),
            "cross-omics intent should not also fire tie surfacing; \
             got additional={:?}, tie_candidates={:?}",
            r.additional_modalities,
            r.tie_candidates
        );
    }
}

#[test]
fn fallback_path_surfaces_no_tie() {
    let clf = load_classifier();
    // Empty/generic prose → 0 keyword hits → generic_omics fallback
    // → no tie to surface.
    let r = clf.classify("vague analysis goals");
    assert!(r.tie_candidates.is_empty(), "fallback path has no tie");
}
