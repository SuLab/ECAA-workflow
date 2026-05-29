//! `GoalSpec` derived from intake prose.
//!
//! The intake layer (LLM as UX shim) constructs a `GoalSpec`
//! alongside the existing `IntakeFacts`. The composer consumes the
//! goal to seed its backward-chain: "what atoms produce something
//! whose `edam_data` matches the goal's `edam_data` AND whose
//! `edam_format` is a subtype of the goal's `edam_format`?"
//!
//! LLM extraction lives at intake (`tools/intake.rs::append_intake_prose`
//! is the natural seam — the same call extracts modality + organism
//! + samples + a `goal:` field on its structured output).
//!
//! # Why this lives in `core`
//!
//! Mirrors the rule for `decision_log`: types that need ts-rs export
//! to `ui/src/types/` and downstream emitter access must be in `core`.
//! The LLM extraction logic lives in `crates/conversation`, but the
//! type (and its serde shape) is `core`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// What the SME wants the analysis to produce.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct GoalSpec {
    /// EDAM data class IRI naming the goal's primary committed
    /// shape. Set by the LLM from the SME's prose ("differential
    /// expression results" → `data:0951` / "an annotated
    /// AnnData" → `data:3917`).
    pub edam_data: String,

    /// EDAM format IRI for the committed artifact (e.g.
    /// `format:3475` for tabular text, `format:3590` for HDF5).
    /// Optional — when the SME hasn't named a format, the composer
    /// uses `find_producers` with format=None and ranks atoms whose
    /// `edam_format` is a subtype of the goal's preferred default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_format: Option<String>,

    /// Free-form modifiers the LLM read from prose. Examples:
    /// `{"granularity": "per-sample", "with_pathway_enrichment":
    /// "true", "compare_arms": "true"}`. The composer's slot-fill
    /// reads these to enable / disable optional atoms in the
    /// matched archetype.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modifiers: BTreeMap<String, String>,

    /// The original SME prose the LLM read. Carried so
    /// the composer can include it in the rationale log when an
    /// atom doesn't fit and the SME needs a "why isn't this
    /// available?" explanation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source_prose: Option<String>,

    /// LLM confidence in the extraction (0.0–1.0). Used by the
    /// classifier-tightening path (D-R3) to gate fast-path
    /// archetype matching: when confidence is below the
    /// configured threshold, the composer falls through to the
    /// SME-facing tie-surfacing card.
    #[serde(default = "default_confidence")]
    pub confidence: f32,
}

fn default_confidence() -> f32 {
    0.0
}

/// IRI shape validator. The LLM extracts EDAM IRIs as
/// strings; we accept the canonical `(operation|data|format|topic):
/// \d+` shape plus our `ecaax:<slug>` extension namespace (per ADR
/// 0004). Anything else means the LLM hallucinated. Caller drops
/// `GoalSpec` to `None` rather than retrying — the model rarely
/// recovers on a second pass and we'd burn cache + budget chasing
/// the same hallucination.
///
/// Note we do NOT verify the numeric id resolves to a real EDAM
/// term — that would require bundling the EDAM ontology, which
/// plan §3.6 non-goal #1 explicitly forbids ("Not bundling an OWL
/// reasoner"). Shape-only validation matches the curated-table
/// philosophy of `crates/core/src/edam.rs`.
pub fn is_valid_edam_iri(iri: &str) -> bool {
    // Match `operation:\d+`, `data:\d+`, `format:\d+`, `topic:\d+`,
    // OR `ecaax:<lowercase-slug-with-underscores>`.
    if let Some(rest) = iri
        .strip_prefix("operation:")
        .or_else(|| iri.strip_prefix("data:"))
        .or_else(|| iri.strip_prefix("format:"))
        .or_else(|| iri.strip_prefix("topic:"))
    {
        return !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some(rest) = iri.strip_prefix("ecaax:") {
        return !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    }
    false
}

impl GoalSpec {
    /// Post-validation check on `edam_data` +
    /// `edam_format` IRIs. Returns `true` when both pass shape
    /// validation; `false` when either fails. Caller drops the
    /// `GoalSpec` to `None` on failure rather than retrying — see
    /// `is_valid_edam_iri` for why.
    pub fn is_well_formed(&self) -> bool {
        if !is_valid_edam_iri(&self.edam_data) {
            return false;
        }
        if let Some(format) = &self.edam_format {
            if !is_valid_edam_iri(format) {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_goal_roundtrips() {
        let goal = GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3590".into()),
            modifiers: {
                let mut m = BTreeMap::new();
                m.insert("granularity".into(), "per-sample".into());
                m.insert("with_pathway_enrichment".into(), "true".into());
                m
            },
            source_prose: Some(
                "I want a clustered single-cell AnnData with marker genes per cluster.".into(),
            ),
            confidence: 0.92,
        };
        let yaml = serde_yml::to_string(&goal).unwrap();
        let back: GoalSpec = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(goal, back);
    }

    #[test]
    fn modifiers_are_btree_ordered() {
        // Determinism: BTreeMap preserves byte-identical YAML for
        // the same modifier set across runs.
        let mut m = BTreeMap::new();
        m.insert("z_axis".into(), "1".into());
        m.insert("a_axis".into(), "2".into());
        m.insert("m_axis".into(), "3".into());
        let goal = GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: None,
            modifiers: m,
            source_prose: None,
            confidence: 0.0,
        };
        let yaml = serde_yml::to_string(&goal).unwrap();
        let a_pos = yaml.find("a_axis").unwrap();
        let m_pos = yaml.find("m_axis").unwrap();
        let z_pos = yaml.find("z_axis").unwrap();
        assert!(a_pos < m_pos && m_pos < z_pos);
    }

    #[test]
    fn confidence_default_is_zero() {
        let raw = r#"edam_data: data:3917"#;
        let goal: GoalSpec = serde_yml::from_str(raw).unwrap();
        assert_eq!(goal.confidence, 0.0);
        assert!(goal.modifiers.is_empty());
    }

    #[test]
    fn skipped_fields_omit_from_yaml() {
        // When optional fields are None / empty, they should not
        // appear in the serialized YAML so determinism stays tight.
        let goal = GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: None,
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.0,
        };
        let yaml = serde_yml::to_string(&goal).unwrap();
        assert!(!yaml.contains("edam_format"));
        assert!(!yaml.contains("modifiers"));
        assert!(!yaml.contains("source_prose"));
    }

    /// Accept canonical EDAM namespaces with numeric ids.
    #[test]
    fn is_valid_edam_iri_accepts_canonical_shapes() {
        assert!(is_valid_edam_iri("data:3917"));
        assert!(is_valid_edam_iri("operation:0292"));
        assert!(is_valid_edam_iri("format:3590"));
        assert!(is_valid_edam_iri("topic:3308"));
    }

    /// Accept `ecaax:` extension namespace per ADR 0004.
    #[test]
    fn is_valid_edam_iri_accepts_ecaax_extension() {
        assert!(is_valid_edam_iri("ecaax:scrnaseq_annotation"));
        assert!(is_valid_edam_iri("ecaax:variant_calling"));
        assert!(is_valid_edam_iri("ecaax:cell_type_annotation"));
    }

    /// Reject everything else.
    #[test]
    fn is_valid_edam_iri_rejects_malformed() {
        assert!(!is_valid_edam_iri("AnnData")); // no prefix
        assert!(!is_valid_edam_iri("data:")); // empty id
        assert!(!is_valid_edam_iri("data:abc")); // non-numeric
        assert!(!is_valid_edam_iri("frmat:3590")); // typo
        assert!(!is_valid_edam_iri("data:39 17")); // whitespace
        assert!(!is_valid_edam_iri("ecaax:")); // empty slug
        assert!(!is_valid_edam_iri("ecaax:CamelCase")); // uppercase
        assert!(!is_valid_edam_iri("ecaax:has-dashes")); // dashes not allowed
        assert!(!is_valid_edam_iri(""));
    }

    /// `is_well_formed()` checks both IRIs.
    #[test]
    fn is_well_formed_validates_both_iris() {
        let mut goal = GoalSpec {
            edam_data: "data:3917".into(),
            edam_format: Some("format:3590".into()),
            modifiers: BTreeMap::new(),
            source_prose: None,
            confidence: 0.0,
        };
        assert!(goal.is_well_formed());

        // Bad data → fail.
        goal.edam_data = "AnnData".into();
        assert!(!goal.is_well_formed());

        // Restore data, break format.
        goal.edam_data = "data:3917".into();
        goal.edam_format = Some("frmat:3590".into());
        assert!(!goal.is_well_formed());

        // Format absent is fine.
        goal.edam_format = None;
        assert!(goal.is_well_formed());
    }
}
