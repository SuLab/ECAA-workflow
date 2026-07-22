//! Regression: a "top down-regulated gene (most negative log2FC …, padj ≈ X)"
//! extreme claim must Verify against the true argmin of the EFFECT column — the
//! incidental padj annotation must not flip the extreme onto the p-value column
//! (himes rerun 2026-07-22 false-Mismatch). The symmetric up claim must Verify
//! over the effect argmax, not by coincidence of also being the smallest-padj.

use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};
use ecaa_workflow_core::claim_verifier::{verify_claims, ClaimStatus};

#[test]
fn extreme_effect_claims_verify_despite_padj_annotation() {
    let policy: serde_json::Value = serde_json::from_slice(
        &std::fs::read("../../config/downstream-policy/interpretation-policy.json").unwrap(),
    )
    .unwrap();
    let cfg = ExtractorConfig::from_policy(&policy).expect("cfg from policy");

    let tmp = tempfile::tempdir().unwrap();
    // Real header; the up gene is BOTH the effect-argmax AND the smallest padj,
    // while the down gene is the effect-argmin but NOT the smallest padj — so a
    // p-value-column extreme would (wrongly) reject the down claim.
    std::fs::write(
        tmp.path().join("de_results.tsv"),
        "gene_id\tbaseMean\tlog2FoldChange\tlfcSE\tstat\tpvalue\tpadj\n\
         ENSG00000152583\t997.4\t4.07588138957873\t0.16\t25.0\t1e-140\t7.05595989741071e-132\n\
         ENSG00000162692\t508.2\t-3.24867714937948\t0.17\t-19.4\t4.5e-84\t5.12235659032677e-81\n\
         ENSG00000101347\t8000.0\t3.5\t0.2\t17.0\t1e-70\t1e-67\n\
         ENSG00000120129\t3000.0\t2.9\t0.2\t14.0\t1e-60\t1e-57\n",
    )
    .unwrap();

    let up = "- **Top up-regulated gene** (largest positive shrunken log2FC among significant genes): ENSG00000152583 (shrunken log2FC \u{2248} 4.08, padj \u{2248} 7.1e-132)";
    let down = "- **Top down-regulated gene** (most negative shrunken log2FC among significant genes): ENSG00000162692 (shrunken log2FC \u{2248} \u{2212}3.25, padj \u{2248} 5.1e-81)";

    for (label, line, entity) in [
        ("UP", up, "ENSG00000152583"),
        ("DOWN", down, "ENSG00000162692"),
    ] {
        let mut claims = extract_claims(line, &cfg);
        assert!(!claims.is_empty(), "{label}: no claim extracted");
        // In the runtime the source table is resolved from package context; set
        // it here so the extreme verifier runs its argmin/argmax path.
        for c in &mut claims {
            c.source_table = Some("de_results.tsv".to_string());
        }
        let report = verify_claims(&claims, tmp.path(), &cfg);
        let v = report
            .verdicts
            .iter()
            .find(|v| v.claim.entity == entity)
            .unwrap_or_else(|| panic!("{label}: no verdict for {entity}"));
        assert!(
            matches!(v.status, ClaimStatus::Verified),
            "{label} claim on {entity} must Verify (it is the true effect-column extreme), got {:?}",
            v.status
        );
    }
}
