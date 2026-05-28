//! Six contract classes the claim verifier discriminates against.
//! Defined by grant PAR-26-040 §Claim verifier.

use serde::{Deserialize, Serialize};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// ClaimContract discriminant.
pub enum ClaimContract {
    /// Direct table-cell numeric lookup (e.g., "316 DEGs in Himes")
    NumericTableLookup,
    /// Thresholded differential-expression / enrichment ("FDR<0.05, log2FC>1")
    ThresholdedDeOrEnrichment,
    /// Rank or top-N membership ("TP53 in top-10 hits")
    RankTopN,
    /// Group-comparison summary ("treated > control by 2.3-fold")
    GroupComparison,
    /// Categorical-label claim ("cluster 5 identified as cardiomyocytes")
    Categorical,
    /// Time-series or clinical-trial summary ("peak at day 14, n=42 enrolled")
    TimeSeriesSummary,
}

impl ClaimContract {
    /// Serde default for the `contract` field on [`crate::claim_extractor::Claim`].
    /// Returns `NumericTableLookup` so that claims serialized before this field was
    /// introduced round-trip cleanly with the backwards-compatible baseline.
    pub fn default_numeric() -> Self {
        Self::NumericTableLookup
    }
}
