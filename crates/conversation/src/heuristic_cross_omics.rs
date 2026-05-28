//! Cross-omics conjunction detector for the heuristic mock backend.
//!
//! The classifier in `crates/core/src/classify.rs::is_cross_omics_intent`
//! is the gold-standard pattern set: it differentiates the SME saying
//! "joint analysis of RNA-seq and ATAC-seq" (a real cross-omics signal)
//! from "we have RNA-seq data; we also have ATAC-seq from a different
//! cohort we plan to compare later" (two analyses, not one). The
//! heuristic backend needs the same disambiguation when picking which
//! tool to dispatch next — without it, the heuristic would fire
//! `classify_intake` immediately and skip the `propose_quick_replies`
//! confirmation round that the live LLM walks under §S6.4.
//!
//! This module is a *narrower* mirror of the classifier's logic. The
//! classifier's job is to label the intake with a primary modality +
//! companions; this module's job is to answer a yes/no — "is the prose
//! carrying a cross-omics conjunction signal that should gate the next
//! tool pick?" — and, when yes, return the ordered modality list so the
//! heuristic's quick-replies prompt can echo it back to the SME.
//!
//! The conjunction signal is one of:
//!   - "X and Y" where X and Y are distinct modality keywords
//!   - "joint analysis of X and Y"
//!   - "integrate X with Y"
//!   - "X combined with Y"
//!   - "paired X and Y from same sample"
//!
//! Plain "we have RNA-seq; also ATAC-seq from another study" should NOT
//! match — semicolons and "also" don't carry the same joint-analysis
//! intent. The conjunction gate is the load-bearing piece.

/// Detected cross-omics conjunction signal in SME prose. See module
/// docs for the gate list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossOmicsSignal {
    /// The conjunction phrase that triggered the match, in normalized
    /// (lowercased, dash-collapsed) form. `None` only when the match
    /// came from a strong marker ("multiomics", "multi-omic") without a
    /// distinct conjunction span.
    pub conjunction_phrase: Option<String>,
    /// Modality keywords in prose order. Guaranteed length >= 2 on a
    /// non-None return; deduplicated.
    pub modalities: Vec<String>,
}

/// Detect whether the SME prose carries a cross-omics conjunction
/// signal — distinct from merely mentioning two modality keywords.
///
/// Returns `Some(signal)` only when (a) two or more distinct modality
/// keywords appear AND (b) at least one explicit conjunction phrase
/// joins them. Returns `None` for "RNA-seq data; also have ATAC-seq",
/// which mentions both modalities but describes two separate analyses.
///
/// Cross-reference: `crates/core/src/classify.rs::is_cross_omics_intent`
/// for the broader classifier gate (this module is a narrower mirror;
/// the classifier owns the source-of-truth pattern set).
pub fn detect_cross_omics(prose: &str) -> Option<CrossOmicsSignal> {
    let normalized = normalize(prose);

    // Strong markers — when present, treat as cross-omics regardless of
    // whether a distinct conjunction span anchors them. Mirrors
    // `classify::has_strong_cross_omics_marker` (narrower subset).
    const STRONG_MARKERS: &[&str] = &[
        "multiomics",
        "multi omic",
        "multi omics",
        "cross omics",
        "cross omic",
        "joint omics",
        "joint analysis",
        "integrated analysis",
        "combined analysis",
        "multiome",
    ];

    // Modality keyword list. Kept aligned with the classifier's
    // MODALITY_NOUNS / COMPOUND_MULTI_MODAL sets, narrowed to the
    // tokens SMEs reliably use in conjunction phrasing. Order doesn't
    // affect correctness; we use first-occurrence to seed prose order.
    const MODALITY_KEYWORDS: &[&str] = &[
        "scrna seq",
        "snrna seq",
        "rna seq",
        "rnaseq",
        "scrna",
        "snrna",
        "atac seq",
        "atacseq",
        "scatac",
        "snatac",
        "chip seq",
        "chipseq",
        "cut and run",
        "cut and tag",
        "cut tag",
        "bisulfite",
        "wgbs",
        "rrbs",
        "methylation",
        "methylome",
        "proteomics",
        "proteome",
        "phosphoproteomics",
        "phosphoproteome",
        "metabolomics",
        "lipidomics",
        "glycomics",
        "transcriptomics",
        "epigenomics",
        "genomics",
        "metagenomics",
        "mass spectrometry",
        "mass spec",
        "spatial transcriptomics",
        "chromatin accessibility",
        "variant calling",
        "whole genome sequencing",
        "whole exome sequencing",
    ];

    // Conjunction patterns that gate the signal. The exact phrase
    // governs whether two modality keywords count as a cross-omics
    // signal. "and" inside a list of two modalities counts; "also" /
    // ";" / ". " do not.
    //
    // Each pattern is (template, span_kind). `Joining` matches when
    // both modalities flank the conjunction; `Wrapped` matches when
    // they sit inside a leading-clause template.
    let modalities_in_prose = collect_modalities_in_order(&normalized, MODALITY_KEYWORDS);
    if modalities_in_prose.len() < 2 {
        return None;
    }

    // Strong-marker fast path — requires at least 2 distinct modalities
    // in prose (guarded above) but doesn't require a conjunction span.
    for marker in STRONG_MARKERS {
        if let Some(idx) = normalized.find(marker) {
            return Some(CrossOmicsSignal {
                conjunction_phrase: Some(
                    extract_phrase_around(&normalized, idx, marker.len()).to_string(),
                ),
                modalities: modalities_in_prose,
            });
        }
    }

    // Joining conjunctions: " X <conj> Y " where X and Y are both
    // modality keywords. The leading/trailing space requirement
    // enforces a word boundary cheaply.
    const JOINING_CONJUNCTIONS: &[&str] =
        &[" and ", " combined with ", " together with ", " plus "];
    for conj in JOINING_CONJUNCTIONS {
        if let Some(phrase) = find_joining_conjunction(&normalized, conj, MODALITY_KEYWORDS) {
            return Some(CrossOmicsSignal {
                conjunction_phrase: Some(phrase),
                modalities: modalities_in_prose,
            });
        }
    }

    // Wrapped templates: "joint analysis of X and Y", "integrate X
    // with Y", "paired X and Y". These carry the conjunction signal
    // even when the simple " <conj> " scan above misses (e.g. when the
    // modalities are separated by extra adjectives).
    const WRAPPED_TEMPLATES: &[(&str, &str)] = &[
        ("joint analysis of ", " and "),
        ("integrate ", " with "),
        ("integrating ", " with "),
        ("paired ", " and "),
        ("pairing ", " and "),
    ];
    for (lead, sep) in WRAPPED_TEMPLATES {
        if let Some(phrase) = find_wrapped_template(&normalized, lead, sep, MODALITY_KEYWORDS) {
            return Some(CrossOmicsSignal {
                conjunction_phrase: Some(phrase),
                modalities: modalities_in_prose,
            });
        }
    }

    None
}

/// Lowercase + replace hyphens / underscores / slashes with spaces so
/// "RNA-seq", "RNA_seq", "RNA/seq" all normalize to "rna seq". Keeps
/// commas, semicolons, and periods so list / sentence boundaries
/// remain visible to the conjunction scan.
fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let lower = ch.to_ascii_lowercase();
        let mapped = match lower {
            '-' | '_' | '/' => ' ',
            _ => lower,
        };
        out.push(mapped);
    }
    // Collapse runs of whitespace so " rna  seq " → " rna seq ".
    let mut collapsed = String::with_capacity(out.len());
    let mut prev_space = false;
    for ch in out.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                collapsed.push(' ');
            }
            prev_space = true;
        } else {
            collapsed.push(ch);
            prev_space = false;
        }
    }
    collapsed
}

/// Scan `text` left to right and collect each modality keyword in the
/// order it first appears. Deduplicates: a single modality mentioned
/// twice still counts as one entry.
fn collect_modalities_in_order(text: &str, keywords: &[&str]) -> Vec<String> {
    // Build a list of (start_idx, keyword) for every keyword occurrence,
    // then sort by start_idx, then dedupe by keyword preserving first
    // occurrence. Avoids overlap quirks (e.g. "rna seq" matches inside
    // "scrna seq") by preferring the longer hit at the same offset:
    // we scan longest-first and skip an offset that's already claimed
    // by a prior (longer) keyword.
    let mut by_len: Vec<&&str> = keywords.iter().collect();
    by_len.sort_by_key(|k| std::cmp::Reverse(k.len()));

    let mut hits: Vec<(usize, String)> = Vec::new();
    let mut claimed: Vec<(usize, usize)> = Vec::new(); // (start, end)
    for kw in by_len {
        let mut search_start = 0;
        while let Some(rel) = text[search_start..].find(*kw) {
            let abs = search_start + rel;
            let end = abs + kw.len();
            let overlaps = claimed.iter().any(|(s, e)| !(end <= *s || abs >= *e));
            if !overlaps {
                hits.push((abs, kw.to_string()));
                claimed.push((abs, end));
            }
            search_start = abs + kw.len();
        }
    }
    hits.sort_by_key(|(idx, _)| *idx);

    // Canonicalize: collapse keyword aliases to the same canonical token
    // so "rna seq" and "rnaseq" don't both appear. Mapping is narrow —
    // only the alias pairs SMEs interchange.
    fn canonical(kw: &str) -> &str {
        match kw {
            "rnaseq" => "rna seq",
            "atacseq" => "atac seq",
            "chipseq" => "chip seq",
            "scrna" => "scrna",
            "scrna seq" => "scrna seq",
            "snrna" => "snrna",
            "snrna seq" => "snrna seq",
            "scatac" => "scatac",
            "snatac" => "snatac",
            "proteome" => "proteomics",
            "phosphoproteome" => "phosphoproteomics",
            "methylome" => "methylation",
            "wgbs" => "bisulfite",
            "rrbs" => "bisulfite",
            "mass spec" => "mass spectrometry",
            other => other,
        }
    }

    let mut seen: Vec<String> = Vec::new();
    for (_, kw) in hits {
        let c = canonical(&kw).to_string();
        if !seen.iter().any(|s| s == &c) {
            seen.push(c);
        }
    }
    seen
}

/// Find ` X <conj> Y ` where X and Y are distinct modality keywords.
/// Returns the matched phrase (e.g. "rna seq and atac seq") when one
/// exists, `None` otherwise. The conjunction must be space-padded so
/// "stand" or "andrew" can't false-positive on a bare "and".
fn find_joining_conjunction(text: &str, conj: &str, keywords: &[&str]) -> Option<String> {
    let mut search_start = 0;
    while let Some(rel) = text[search_start..].find(conj) {
        let conj_idx = search_start + rel;
        let before = &text[..conj_idx];
        let after = &text[conj_idx + conj.len()..];

        // Find the latest (closest to the conjunction) modality on the
        // before side and the earliest on the after side so the matched
        // phrase is the *adjacent* conjunction, not a far-away pair
        // that happens to flank an unrelated "and".
        let before_hit = keywords
            .iter()
            .filter_map(|kw| before.rfind(kw).map(|i| (i, *kw)))
            .max_by_key(|(i, _)| *i);
        let after_hit = keywords
            .iter()
            .filter_map(|kw| after.find(kw).map(|i| (i, *kw)))
            .min_by_key(|(i, _)| *i);

        if let (Some((b_idx, b_kw)), Some((a_idx, a_kw))) = (before_hit, after_hit) {
            // Require the modality hits to sit close to the conjunction
            // — within ~16 chars on each side — so far-flung modality
            // mentions in adjacent sentences don't trip the gate. The
            // tighter bound (was 32) keeps cases like
            // `"rna seq from disc tissue last summer, and ... methylation"`
            // out: that's 29 chars between `rna seq` and ` and `,
            // semantically two clauses joined by a discourse `and`, not
            // a conjunction of modalities.
            let before_gap = conj_idx.saturating_sub(b_idx + b_kw.len());
            let after_gap = a_idx;
            if b_kw != a_kw && before_gap <= 16 && after_gap <= 16 {
                let span_start = b_idx;
                let span_end = conj_idx + conj.len() + a_idx + a_kw.len();
                return Some(text[span_start..span_end].to_string());
            }
        }

        search_start = conj_idx + conj.len();
    }
    None
}

/// Match a wrapped template like "joint analysis of X and Y" or
/// "integrate X with Y". `lead` and `sep` bracket the two modality
/// slots; the modalities must sit within the same ~64-char window so
/// the template doesn't span paragraphs.
fn find_wrapped_template(text: &str, lead: &str, sep: &str, keywords: &[&str]) -> Option<String> {
    let mut search_start = 0;
    while let Some(rel) = text[search_start..].find(lead) {
        let lead_idx = search_start + rel;
        let after_lead = &text[lead_idx + lead.len()..];
        // Look for the separator within the next 80 chars.
        let window_end = std::cmp::min(after_lead.len(), 80);
        let window = &after_lead[..window_end];
        if let Some(sep_idx) = window.find(sep) {
            let before_sep = &window[..sep_idx];
            let after_sep = &window[sep_idx + sep.len()..];
            let b_hit = keywords.iter().find(|kw| before_sep.contains(*kw));
            let a_hit = keywords.iter().find(|kw| after_sep.contains(*kw));
            if let (Some(b), Some(a)) = (b_hit, a_hit) {
                if b != a {
                    let abs_end = lead_idx
                        + lead.len()
                        + sep_idx
                        + sep.len()
                        + after_sep.find(a).unwrap_or(0)
                        + a.len();
                    return Some(text[lead_idx..abs_end].to_string());
                }
            }
        }
        search_start = lead_idx + lead.len();
    }
    None
}

/// Extract a short snippet around a marker hit so the returned
/// `conjunction_phrase` carries useful context. Caps at 40 chars to
/// keep the field short.
fn extract_phrase_around(text: &str, idx: usize, len: usize) -> &str {
    let start = idx.saturating_sub(8);
    let end = std::cmp::min(text.len(), idx + len + 16);
    // Backstep to a char boundary on both ends.
    let mut s = start;
    while s > 0 && !text.is_char_boundary(s) {
        s -= 1;
    }
    let mut e = end;
    while e < text.len() && !text.is_char_boundary(e) {
        e += 1;
    }
    &text[s..e]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joint_analysis_template_matches() {
        let prose = "joint analysis of bulk RNA-seq and ATAC-seq from human PBMCs";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        assert!(signal.modalities.len() >= 2);
        assert!(signal.modalities.iter().any(|m| m == "rna seq"));
        assert!(signal.modalities.iter().any(|m| m == "atac seq"));
    }

    #[test]
    fn integrate_with_template_matches() {
        let prose = "We want to integrate RNA-seq with ChIP-seq across the same tissue.";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        let phrase = signal.conjunction_phrase.unwrap();
        assert!(phrase.contains("integrate") || phrase.contains("with"));
    }

    #[test]
    fn paired_template_matches() {
        let prose = "paired RNA-seq and ATAC-seq from same sample";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        assert_eq!(signal.modalities.len(), 2);
    }

    #[test]
    fn combined_with_template_matches() {
        let prose = "RNA-seq combined with proteomics from matched donors";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        assert!(signal.modalities.iter().any(|m| m == "proteomics"));
        assert!(signal.modalities.iter().any(|m| m == "rna seq"));
    }

    #[test]
    fn strong_marker_multiomics_matches() {
        let prose = "multiomics study comparing RNA-seq and ATAC-seq profiles";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        assert!(signal.modalities.len() >= 2);
    }

    #[test]
    fn semicolon_separated_two_modalities_does_not_match() {
        // Regression guard: two modality keywords present, but no
        // conjunction joins them — describes two separate analyses,
        // not a cross-omics joint analysis.
        let prose = "bulk RNA-seq differential expression from human PBMCs. \
                     We also have proteomics from a different study we want \
                     to compare later.";
        assert!(detect_cross_omics(prose).is_none());
    }

    #[test]
    fn single_modality_does_not_match() {
        let prose = "scRNA-seq from intervertebral disc tissue, 47 libraries";
        assert!(detect_cross_omics(prose).is_none());
    }

    #[test]
    fn far_away_modalities_with_unrelated_and_does_not_match() {
        // " and " here joins clauses, not modalities. The modality hits
        // are far from the conjunction so the proximity guard rejects.
        let prose = "We collected RNA-seq from disc tissue last summer, and \
                     a separate cohort has methylation data from 2019.";
        // The proximity guard requires modality keywords within ~32
        // chars of the conjunction; "RNA-seq" and "methylation" sit
        // farther apart, so no signal. (The strong-marker fast path
        // is also gated on a marker phrase which is absent here.)
        assert!(detect_cross_omics(prose).is_none());
    }

    #[test]
    fn modality_order_preserved_in_prose_order() {
        let prose = "joint analysis of ATAC-seq and RNA-seq";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        assert_eq!(signal.modalities[0], "atac seq");
        assert_eq!(signal.modalities[1], "rna seq");
    }

    #[test]
    fn deduplicates_repeated_modality() {
        let prose = "joint analysis of RNA-seq and ATAC-seq with extra RNA-seq controls";
        let signal = detect_cross_omics(prose).expect("expected cross-omics signal");
        // RNA-seq mentioned twice should appear once in the list.
        let rna_count = signal
            .modalities
            .iter()
            .filter(|m| m.as_str() == "rna seq")
            .count();
        assert_eq!(rna_count, 1);
    }
}
