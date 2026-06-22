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
    /// Literature-grounded support claim ("TP53 dysregulation is concordant
    /// with prior reports (PMID 12345678)"). Verified against the
    /// `claims_evidence_matrix.csv` PMID-anchored prior-work rows rather than
    /// a numeric result table.
    LiteratureGrounded,
    /// Ordinal / superlative extreme claim WITHOUT an explicit rank digit
    /// ("the strongest enrichment", "the most-downregulated gene by log2FC",
    /// "TP53 had the lowest padj"). Distinct from [`Self::RankTopN`], which
    /// requires a numeric rank ("top-10"). Verified by confirming the named
    /// entity is the actual argmax/argmin of the cited column for the stated
    /// direction (highest/largest/strongest → argmax; lowest/least/smallest/
    /// weakest → argmin).
    ExtremeValue,
}

impl ClaimContract {
    /// Serde default for the `contract` field on [`crate::claim_extractor::Claim`].
    /// Returns `NumericTableLookup` so that claims serialized before this field was
    /// introduced round-trip cleanly with the backwards-compatible baseline.
    pub fn default_numeric() -> Self {
        Self::NumericTableLookup
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literature_grounded_serde_rename_is_snake_case() {
        let c = ClaimContract::LiteratureGrounded;
        let json = serde_json::to_string(&c).expect("serialize");
        assert_eq!(json, "\"literature_grounded\"");
        let back: ClaimContract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, ClaimContract::LiteratureGrounded);
    }

    #[test]
    fn default_is_still_numeric_table_lookup() {
        assert_eq!(
            ClaimContract::default_numeric(),
            ClaimContract::NumericTableLookup
        );
    }
}
