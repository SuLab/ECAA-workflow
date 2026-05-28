//! Pin the p-value tolerance behavior of `claim_verifier` across four
//! representative edge cases.
//!
//! The comparison in `verify_one` (claim_verifier.rs ~line 729) uses a
//! log-ratio window, NOT a simple relative delta:
//!
//!   |ln(claimed_p / observed_p)| > ln(1 + pvalue_relative_tolerance)
//!
//! With the default `pvalueRelativeDelta = 0.1`, the threshold is
//! `ln(1.1) ≈ 0.0953`.  This is *already* a log-scale uniform window —
//! it is equivalent to requiring the ratio claimed/observed to lie in
//! [1/1.1, 1.1].  The original critique worried about non-uniformity
//! from a simple relative tolerance; that concern does not apply to the
//! actual implementation.
//!
//! These tests document the four canonical edge cases so future changes
//! to the comparison formula can't accidentally regress them.

use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};
use ecaa_workflow_core::claim_verifier::{verify_claims, ClaimStatus};
use serde_json::json;
use tempfile::tempdir;

fn policy_json() -> serde_json::Value {
    json!({
        "verifiableEntities": {
            "enabled": true,
            "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
            "directionVocab": {
                "up": ["upregulated", "increased", "elevated"],
                "down": ["downregulated", "decreased", "reduced"]
            },
            "effectSizeColumns": ["log2FC"],
            "entityColumns": ["gene"],
            "pvalueColumns": ["padj"],
            "tolerance": {
                "log2FcAbsoluteDelta": 0.05,
                "pvalueRelativeDelta": 0.1
            }
        }
    })
}

fn write_table(dir: &std::path::Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).unwrap();
}

/// Tight edge: narrative cites p=0.001, table has p=0.0011 (exactly 10%
/// relative difference).  The log-ratio |ln(0.001/0.0011)| ≈ 0.09531 equals
/// ln(1.1) ≈ 0.09531 to double precision; Rust f64 arithmetic places ratio
/// *below* threshold so the check is WITHIN tolerance → Verified.
#[test]
fn pvalue_tight_edge_within_tolerance_is_verified() {
    let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
    let tmp = tempdir().unwrap();
    // Table has p=0.0011; narrative claims p=0.001 — 10 % relative difference.
    write_table(
        tmp.path(),
        "de_s1.tsv",
        "gene\tlog2FC\tpadj\nACAN\t2.1\t0.0011\n",
    );
    let claims = extract_claims(
        "ACAN was upregulated (log2FC=2.1, padj=0.001, Table S1).",
        &cfg,
    );
    let acan_claim = claims
        .iter()
        .find(|c| c.entity == "ACAN")
        .expect("claim extracted");
    // Confirm the pvalue slot was parsed.
    assert!(
        acan_claim.pvalue.is_some(),
        "expected pvalue to be parsed from narrative"
    );
    let report = verify_claims(&claims, tmp.path(), &cfg);
    let verdict = report
        .verdicts
        .iter()
        .find(|v| v.claim.entity == "ACAN")
        .expect("verdict present");
    assert!(
        matches!(verdict.status, ClaimStatus::Verified),
        "p=0.001 vs table p=0.0011 is at the tolerance boundary; expected Verified, got {:?}",
        verdict.status
    );
}

/// Significance-flipping edge: narrative cites p=0.040, table has p=0.060.
/// The 50% relative difference translates to |ln(0.040/0.060)| ≈ 0.405,
/// which far exceeds ln(1.1) ≈ 0.095 → Mismatch.
/// This is the critical safety property: a claim that is significant at α=0.05
/// cannot be verified against a table row that is not.
#[test]
fn pvalue_significance_flip_is_mismatch() {
    let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
    let tmp = tempdir().unwrap();
    // Table has p=0.060 (not significant at α=0.05); narrative claims p=0.040.
    write_table(
        tmp.path(),
        "de_s1.tsv",
        "gene\tlog2FC\tpadj\nACAN\t2.1\t0.060\n",
    );
    let claims = extract_claims(
        "ACAN was upregulated (log2FC=2.1, padj=0.040, Table S1).",
        &cfg,
    );
    let acan_claim = claims
        .iter()
        .find(|c| c.entity == "ACAN")
        .expect("claim extracted");
    assert!(
        acan_claim.pvalue.is_some(),
        "expected pvalue to be parsed from narrative"
    );
    let report = verify_claims(&claims, tmp.path(), &cfg);
    let verdict = report
        .verdicts
        .iter()
        .find(|v| v.claim.entity == "ACAN")
        .expect("verdict present");
    assert!(
        matches!(verdict.status, ClaimStatus::Mismatch { .. }),
        "p=0.040 vs table p=0.060 exceeds tolerance; expected Mismatch, got {:?}",
        verdict.status
    );
    if let ClaimStatus::Mismatch { detail } = &verdict.status {
        assert!(
            detail.contains("p-value"),
            "detail should mention p-value: {detail}"
        );
    }
}

/// Tiny-p exact match: narrative cites p=1e-10, table has p=1e-10.
/// log-ratio = 0 → well within tolerance → Verified.
#[test]
fn pvalue_tiny_exact_match_is_verified() {
    let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
    let tmp = tempdir().unwrap();
    write_table(
        tmp.path(),
        "de_s1.tsv",
        "gene\tlog2FC\tpadj\nACAN\t2.1\t0.0000000001\n",
    );
    // Narrative must cite padj= or p= to trigger pvalue extraction.
    let claims = extract_claims(
        "ACAN was upregulated (log2FC=2.1, padj=0.0000000001, Table S1).",
        &cfg,
    );
    let acan_claim = claims
        .iter()
        .find(|c| c.entity == "ACAN")
        .expect("claim extracted");
    assert!(
        acan_claim.pvalue.is_some(),
        "expected pvalue to be parsed from narrative"
    );
    let report = verify_claims(&claims, tmp.path(), &cfg);
    let verdict = report
        .verdicts
        .iter()
        .find(|v| v.claim.entity == "ACAN")
        .expect("verdict present");
    assert!(
        matches!(verdict.status, ClaimStatus::Verified),
        "exact p-value match at 1e-10 should be Verified, got {:?}",
        verdict.status
    );
}

/// Tiny-p slight drift: narrative cites p=1e-10, table has p=1.5e-10.
/// |ln(1e-10 / 1.5e-10)| = |ln(2/3)| ≈ 0.405 >> ln(1.1) → Mismatch.
/// Validates that the log-ratio window is tight even at very small p-values;
/// a 50% relative drift at p~1e-10 is correctly flagged.
#[test]
fn pvalue_tiny_p_50pct_drift_is_mismatch() {
    let cfg = ExtractorConfig::from_policy(&policy_json()).unwrap();
    let tmp = tempdir().unwrap();
    // Table has 1.5e-10; narrative claims 1e-10.
    write_table(
        tmp.path(),
        "de_s1.tsv",
        "gene\tlog2FC\tpadj\nACAN\t2.1\t0.00000000015\n",
    );
    let claims = extract_claims(
        "ACAN was upregulated (log2FC=2.1, padj=0.0000000001, Table S1).",
        &cfg,
    );
    let acan_claim = claims
        .iter()
        .find(|c| c.entity == "ACAN")
        .expect("claim extracted");
    assert!(
        acan_claim.pvalue.is_some(),
        "expected pvalue to be parsed from narrative"
    );
    let report = verify_claims(&claims, tmp.path(), &cfg);
    let verdict = report
        .verdicts
        .iter()
        .find(|v| v.claim.entity == "ACAN")
        .expect("verdict present");
    assert!(
        matches!(verdict.status, ClaimStatus::Mismatch { .. }),
        "50%% relative drift at tiny p should be Mismatch, got {:?}",
        verdict.status
    );
}
