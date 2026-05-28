//! Calibrated learning-to-defer disambiguation table.
//!
//! When the classifier identifies two near-rival archetypes / slot
//! values whose calibrated confidence falls within the trigger window,
//! the chat layer issues ONE SME disambiguation question via
//! `propose_quick_replies` instead of guessing. The table is loaded
//! from `config/classifier-disambiguation.yaml` and validated against
//! `config/classifier-disambiguation.schema.json`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// DisambiguationRegistry data.
pub struct DisambiguationRegistry {
    /// Schema version.
    pub schema_version: String,
    /// Pairs.
    pub pairs: Vec<DisambiguationPair>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// DisambiguationPair data.
pub struct DisambiguationPair {
    /// Id.
    pub id: String,
    /// Rivals.
    pub rivals: Vec<String>,
    /// Trigger when.
    pub trigger_when: TriggerCondition,
    /// Sme prompt.
    pub sme_prompt: String,
    /// Quick replies.
    pub quick_replies: Vec<QuickReply>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// TriggerCondition data.
pub struct TriggerCondition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Slot name.
    pub slot_name: Option<String>,
    #[serde(default)]
    /// Candidate values.
    pub candidate_values: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Classified modality.
    pub classified_modality: Option<String>,
    #[serde(default)]
    /// Prose contains any.
    pub prose_contains_any: Vec<String>,
    #[serde(default)]
    /// Tied modalities.
    pub tied_modalities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Max confidence below.
    pub max_confidence_below: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
/// QuickReply data.
pub struct QuickReply {
    /// Id.
    pub id: String,
    /// Label.
    pub label: String,
}

impl DisambiguationRegistry {
    /// Load.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading disambiguation file {}", path.display()))?;
        let reg: Self = serde_yml::from_str(&raw)
            .with_context(|| format!("parsing disambiguation file {}", path.display()))?;
        Ok(reg)
    }

    /// Find the first pair whose trigger fires for the given context.
    ///
    /// Trigger semantics:
    /// - `slot_name`: when set, the caller's slot identifier must match.
    /// - `candidate_values`: when non-empty, every listed candidate must
    ///   appear in the caller's candidate list (subset check).
    /// - `classified_modality`: when set, the chosen modality must match.
    /// - `prose_contains_any`: when non-empty, at least one token must
    ///   appear as a substring of the (lowercased) prose.
    /// - `tied_modalities`: when non-empty, every listed modality must
    ///   appear in the caller's candidate list (used for modality ties).
    /// - `max_confidence_below`: when set, the caller's confidence must
    ///   be strictly less than the threshold.
    pub fn match_pair(
        &self,
        slot_name: Option<&str>,
        candidates: &[&str],
        classified_modality: &str,
        prose: &str,
        max_confidence: f32,
    ) -> Option<&DisambiguationPair> {
        let prose_lower = prose.to_lowercase();
        self.pairs.iter().find(|p| {
            let cond = &p.trigger_when;
            if let Some(sn) = &cond.slot_name {
                if Some(sn.as_str()) != slot_name {
                    return false;
                }
            }
            if !cond.candidate_values.is_empty() {
                let cands: std::collections::HashSet<&str> = candidates.iter().copied().collect();
                if !cond
                    .candidate_values
                    .iter()
                    .all(|v| cands.contains(v.as_str()))
                {
                    return false;
                }
            }
            if let Some(cm) = &cond.classified_modality {
                if cm != classified_modality {
                    return false;
                }
            }
            if !cond.prose_contains_any.is_empty()
                && !cond
                    .prose_contains_any
                    .iter()
                    .any(|tok| prose_lower.contains(&tok.to_lowercase()))
            {
                return false;
            }
            if !cond.tied_modalities.is_empty() {
                let cands: std::collections::HashSet<&str> = candidates.iter().copied().collect();
                if !cond
                    .tied_modalities
                    .iter()
                    .all(|v| cands.contains(v.as_str()))
                {
                    return false;
                }
            }
            if let Some(thresh) = cond.max_confidence_below {
                if max_confidence >= thresh {
                    return false;
                }
            }
            true
        })
    }
}
