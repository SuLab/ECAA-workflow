//! Deterministic biomedical-subfield selector: a pure function over the
//! classified goal text + the Task-2 [`crate::ensemble_subfield::SubfieldCatalog`].
//! No LLM, no wall clock — scores each catalog entry's `select_keywords`
//! against the normalized goal text and returns the top `s_max` matches at
//! or above `min_score`, ranked by score desc then id asc for a
//! deterministic tie-break.

use crate::ensemble_subfield::SubfieldCatalog;

/// One subfield selected for a goal, with the keywords that matched.
#[derive(Debug, Clone, PartialEq)]
pub struct SelectedSubfield {
    /// The selected subfield's stable id.
    pub id: String,
    /// The (sorted, deduplicated) `select_keywords` that matched the goal
    /// text.
    pub matched_keywords: Vec<String>,
}

/// Default cap on the number of subfields selected per goal.
pub const S_MAX: usize = 3;
/// Default minimum keyword-hit count for a subfield to be selected.
pub const MIN_SELECT_SCORE: usize = 1;

/// Score every subfield in `catalog` against `match_text` by counting how
/// many of its `select_keywords` appear (case/separator-insensitive, via
/// [`crate::classify::normalize_for_match`]) in the normalized text. Keeps
/// subfields scoring at least `min_score`, ranks by score desc then id asc
/// (deterministic tie-break — `catalog.by_id` is already a `BTreeMap`, but
/// the explicit sort makes ranking unambiguous), and returns at most
/// `s_max` results.
pub fn select_subfields(
    match_text: &str,
    catalog: &SubfieldCatalog,
    s_max: usize,
    min_score: usize,
) -> Vec<SelectedSubfield> {
    // Word-boundary match: lowercase, every non-alphanumeric char → space, and
    // require a space (word start) immediately before the keyword. So "variant"
    // does NOT match inside "invariant" and "kinetics" not inside
    // "pharmacokinetics" (the substring false-positives the adversarial audit
    // found), while a trailing suffix is still allowed so plurals
    // ("variant" → "variants") still match.
    let norm = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { ' ' })
            .collect::<String>()
    };
    let hay = format!(" {}", norm(match_text));
    let mut scored: Vec<(usize, SelectedSubfield)> = catalog
        .by_id
        .values()
        .filter_map(|sf| {
            let mut hits: Vec<String> = sf
                .select_keywords
                .iter()
                .filter(|kw| hay.contains(&format!(" {}", norm(kw))))
                .cloned()
                .collect();
            hits.sort();
            hits.dedup();
            (hits.len() >= min_score).then(|| {
                (
                    hits.len(),
                    SelectedSubfield {
                        id: sf.id.clone(),
                        matched_keywords: hits,
                    },
                )
            })
        })
        .collect();
    // rank: score desc, then id asc (deterministic)
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored.into_iter().take(s_max).map(|(_, s)| s).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensemble_subfield::SubfieldLens;
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn lens(id: &str, keywords: &[&str]) -> SubfieldLens {
        SubfieldLens {
            schema_version: "0.1".to_string(),
            id: id.to_string(),
            persona_ref: format!("{id}.md"),
            model_tier: "sonnet".to_string(),
            retrieval: "recent".to_string(),
            select_keywords: keywords.iter().map(|k| k.to_string()).collect(),
        }
    }

    fn test_catalog() -> SubfieldCatalog {
        let mut by_id = BTreeMap::new();
        by_id.insert(
            "immunology".to_string(),
            lens(
                "immunology",
                &["t cell", "inflammation", "cytokine", "immune"],
            ),
        );
        by_id.insert(
            "oncology".to_string(),
            lens("oncology", &["tumor", "carcinoma", "oncogene", "metastasis"]),
        );
        SubfieldCatalog {
            by_id,
            root: PathBuf::new(),
        }
    }

    #[test]
    fn selects_immunology_for_immune_goal() {
        let cat = test_catalog();
        let got = select_subfields("role of T cell inflammation in asthma", &cat, 3, 1);
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["immunology"]
        );
    }

    #[test]
    fn empty_when_no_keyword_matches() {
        assert!(select_subfields("generic widget throughput", &test_catalog(), 3, 1).is_empty());
    }

    #[test]
    fn word_boundary_avoids_substring_false_positives() {
        let mut by_id = BTreeMap::new();
        by_id.insert("genetics".to_string(), lens("genetics", &["variant", "genome"]));
        by_id.insert("biophysics".to_string(), lens("biophysics", &["kinetics", "conformation"]));
        let cat = SubfieldCatalog {
            by_id,
            root: PathBuf::new(),
        };
        // "invariant" must NOT match keyword "variant", and "pharmacokinetics"
        // must NOT match "kinetics" — both are substrings, not word-boundary hits.
        assert!(
            select_subfields(
                "invariant natural killer T cells; pharmacokinetics of the drug",
                &cat,
                3,
                1
            )
            .is_empty(),
            "substring-only matches must be rejected by word-boundary matching"
        );
        // A real whole-word hit still matches, including its plural.
        let got = select_subfields("germline variants across the genome", &cat, 3, 1);
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["genetics"]
        );
    }

    #[test]
    fn ties_broken_by_id_and_capped_at_s_max() {
        let cat = test_catalog();
        // "immune" hits immunology once; "tumor" hits oncology once — a tie.
        let got = select_subfields("immune tumor study", &cat, 3, 1);
        assert_eq!(
            got.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["immunology", "oncology"],
            "equal-score subfields must sort by id ascending"
        );
        assert!(got.len() <= 3);

        // Cap at s_max=1 keeps only the first by the tie-break order.
        let capped = select_subfields("immune tumor study", &cat, 1, 1);
        assert_eq!(capped.len(), 1);
        assert_eq!(capped[0].id, "immunology");
    }

    #[test]
    fn deterministic_across_repeats() {
        let cat = test_catalog();
        let a = select_subfields("role of T cell inflammation in asthma", &cat, 3, 1);
        let b = select_subfields("role of T cell inflammation in asthma", &cat, 3, 1);
        assert_eq!(a, b);
    }
}
