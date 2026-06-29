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

// ── G4: scope cross-omics by the QUESTION, not the data-inventory listing ──
//
// A single-modality analytic question over a multi-omic *dataset* must NOT
// fan the DAG out to every modality just because the data-inventory section
// lists them. Mirrors the BiomniBench da-19 scenarios where the over-
// composition came from re-classifying the full intake message (Task +
// "Available data: … profiled by RNA-seq, ChIP-seq, and ATAC-seq").

/// Shared multi-omic inventory block (the part that names three modalities).
const DA19_INVENTORY: &str = "\n\nAvailable data:\nMulti-omics cohort from the \
    CBFB-SMMHC inhibition study. Human inv(16) leukemia cells (ME-1) treated with \
    the inhibitor AI-10-49 or DMSO control, profiled by RNA-seq, ChIP-seq (H3K27ac \
    and RUNX1), and ATAC-seq. ChIP-seq H3K27ac BAMs (GSM2715535 DMSO, GSM2715536 \
    AI-10-49) with MACS2 narrowPeak peaks called against matched input controls; \
    RUNX1 ChIP-seq BAMs and peaks likewise. Sample-to-condition mapping used \
    across all modalities.\n\nProduce the analysis with appropriate per-step \
    result tables and a summary report. Organism and modality: infer from the \
    task and data above.";

#[test]
fn da19_chip_question_over_multiomic_inventory_stays_single_modality() {
    // Pure ChIP-seq question (reduced H3K27ac signal) over the multi-omic
    // dataset. The inventory's RNA-seq/ATAC-seq must NOT surface as companions.
    let clf = load_classifier();
    let prose = format!(
        "Task: To identify enhancers that drive leukemia maintenance, which \
         genomic regions show reduced H3K27ac ChIP-seq signal upon AI-10-49 \
         treatment?{DA19_INVENTORY}"
    );
    let result = clf.classify(&prose);
    assert_eq!(
        result.modality, "chip_seq",
        "single-modality H3K27ac question should classify as chip_seq, got {}",
        result.modality
    );
    assert!(
        result.additional_modalities.is_empty(),
        "ChIP question over a multi-omic dataset must NOT surface companion \
         modalities from the data inventory, got {:?}",
        result
            .additional_modalities
            .iter()
            .map(|m| m.modality.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn da19_rnaseq_question_over_multiomic_inventory_stays_single_modality() {
    // Pure RNA-seq DE question over the same multi-omic dataset.
    let clf = load_classifier();
    let prose = format!(
        "Task: To identify therapeutic targets, which genes are most \
         significantly downregulated upon AI-10-49 treatment in the RNA-seq \
         differential expression results?{DA19_INVENTORY}"
    );
    let result = clf.classify(&prose);
    assert!(
        result.additional_modalities.is_empty(),
        "RNA-seq DE question over a multi-omic dataset must NOT surface companion \
         modalities from the data inventory, got primary={} additional={:?}",
        result.modality,
        result
            .additional_modalities
            .iter()
            .map(|m| m.modality.as_str())
            .collect::<Vec<_>>()
    );
}

#[test]
fn integrative_question_over_multiomic_inventory_keeps_companions() {
    // CONTROL: when the QUESTION itself is integrative (not just the dataset),
    // cross-omics companions MUST still surface — the fix keys on the ask.
    let clf = load_classifier();
    let prose = format!(
        "Task: Integrate ChIP-seq H3K27ac enhancer changes with the RNA-seq \
         differential expression results to link enhancer loss to downregulated \
         genes across both assays in a joint cross-omics analysis.{DA19_INVENTORY}"
    );
    let result = clf.classify(&prose);
    let all: std::collections::HashSet<&str> = std::iter::once(result.modality.as_str())
        .chain(result.additional_modalities.iter().map(|m| m.modality.as_str()))
        .collect();
    assert!(
        result.additional_modalities.len() >= 1 && all.len() >= 2,
        "an explicitly integrative question must still surface cross-omics \
         companions, got {:?}",
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
