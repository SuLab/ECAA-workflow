//! Deterministic confidence estimator for the heuristic mock backend.
//!
//! Reads SME prose against a small fixed keyword table and emits a
//! `(confidence, ambiguous_modalities, dominant_modality)` triple that
//! drives `HeuristicMockBackend`'s decision table:
//!
//! - `confidence < 0.3` → dispatch `propose_quick_replies`.
//! - `0.3 ≤ confidence < 0.7` → dispatch `classify_intake` then
//!   `get_classification_evidence` before mutating intake.
//! - `confidence ≥ 0.7` → current happy path.
//!
//! Scoring bands:
//!
//! | matches per modality                       | confidence | dominant |
//! |--------------------------------------------|------------|----------|
//! | 0 across all modalities (only meta phrases) | 0.1        | None     |
//! | 0 across all modalities (no signal at all)  | 0.0        | None     |
//! | 1 match for exactly 1 modality              | 0.9        | Some(_)  |
//! | 2+ matches for the SAME modality            | 0.95       | Some(_)  |
//! | matches for 2+ DIFFERENT modalities         | 0.5        | leader   |
//!
//! The keyword table is intentionally tiny and hand-coded; the full
//! production classifier reads `config/modality-keywords.yaml` +
//! `config/modalities/<id>.yaml`. This module is a *test oracle*, not a
//! replacement for `core::classify`.

/// Result of running `estimate_confidence` over SME prose. `confidence`
/// lies in `0.0..=1.0`; `ambiguous_modalities` is populated only when
/// the prose hits two or more distinct modalities; `dominant_modality`
/// is the modality with the highest match count when there is a clear
/// leader (ties under the ambiguous band still set this to the first
/// modality reached so the caller can render a deterministic label).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfidenceEstimate {
    /// Confidence score in `[0.0, 1.0]`.
    pub confidence: f32,
    /// Modalities with similar match counts; non-empty when confidence is
    /// below the ambiguous-band threshold.
    pub ambiguous_modalities: Vec<String>,
    /// The modality with the highest keyword match count, if one exists.
    pub dominant_modality: Option<String>,
}

/// Hand-coded modality keyword table. Each row is `(modality_id,
/// keywords)`; matches are case-insensitive whole-substring with word-
/// boundary trimming on punctuation so phrases like `"RNA-seq"` and
/// `"single-cell RNA seq"` both hit `rna-seq`.
///
/// The list deliberately stays small — the goal is to drive the three
/// confidence bands deterministically from short fixture prose, not to
/// replicate the full classifier corpus. Adding a modality here means
/// adding a row to one of the fixture scenarios, not migrating the
/// production classifier.
const MODALITY_TABLE: &[(&str, &[&str])] = &[
    (
        "scrna-seq",
        &[
            "scrna-seq",
            "scrnaseq",
            "single-cell rna",
            "single cell rna",
        ],
    ),
    (
        "snrna-seq",
        &[
            "snrna-seq",
            "snrnaseq",
            "single-nucleus rna",
            "single nucleus rna",
        ],
    ),
    (
        "rna-seq",
        &[
            "rna-seq",
            "rnaseq",
            "rna seq",
            "bulk rna",
            "transcriptomics",
        ],
    ),
    (
        "atac-seq",
        &["atac-seq", "atacseq", "atac seq", "chromatin accessibility"],
    ),
    (
        "chip-seq",
        &["chip-seq", "chipseq", "chip seq", "tf binding"],
    ),
    (
        "methyl-seq",
        &[
            "methyl-seq",
            "methylseq",
            "methylation",
            "bisulfite",
            "wgbs",
            "rrbs",
        ],
    ),
    (
        "proteomics",
        &[
            "proteomics",
            "proteome",
            "mass spectrometry",
            "lc-ms",
            "shotgun proteomics",
        ],
    ),
    (
        "metabolomics",
        &["metabolomics", "metabolome", "metabolite profiling"],
    ),
    (
        "wgs",
        &["wgs", "whole-genome sequencing", "whole genome sequencing"],
    ),
    (
        "exome",
        &[
            "exome",
            "wes",
            "whole-exome sequencing",
            "whole exome sequencing",
        ],
    ),
];

/// Phrases that signal "I have no idea what to do" — the SME hasn't
/// given the system enough signal to even pick a modality. Hitting any
/// of these floors confidence at 0.1 (slightly above absolute zero so
/// the decision table can distinguish "user typed nothing" from "user
/// typed something but it carries no modality signal").
const META_PHRASES: &[&str] = &[
    "some data",
    "analyze this",
    "what kind of analysis",
    "help me figure out",
    "not sure what",
];

/// Walk `prose` against the modality table + meta-phrase list and
/// return a `ConfidenceEstimate` per the band table in the module docs.
/// Pure; no I/O; case-insensitive.
pub fn estimate_confidence(prose: &str) -> ConfidenceEstimate {
    let lowered = prose.to_lowercase();
    // Working copy that gets scrubbed as each keyword matches, so the
    // long-form modality rows can claim a shared substring before the
    // short-form row scans for it. `lowered` stays pristine for the
    // meta-phrase check below.
    let mut working = lowered.clone();

    // Count matches per modality. Each modality contributes at most
    // one increment per distinct keyword that appears in the prose,
    // so "rna-seq rna-seq rna-seq" still counts as 1 for that modality
    // but "rna-seq bulk rna transcriptomics" counts as 3. We walk the
    // table in declaration order and *consume* (scrub to spaces) each
    // matched keyword from the working text before moving on, so the
    // long-form rows (`scrna-seq`, `snrna-seq`) win against the
    // overlapping short-form rows (`rna-seq`'s `"rna-seq"` keyword).
    // Without this, "scRNA-seq" would double-match scrna-seq AND
    // rna-seq and the ambiguous-band assertion would fire on prose
    // that is unambiguously single-cell.
    let mut per_modality: Vec<(String, usize)> = Vec::new();
    for (modality, keywords) in MODALITY_TABLE.iter() {
        let mut hits = 0usize;
        for kw in keywords.iter() {
            if working.contains(kw) {
                hits += 1;
                // Scrub matched keyword so subsequent rows don't
                // also match it. Replace with spaces (not empty) to
                // preserve word boundaries on either side.
                let placeholder = " ".repeat(kw.len());
                working = working.replace(kw, &placeholder);
            }
        }
        if hits > 0 {
            per_modality.push(((*modality).to_string(), hits));
        }
    }

    // Sort by hit count descending so the leader sits at index 0;
    // ties break by name for determinism.
    per_modality.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    if per_modality.is_empty() {
        // Distinguish "no signal at all" from "explicit meta plea".
        let confidence = if META_PHRASES.iter().any(|p| lowered.contains(p)) {
            0.1
        } else {
            0.0
        };
        return ConfidenceEstimate {
            confidence,
            ambiguous_modalities: vec![],
            dominant_modality: None,
        };
    }

    if per_modality.len() == 1 {
        let (name, hits) = &per_modality[0];
        let confidence = if *hits >= 2 { 0.95 } else { 0.9 };
        return ConfidenceEstimate {
            confidence,
            ambiguous_modalities: vec![],
            dominant_modality: Some(name.clone()),
        };
    }

    // 2+ distinct modalities matched → ambiguous band.
    let ambiguous_modalities: Vec<String> =
        per_modality.iter().map(|(name, _)| name.clone()).collect();
    let dominant = ambiguous_modalities.first().cloned();
    ConfidenceEstimate {
        confidence: 0.5,
        ambiguous_modalities,
        dominant_modality: dominant,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_prose_returns_zero_confidence() {
        let est = estimate_confidence("");
        assert_eq!(est.confidence, 0.0);
        assert!(est.ambiguous_modalities.is_empty());
        assert!(est.dominant_modality.is_none());
    }

    #[test]
    fn no_signal_prose_returns_zero_confidence() {
        let est = estimate_confidence("here is a long sentence about my project goals");
        assert_eq!(est.confidence, 0.0);
        assert!(est.dominant_modality.is_none());
    }

    #[test]
    fn meta_phrase_floors_to_0_1() {
        let est = estimate_confidence("we have some data, please analyze");
        assert!((est.confidence - 0.1).abs() < f32::EPSILON);
        assert!(est.dominant_modality.is_none());
        assert!(est.ambiguous_modalities.is_empty());
    }

    #[test]
    fn single_keyword_single_modality_yields_0_9() {
        let est = estimate_confidence("we have bulk RNA-seq from human liver");
        assert!((est.confidence - 0.9).abs() < f32::EPSILON);
        assert_eq!(est.dominant_modality.as_deref(), Some("rna-seq"));
        assert!(est.ambiguous_modalities.is_empty());
    }

    #[test]
    fn multiple_keywords_same_modality_yields_0_95() {
        // "rna-seq" + "bulk rna" + "transcriptomics" all map to rna-seq.
        let est =
            estimate_confidence("bulk RNA-seq transcriptomics experiment, bulk rna libraries");
        assert!((est.confidence - 0.95).abs() < f32::EPSILON);
        assert_eq!(est.dominant_modality.as_deref(), Some("rna-seq"));
        assert!(est.ambiguous_modalities.is_empty());
    }

    #[test]
    fn two_modalities_yields_ambiguous_0_5() {
        let est =
            estimate_confidence("joint analysis of RNA-seq and ATAC-seq data from human PBMCs");
        assert!((est.confidence - 0.5).abs() < f32::EPSILON);
        assert!(est.ambiguous_modalities.len() >= 2);
        assert!(est.ambiguous_modalities.iter().any(|m| m == "rna-seq"));
        assert!(est.ambiguous_modalities.iter().any(|m| m == "atac-seq"));
        assert!(est.dominant_modality.is_some());
    }

    #[test]
    fn proteomics_keyword_dominates_when_alone() {
        let est = estimate_confidence("DIA proteomics mass spectrometry from plasma samples");
        // "proteomics" + "mass spectrometry" both hit the proteomics row.
        assert!((est.confidence - 0.95).abs() < f32::EPSILON);
        assert_eq!(est.dominant_modality.as_deref(), Some("proteomics"));
    }

    #[test]
    fn case_insensitive_matching() {
        let est_lower = estimate_confidence("scrna-seq libraries");
        let est_upper = estimate_confidence("SCRNA-SEQ LIBRARIES");
        assert_eq!(est_lower.confidence, est_upper.confidence);
        assert_eq!(est_lower.dominant_modality, est_upper.dominant_modality);
    }

    #[test]
    fn three_modalities_all_listed_in_ambiguous() {
        let est = estimate_confidence(
            "we have RNA-seq plus ATAC-seq plus methylation data from matched samples",
        );
        assert!((est.confidence - 0.5).abs() < f32::EPSILON);
        assert!(est.ambiguous_modalities.iter().any(|m| m == "rna-seq"));
        assert!(est.ambiguous_modalities.iter().any(|m| m == "atac-seq"));
        assert!(est.ambiguous_modalities.iter().any(|m| m == "methyl-seq"));
    }
}
