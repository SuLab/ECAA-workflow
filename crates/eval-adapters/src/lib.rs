//! Tier-4-1 claim-verifier fabrication-catch regression harness.
//!
//! Hand-curated narratives + reference tables exercise the deterministic
//! `claim_extractor` + `claim_verifier` pipeline; each scenario's authored
//! `expected_mismatch_count` is an adjudicated ground truth (derived from the
//! narrative-vs-table rubric, NOT from verifier output).
pub mod tier_4_1_claim_verifier_fabrications;
