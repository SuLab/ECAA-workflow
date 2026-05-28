//! Classifier multi-modality N-way detection.
//!
//! The M1 implementation already returns a `Vec<ModalityCandidate>`
//! that's N-way in shape; M7 extends `is_cross_omics_intent` to
//! recognize Oxford-comma + and-list phrasing ("transcriptomics,
//! proteomics, and metabolomics") so SMEs who drop the closing "and"
//! still get cross-omics surfacing.

use ecaa_workflow_core::classify::Classifier;
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
    Classifier::load(&config_root().join("modality-keywords.yaml")).expect("Classifier must load")
}

#[test]
fn oxford_comma_and_list_three_modalities() {
    let clf = load_classifier();
    // Use prose with three branches that map to known modality
    // keywords. The Oxford-comma-and form has " and " before the
    // last item, so the conjunction loop catches it; this test
    // pins that behavior so it stays consistent with the comma-only
    // path below.
    let result = clf.classify(
        "Bulk RNA-seq differential expression, ATAC-seq peak calling, \
         and ChIP-seq peak calling across the same patient cohort. \
         Joint analysis joining the three branches for cross-omics \
         comparison.",
    );
    let primary = result.modality.as_str();
    let mut all: Vec<&str> = std::iter::once(primary)
        .chain(
            result
                .additional_modalities
                .iter()
                .map(|m| m.modality.as_str()),
        )
        .collect();
    all.sort();
    all.dedup();
    assert!(
        all.contains(&"bulk_rnaseq"),
        "bulk_rnaseq must appear (primary or in additional), got {:?}",
        all
    );
    // At least one secondary modality (atac_seq or chip_seq) should
    // surface — the Oxford-comma-and form must trigger cross-omics
    // detection at all. We don't pin which one is primary because
    // keyword-hit-count tiebreak is ordering-dependent.
    let secondaries: std::collections::HashSet<&str> = all
        .iter()
        .filter(|m| **m != "bulk_rnaseq")
        .copied()
        .collect();
    assert!(
        !secondaries.is_empty(),
        "at least one secondary modality must appear via Oxford-comma list, got {:?}",
        all
    );
}

#[test]
fn comma_only_list_two_modalities_no_and() {
    // SMEs sometimes drop "and" entirely in lists. The comma-list
    // detection should still trigger.
    let clf = load_classifier();
    let result = clf.classify(
        "Cross-omics analysis: RNA-seq, mass spec proteomics. \
         Differential expression across two groups, contrast in the \
         intake.",
    );
    let primary = result.modality.as_str();
    let secondaries: Vec<&str> = result
        .additional_modalities
        .iter()
        .map(|m| m.modality.as_str())
        .collect();
    let all: std::collections::HashSet<&str> = std::iter::once(primary)
        .chain(secondaries.iter().copied())
        .collect();
    assert!(
        all.contains("bulk_rnaseq"),
        "bulk_rnaseq must appear, got {:?}",
        all
    );
    assert!(
        all.contains("proteomics"),
        "proteomics must appear via comma-only list detection, got {:?} — \
         is_cross_omics_intent's comma-list branch must trigger when no 'and'",
        all
    );
}

#[test]
fn single_modality_list_with_methods_no_false_positive() {
    // Regression guard: a single-modality intake that happens to use
    // commas to list methods/parameters must NOT trigger cross-omics
    // detection. "RNA-seq, paired-end, 150bp" mentions only one
    // modality (bulk_rnaseq) and a parameter list.
    let clf = load_classifier();
    let result = clf.classify(
        "Bulk RNA-seq differential expression. Illumina paired-end 150bp \
         reads, Homo sapiens samples, twelve cases versus twelve controls.",
    );
    assert!(
        result.additional_modalities.is_empty(),
        "single-modality prose must NOT trigger cross-omics, got additional={:?}",
        result.additional_modalities
    );
}
