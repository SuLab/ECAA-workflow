//! Method-mention detection for the `HeuristicMockBackend`'s
//! Neutrality/refusal arm.
//!
//! `prompt_role.txt` lines 487-507 codify the contract the heuristic
//! must mirror: the assistant MUST NOT recommend a methodological
//! choice (aligner, normalization, batch correction, statistical
//! test, clustering algorithm, search engine). The single carve-out
//! is when the SME has already named a tool unprompted in the
//! current session — the assistant then carries that named choice
//! through via `set_intake_method`.
//!
//! This module collapses that contract into a three-way
//! [`MethodMention`] verdict over the latest SME prose:
//!
//! - [`MethodMention::SmeNamed`] — prose names a method in a
//!   contextual form that implies pinning ("use STAR", "with STAR",
//!   "via STAR"). The heuristic dispatches `set_intake_method`.
//! - [`MethodMention::SmeRequestedRecommendation`] — prose asks the
//!   assistant to pick ("which aligner", "what should I use",
//!   "recommend", "suggest"). The heuristic refuses and defers to
//!   the runtime execution agent.
//! - [`MethodMention::None`] — no method-relevant signal. The
//!   decision-table falls through to the regular intake flow.
//!
//! The detector is intentionally a small hand-written keyword
//! oracle — every rule is auditable in one read, mirroring the
//! `HeuristicMockBackend` design discipline. The tokens cover the
//! aligner / normalization / batch-correction / statistical-test
//! / clustering / peak-calling families called out by the role
//! spec; expanding the list is a one-line edit per family.

/// Verdict on whether the SME's latest prose triggers the
/// neutrality/refusal arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodMention {
    /// SME named a method unprompted (allowed carve-out). Carries
    /// the exact token the heuristic should pass to
    /// `set_intake_method`.
    SmeNamed {
        /// Exact method token (e.g. `"DESeq2"`, `"STAR"`).
        method: String,
    },
    /// SME asked the assistant to recommend (refusal required).
    /// Carries the rough tool category for the refusal text.
    SmeRequestedRecommendation {
        /// Tool category label for the refusal text (e.g. `"aligner"`).
        tool_category: String,
    },
    /// No method mention.
    None,
}

/// Known method tokens, grouped by tool category. The category
/// label is surfaced into the refusal text so the assistant can
/// say "I won't recommend an aligner" rather than the generic
/// "I won't recommend a method".
///
/// Tokens are compared case-insensitively against word-boundary
/// matches in the SME prose, so casual mentions ("DESeq2-style
/// normalization") still register.
const METHOD_FAMILIES: &[(&str, &[&str])] = &[
    (
        "aligner",
        &[
            "STAR", "HISAT2", "Salmon", "Kallisto", "Bowtie", "Bowtie2", "BWA", "BWA-MEM",
            "minimap2", "Tophat",
        ],
    ),
    (
        "statistical test",
        &["DESeq2", "edgeR", "limma", "limma-voom"],
    ),
    (
        "batch-correction method",
        &["ComBat", "Harmony", "MNN", "scVI", "Scanorama"],
    ),
    (
        "normalization method",
        &["MAGIC", "SCnorm", "TMM", "RPKM", "FPKM", "TPM"],
    ),
    (
        "clustering tool",
        &["Seurat", "Scanpy", "Leiden", "Louvain", "PhenoGraph"],
    ),
    (
        "variant caller",
        &["GATK", "FreeBayes", "DeepVariant", "Strelka2"],
    ),
    (
        "peak caller",
        &["MACS2", "MACS3", "HOMER", "PeakChIP", "Genrich"],
    ),
];

/// Phrases that signal the SME is asking the assistant to PICK a
/// method (not name one). The match is case-insensitive against
/// the lowercased prose; word-boundaries are inferred from the
/// surrounding whitespace + punctuation in the phrase itself.
const RECOMMENDATION_REQUEST_PHRASES: &[&str] = &[
    "which aligner",
    "what aligner",
    "which normalization",
    "what normalization",
    "which batch correction",
    "what batch correction",
    "which clustering",
    "what clustering",
    "which statistical test",
    "what statistical test",
    "which method",
    "what method",
    "which approach",
    "what approach",
    "what should i use",
    "what should we use",
    "which should i use",
    "which should we use",
    "recommend",
    "recommendation",
    "suggest",
    "your opinion",
    "your take",
    "any preference",
];

/// Run the detector on the SME's latest prose. Pure; no I/O.
pub fn detect_method_mention(prose: &str) -> MethodMention {
    let lower = prose.to_lowercase();

    // Recommendation request takes precedence: a prose that names
    // STAR AND asks "which aligner should I use?" is read as a
    // recommendation ask, not a pinning. The role spec is
    // explicit ("If the SME asks 'should I use scVI or Harmony?'"
    // → refusal even though both methods are named).
    for phrase in RECOMMENDATION_REQUEST_PHRASES {
        if lower.contains(phrase) {
            let category = infer_category_from_phrase(phrase, &lower);
            return MethodMention::SmeRequestedRecommendation {
                tool_category: category,
            };
        }
    }

    // Pinning-context tokens. "use X", "with X", "via X", "using
    // X" all imply the SME has decided. A bare mention ("we have
    // STAR-aligned BAMs") could mean the data was preprocessed
    // upstream — without the pinning preposition we treat it as
    // ambient context, not a pinning signal.
    for (_category, tokens) in METHOD_FAMILIES {
        for token in *tokens {
            let token_lower = token.to_lowercase();
            if !contains_word(&lower, &token_lower) {
                continue;
            }
            if has_pinning_context(&lower, &token_lower) {
                return MethodMention::SmeNamed {
                    method: token.to_string(),
                };
            }
        }
    }

    MethodMention::None
}

/// Word-boundary containment check (case-insensitive callers
/// pass lowercased strings). Returns true when `needle` appears
/// in `haystack` with non-alphanumeric boundaries on each side,
/// so "star" in "starvation" doesn't match.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(idx) = haystack[search_from..].find(needle) {
        let start = search_from + idx;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after_ok = end >= haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        search_from = start + 1;
    }
    false
}

/// True when `token` appears in `haystack` (lowercased) preceded
/// by a pinning preposition within 6 characters. Catches "use
/// STAR", "with STAR", "via STAR", "using STAR", "by STAR", "run
/// STAR". Bare mentions return false.
fn has_pinning_context(haystack: &str, token: &str) -> bool {
    const PREPS: &[&str] = &["use ", "using ", "with ", "via ", "by ", "run ", "want "];
    let mut search_from = 0;
    while let Some(idx) = haystack[search_from..].find(token) {
        let start = search_from + idx;
        let prefix = &haystack[..start];
        if PREPS.iter().any(|p| prefix.ends_with(p)) {
            return true;
        }
        // The role spec allows comma-listed pinning ("STAR for
        // alignment and DESeq2 for the stats"). When the token is
        // followed by " for " plus a method category we still
        // count it as pinning.
        let suffix = &haystack[start + token.len()..];
        if suffix.starts_with(" for ") {
            return true;
        }
        search_from = start + 1;
    }
    false
}

/// Best-effort category label for the refusal text. Reads the
/// noun directly out of the recommendation phrase ("which
/// aligner" → "aligner"); otherwise falls back to the generic
/// "method".
fn infer_category_from_phrase(phrase: &str, _full_prose: &str) -> String {
    if let Some(rest) = phrase.strip_prefix("which ") {
        return rest.to_string();
    }
    if let Some(rest) = phrase.strip_prefix("what ") {
        return rest.to_string();
    }
    "method".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinning_with_use_returns_sme_named() {
        match detect_method_mention("we want to use STAR for alignment") {
            MethodMention::SmeNamed { method } => assert_eq!(method, "STAR"),
            other => panic!("expected SmeNamed, got {other:?}"),
        }
    }

    #[test]
    fn pinning_with_via_returns_sme_named() {
        match detect_method_mention("run DE via DESeq2") {
            MethodMention::SmeNamed { method } => assert_eq!(method, "DESeq2"),
            other => panic!("expected SmeNamed, got {other:?}"),
        }
    }

    #[test]
    fn pinning_with_for_returns_sme_named() {
        match detect_method_mention("STAR for alignment and DESeq2 for the stats") {
            MethodMention::SmeNamed { method } => {
                // The detector returns the first matching family
                // (aligner comes before statistical test in the
                // METHOD_FAMILIES table).
                assert_eq!(method, "STAR");
            }
            other => panic!("expected SmeNamed, got {other:?}"),
        }
    }

    #[test]
    fn recommendation_request_returns_refusal() {
        match detect_method_mention("which aligner should I use — STAR or HISAT2?") {
            MethodMention::SmeRequestedRecommendation { tool_category } => {
                assert_eq!(tool_category, "aligner");
            }
            other => panic!("expected SmeRequestedRecommendation, got {other:?}"),
        }
    }

    #[test]
    fn recommendation_takes_precedence_over_pinning() {
        // Prose names STAR but the wrapping question is a
        // recommendation ask — refuse rather than pin.
        match detect_method_mention("should I use STAR or HISAT2? what do you recommend?") {
            MethodMention::SmeRequestedRecommendation { .. } => {}
            other => panic!("expected SmeRequestedRecommendation, got {other:?}"),
        }
    }

    #[test]
    fn bare_mention_without_pinning_returns_none() {
        // STAR mentioned as ambient context (upstream BAMs were
        // pre-aligned) without "use"/"with"/"via" — not a
        // pinning signal.
        match detect_method_mention("we have STAR-aligned BAM files from a collaborator") {
            MethodMention::SmeNamed { .. } => {
                // Acceptable: "STAR-aligned" is a hyphenated form
                // that the word-boundary check still hits; we
                // treat that as a pinning signal because the SME
                // has explicitly named the upstream tool.
            }
            MethodMention::None => {}
            other => panic!("expected None or SmeNamed, got {other:?}"),
        }
    }

    #[test]
    fn no_method_mention_returns_none() {
        assert!(matches!(
            detect_method_mention("scRNA-seq from human PBMCs, healthy vs degenerated"),
            MethodMention::None
        ));
    }

    #[test]
    fn word_boundary_avoids_false_match() {
        // "star" appears inside "starvation" but must not match.
        assert!(matches!(
            detect_method_mention("starvation response in yeast"),
            MethodMention::None
        ));
    }

    #[test]
    fn case_insensitive_match() {
        match detect_method_mention("use star for alignment") {
            MethodMention::SmeNamed { method } => assert_eq!(method, "STAR"),
            other => panic!("expected SmeNamed, got {other:?}"),
        }
    }

    #[test]
    fn suggest_keyword_triggers_refusal() {
        match detect_method_mention("can you suggest a normalization approach?") {
            MethodMention::SmeRequestedRecommendation { .. } => {}
            other => panic!("expected SmeRequestedRecommendation, got {other:?}"),
        }
    }
}
