//! Per-modality semantic-equivalence bounds.
//!
//! Loads the `semantic_equivalence_bounds` block from a modality manifest YAML
//! and exposes a `check_bound` helper that evaluates whether an observed metric
//! value satisfies a bound.  The claim verifier and rubric scorer call
//! `load_bounds_for_modality` to retrieve the bounds declared for the modality
//! associated with the current session, then drive `check_bound` over each
//! metric read from result artifacts.

use serde::Deserialize;
use std::path::Path;

/// One numeric tolerance entry from `semantic_equivalence_bounds` in a
/// modality manifest YAML.
#[derive(Debug, Clone, PartialEq, Deserialize, schemars::JsonSchema)]
pub struct SemanticEquivalenceBound {
    /// Metric identifier, e.g. `log2fc_abs_delta`, `ari`, `jaccard`,
    /// `vcf_concordance`, `spearman_rho`.
    pub metric: String,

    /// Comparison operator applied as `observed <operator> threshold`.
    pub operator: BoundOperator,

    /// Numeric threshold value.
    pub threshold: f64,

    /// Optional glob-style artifact pattern relative to the package root
    /// (e.g. `"results/tables/de_*.csv"`).  When absent the bound applies to
    /// any artifact that exposes the metric.
    #[serde(default)]
    pub applies_to: Option<String>,

    /// Human-readable rationale referencing the grant section or recreation
    /// plan that justifies the specific threshold.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// Comparison direction for a `SemanticEquivalenceBound`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
pub enum BoundOperator {
    /// `observed <= threshold`
    #[serde(rename = "<=")]
    Lte,
    /// `observed >= threshold`
    #[serde(rename = ">=")]
    Gte,
    /// `observed < threshold`
    #[serde(rename = "<")]
    Lt,
    /// `observed > threshold`
    #[serde(rename = ">")]
    Gt,
    /// `|observed - threshold| / threshold <= 0.01` (within 1%).
    #[serde(rename = "approx")]
    Approx,
}

/// Returns `true` when `observed` satisfies `bound`.
///
/// For `BoundOperator::Approx` the check is `|observed - threshold| /
/// threshold.abs() <= 0.01`, i.e. within 1 % of the declared threshold.
/// When `threshold` is zero the check falls back to `observed == 0.0`.
pub fn check_bound(bound: &SemanticEquivalenceBound, observed: f64) -> bool {
    match bound.operator {
        BoundOperator::Lte => observed <= bound.threshold,
        BoundOperator::Gte => observed >= bound.threshold,
        BoundOperator::Lt => observed < bound.threshold,
        BoundOperator::Gt => observed > bound.threshold,
        BoundOperator::Approx => {
            let denom = bound.threshold.abs();
            if denom == 0.0 {
                observed == 0.0
            } else {
                ((observed - bound.threshold) / denom).abs() <= 0.01
            }
        }
    }
}

/// Thin serde wrapper that mirrors only the fields needed to extract bounds
/// from a full modality manifest YAML.
#[derive(Deserialize)]
struct ModalityManifestBoundsSlice {
    #[serde(default)]
    semantic_equivalence_bounds: Vec<SemanticEquivalenceBound>,
}

/// Loads the `semantic_equivalence_bounds` list for `modality_id` from the
/// YAML file at `<modalities_dir>/<modality_id>.yaml`.
///
/// Returns an empty `Vec` (not an error) when the manifest exists but declares
/// no `semantic_equivalence_bounds` key, matching the schema's `optional`
/// semantics.
pub fn load_bounds_for_modality(
    modality_id: &str,
    modalities_dir: &Path,
) -> std::io::Result<Vec<SemanticEquivalenceBound>> {
    let manifest_path = modalities_dir.join(format!("{modality_id}.yaml"));
    let contents = std::fs::read_to_string(&manifest_path)?;
    let slice: ModalityManifestBoundsSlice = serde_yml::from_str(&contents).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "failed to parse semantic_equivalence_bounds from {}: {e}",
                manifest_path.display()
            ),
        )
    })?;
    Ok(slice.semantic_equivalence_bounds)
}
