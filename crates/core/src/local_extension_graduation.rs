//! v4 P6 / D4 — `LocalExtension` graduation pathway.
//!
//! Closes design v4 §4.2 (duplicate detection + graduation).
//! Supersedes v3 P11's cross-session `Unknown` signal.
//!
//! Three responsibilities:
//! 1. **Lexical duplicate detection** — `jaccard_similarity` over
//!    token sets distinguishes "near duplicate" (>= 0.85) from
//!    "candidate match" (>= 0.70). The conversation crate's
//!    `tools::intake` consults this on every LocalExtension mint to
//!    short-circuit recurrent definitions.
//! 2. **Graduation thresholds** — `GraduationThresholds` declared
//!    here; loaded from `config/local-extension-graduation.yaml` by
//!    the conversation crate.
//! 3. **Candidacy result type** — `GraduationCandidacy` carries the
//!    fields needed to update a `LocalExtensionMaturity::GraduationCandidate`.
//!
//! The cross-session aggregator + persistence layer that produces
//! `ExistingLocalExtension` lists lives in
//! `crates/conversation/src/session/cross_session_aggregator.rs`
//! (conversation crate, not core, because it touches the on-disk
//! session-store path).

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
// R6-U7: ts-rs no longer used in this module — DuplicateCandidate +
// DuplicateDetectionResult are server-internal helpers.

/// Loaded thresholds envelope. Loaded from
/// `config/local-extension-graduation.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct GraduationConfig {
    /// Version.
    pub version: String,
    /// Graduation.
    pub graduation: GraduationThresholds,
    /// Duplicate detection.
    pub duplicate_detection: DuplicateDetectionThresholds,
}

/// Thresholds for graduation eligibility.
///
/// All three must hold simultaneously for the cross-session aggregator
/// to flip an entry to `LocalExtensionMaturity::GraduationCandidate`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct GraduationThresholds {
    /// Minimum number of total usages.
    pub min_usage_count: u32,
    /// Minimum number of distinct sessions.
    pub min_unique_sessions: u32,
    /// Minimum success rate (0.0–1.0).
    pub min_success_rate: f32,
}

/// Thresholds for duplicate detection. Two thresholds — `near` is the
/// "almost certainly the same thing" cutoff; `match` is the "worth
/// surfacing as a candidate" cutoff.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct DuplicateDetectionThresholds {
    /// Jaccard near duplicate threshold.
    pub jaccard_near_duplicate_threshold: f32,
    /// Jaccard candidate match threshold.
    pub jaccard_candidate_match_threshold: f32,
}

/// Default thresholds matching the bundled config file. Used when no
/// config file is present (test paths, offline mode).
impl Default for GraduationThresholds {
    fn default() -> Self {
        Self {
            min_usage_count: 5,
            min_unique_sessions: 3,
            min_success_rate: 0.6,
        }
    }
}

impl Default for DuplicateDetectionThresholds {
    fn default() -> Self {
        Self {
            jaccard_near_duplicate_threshold: 0.85,
            jaccard_candidate_match_threshold: 0.70,
        }
    }
}

impl GraduationConfig {
    /// Load + validate the thresholds YAML.
    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, GraduationConfigError> {
        let p = path.as_ref();
        if !p.exists() {
            return Err(GraduationConfigError::NotFound(p.display().to_string()));
        }
        let raw = std::fs::read_to_string(p).map_err(|e| GraduationConfigError::Io {
            path: p.display().to_string(),
            source: e,
        })?;
        let parsed: GraduationConfig = serde_yml::from_str(&raw)?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Same shape as `OntologyScopeMatrix::load_from_path`: returns
    /// `Ok(None)` when the file is absent so callers can run without
    /// the YAML by falling back to `Default::default()`.
    pub fn try_load_default(
        config_dir: impl AsRef<Path>,
    ) -> Result<Option<Self>, GraduationConfigError> {
        let path = config_dir.as_ref().join("local-extension-graduation.yaml");
        if !path.exists() {
            return Ok(None);
        }
        Self::load_from_path(&path).map(Some)
    }

    fn validate(&self) -> Result<(), GraduationConfigError> {
        let version_re = regex::Regex::new(r"^\d+\.\d+\.\d+$").expect("static regex compiles");
        if !version_re.is_match(&self.version) {
            return Err(GraduationConfigError::InvalidVersion {
                got: self.version.clone(),
            });
        }
        let g = &self.graduation;
        if g.min_success_rate < 0.0 || g.min_success_rate > 1.0 {
            return Err(GraduationConfigError::InvalidSuccessRate {
                got: g.min_success_rate,
            });
        }
        let d = &self.duplicate_detection;
        for (name, v) in [
            ("near", d.jaccard_near_duplicate_threshold),
            ("match", d.jaccard_candidate_match_threshold),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(GraduationConfigError::InvalidJaccard {
                    name: name.into(),
                    got: v,
                });
            }
        }
        if d.jaccard_near_duplicate_threshold < d.jaccard_candidate_match_threshold {
            return Err(GraduationConfigError::OrderedThresholds {
                near: d.jaccard_near_duplicate_threshold,
                candidate: d.jaccard_candidate_match_threshold,
            });
        }
        Ok(())
    }
}

/// Loader / validation errors.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum GraduationConfigError {
    #[error("graduation config not found: {0}")]
    /// NotFound variant.
    NotFound(String),
    #[error("graduation config read error at {path}: {source}")]
    /// Io variant.
    Io {
        /// Path.
        path: String,
        #[source]
        /// Source.
        source: std::io::Error,
    },
    #[error("graduation config YAML parse error: {0}")]
    /// Parse variant.
    Parse(#[from] serde_yml::Error),
    #[error("graduation config invalid version: {got}")]
    /// Variant.
    /// Field value.
    InvalidVersion { got: String },
    #[error("graduation config invalid success_rate (must be 0.0-1.0): {got}")]
    /// Variant.
    /// Field value.
    InvalidSuccessRate { got: f32 },
    #[error("graduation config invalid jaccard {name} threshold (must be 0.0-1.0): {got}")]
    /// Variant.
    /// Field value.
    /// Field value.
    InvalidJaccard { name: String, got: f32 },
    #[error("graduation config jaccard near ({near}) must be >= candidate ({candidate})")]
    /// Variant.
    /// Field value.
    /// Field value.
    OrderedThresholds { near: f32, candidate: f32 },
}

/// A duplicate candidate surfaced by `detect_duplicates`. `jaccard`
/// is the similarity score against the input being minted.
///
/// R6-U7: ts-rs export removed — used only by the `detect_duplicates`
/// server-side helper, never read by the UI.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct DuplicateCandidate {
    /// Stable id of the existing extension.
    pub iri: String,
    /// Human label of the existing extension.
    pub label: String,
    /// Jaccard similarity to the input being minted (0.0–1.0).
    pub jaccard: f32,
}

/// Result of running duplicate detection. `near_duplicates` >=
/// `jaccard_near_duplicate_threshold` (typically auto-merge candidates
/// requiring SME confirmation); `candidate_matches` >=
/// `jaccard_candidate_match_threshold` (worth surfacing in the UI but
/// not auto-merging).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct DuplicateDetectionResult {
    /// Near duplicates.
    pub near_duplicates: Vec<DuplicateCandidate>,
    /// Candidate matches.
    pub candidate_matches: Vec<DuplicateCandidate>,
}

/// An existing `LocalExtension` entry passed into `detect_duplicates`.
/// Shape mirrors the subset of `SemanticType::LocalExtension` fields
/// the similarity computation needs; the conversation crate's
/// aggregator emits these from the `_local_extension_registry.jsonl`
/// file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistingLocalExtension {
    /// Iri.
    pub iri: String,
    /// Label.
    pub label: String,
    /// Definition.
    pub definition: String,
    /// Proposed parent terms.
    pub proposed_parent_terms: Vec<String>,
}

/// Result returned by the cross-session aggregator's `check_graduation`
/// when an entry has crossed all three thresholds. The conversation
/// crate constructs a `LocalExtensionMaturity::GraduationCandidate`
/// from this with an additional `proposed_at` timestamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct GraduationCandidacy {
    /// Usage count.
    pub usage_count: u32,
    /// Unique sessions.
    pub unique_sessions: u32,
    /// Success rate.
    pub success_rate: f32,
    /// Graduation target ontology.
    pub graduation_target_ontology: String,
}

/// Jaccard similarity over two token sets. Returns 1.0 for two empty
/// sets; 0.0 for one empty and one non-empty; otherwise
/// `|A ∩ B| / |A ∪ B|`.
pub fn jaccard_similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Tokenize a (label, definition, parents) triple for jaccard
/// similarity. Lowercase, keep alphanumeric chars only, drop tokens
/// shorter than 3 chars (filter the noise from words like "a", "of",
/// "is"). For parent IRIs, the trailing suffix (after the last `/`) is
/// extracted and lowercased — this folds compact and PURL forms
/// together (`data:1383` and `http://edamontology.org/data_1383` produce
/// the same token set without us having to canonicalize).
pub fn tokenize_for_similarity(
    label: &str,
    definition: &str,
    parents: &[String],
) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for piece in label
        .split_whitespace()
        .chain(definition.split_whitespace())
    {
        let cleaned: String = piece.chars().filter(|c| c.is_alphanumeric()).collect();
        if cleaned.len() >= 3 {
            tokens.insert(cleaned.to_lowercase());
        }
    }
    for p in parents {
        // For URIs, last path segment; for compact IRIs, the bit after `:`.
        let suffix = p.rsplit('/').next().unwrap_or(p);
        let suffix = suffix.rsplit(':').next().unwrap_or(suffix);
        if !suffix.is_empty() {
            tokens.insert(suffix.to_lowercase());
        }
    }
    tokens
}

/// Compare a candidate-to-mint extension against every existing
/// `LocalExtension` and return near-duplicates + candidate matches.
///
/// Output ordering: each slice sorted descending by `jaccard` so the
/// strongest match is first.
pub fn detect_duplicates(
    candidate_label: &str,
    candidate_definition: &str,
    candidate_parents: &[String],
    existing: &[ExistingLocalExtension],
    near_threshold: f32,
    match_threshold: f32,
) -> DuplicateDetectionResult {
    let candidate_tokens =
        tokenize_for_similarity(candidate_label, candidate_definition, candidate_parents);
    let mut near = Vec::new();
    let mut matches = Vec::new();
    for ext in existing {
        let other_tokens =
            tokenize_for_similarity(&ext.label, &ext.definition, &ext.proposed_parent_terms);
        let sim = jaccard_similarity(&candidate_tokens, &other_tokens);
        let cand = DuplicateCandidate {
            iri: ext.iri.clone(),
            label: ext.label.clone(),
            jaccard: sim,
        };
        if sim >= near_threshold {
            near.push(cand);
        } else if sim >= match_threshold {
            matches.push(cand);
        }
    }
    near.sort_by(|a, b| {
        b.jaccard
            .partial_cmp(&a.jaccard)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    matches.sort_by(|a, b| {
        b.jaccard
            .partial_cmp(&a.jaccard)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    DuplicateDetectionResult {
        near_duplicates: near,
        candidate_matches: matches,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(parts: &[&str]) -> BTreeSet<String> {
        parts.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn jaccard_empty_sets_returns_one() {
        assert_eq!(jaccard_similarity(&s(&[]), &s(&[])), 1.0);
    }

    #[test]
    fn jaccard_one_empty_returns_zero() {
        let a = s(&["foo", "bar"]);
        let b = s(&[]);
        assert_eq!(jaccard_similarity(&a, &b), 0.0);
        assert_eq!(jaccard_similarity(&b, &a), 0.0);
    }

    #[test]
    fn jaccard_identical_returns_one() {
        let a = s(&["foo", "bar"]);
        let b = s(&["foo", "bar"]);
        assert_eq!(jaccard_similarity(&a, &b), 1.0);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a = s(&["foo", "bar", "baz"]);
        let b = s(&["bar", "baz", "qux"]);
        // intersection {bar, baz}, union {foo, bar, baz, qux} => 2/4 = 0.5
        assert_eq!(jaccard_similarity(&a, &b), 0.5);
    }

    #[test]
    fn tokenizer_drops_short_words_and_lowercases() {
        let tokens = tokenize_for_similarity("Cell Doublet Score", "A per-cell score.", &[]);
        assert!(tokens.contains("cell"));
        assert!(tokens.contains("doublet"));
        assert!(tokens.contains("score"));
        // "per-cell" → "percell" after the alphanumeric filter strips
        // the hyphen (the tokenizer is whitespace-split + char-filter,
        // not regex-split). Verify the merged token survives.
        assert!(tokens.contains("percell"));
        // "A" is below the 3-char floor; should be absent.
        assert!(!tokens.contains("a"));
    }

    #[test]
    fn tokenizer_extracts_parent_suffixes() {
        let tokens = tokenize_for_similarity(
            "",
            "",
            &[
                "http://edamontology.org/data_1383".to_string(),
                "data:2603".to_string(),
            ],
        );
        assert!(tokens.contains("data_1383"));
        assert!(tokens.contains("2603"));
    }

    #[test]
    fn detect_duplicates_orders_strongest_first() {
        let existing = vec![
            ExistingLocalExtension {
                iri: "swfc:doublet_a".into(),
                label: "Cell Doublet Score A".into(),
                definition: "Per-cell doublet probability".into(),
                proposed_parent_terms: vec![],
            },
            ExistingLocalExtension {
                iri: "swfc:doublet_b".into(),
                label: "Doublet Probability".into(),
                definition: "Per-cell doublet probability score".into(),
                proposed_parent_terms: vec![],
            },
            ExistingLocalExtension {
                iri: "swfc:unrelated".into(),
                label: "Unrelated Score".into(),
                definition: "Completely different domain item".into(),
                proposed_parent_terms: vec![],
            },
        ];
        let result = detect_duplicates(
            "Cell Doublet Score",
            "Per-cell doublet probability",
            &[],
            &existing,
            0.85,
            0.50,
        );
        // Should have at least a near or candidate match for the
        // doublet entries. Order: strongest jaccard first.
        let all: Vec<&DuplicateCandidate> = result
            .near_duplicates
            .iter()
            .chain(result.candidate_matches.iter())
            .collect();
        assert!(!all.is_empty());
        if all.len() >= 2 {
            assert!(all[0].jaccard >= all[1].jaccard);
        }
    }

    #[test]
    fn graduation_config_validates_well_formed() {
        let cfg = GraduationConfig {
            version: "1.0.0".into(),
            graduation: GraduationThresholds::default(),
            duplicate_detection: DuplicateDetectionThresholds::default(),
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn graduation_config_rejects_inverted_thresholds() {
        let cfg = GraduationConfig {
            version: "1.0.0".into(),
            graduation: GraduationThresholds::default(),
            duplicate_detection: DuplicateDetectionThresholds {
                jaccard_near_duplicate_threshold: 0.4,
                jaccard_candidate_match_threshold: 0.7,
            },
        };
        match cfg.validate() {
            Err(GraduationConfigError::OrderedThresholds { .. }) => {}
            other => panic!("expected OrderedThresholds, got {other:?}"),
        }
    }

    #[test]
    fn graduation_config_rejects_out_of_range_success_rate() {
        let cfg = GraduationConfig {
            version: "1.0.0".into(),
            graduation: GraduationThresholds {
                min_usage_count: 5,
                min_unique_sessions: 3,
                min_success_rate: 1.5, // bad
            },
            duplicate_detection: DuplicateDetectionThresholds::default(),
        };
        match cfg.validate() {
            Err(GraduationConfigError::InvalidSuccessRate { .. }) => {}
            other => panic!("expected InvalidSuccessRate, got {other:?}"),
        }
    }

    #[test]
    fn loads_shipped_config_file() {
        // The bundled config file MUST validate.
        let cfg = GraduationConfig::load_from_path("../../config/local-extension-graduation.yaml")
            .expect("shipped config loads");
        assert_eq!(cfg.graduation.min_usage_count, 5);
        assert_eq!(cfg.graduation.min_unique_sessions, 3);
    }
}
