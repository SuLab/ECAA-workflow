//! DR-2 / DR-10 — the gene-symbol obligation verdict must reach the
//! deposit-readiness domain rollup.
//!
//! The `gene_symbol_ensembl_consistent` obligation runs against the
//! contextualize step, but the deposit-readiness rollup
//! (`deposit_readiness::scan_domain_validation`) only scans
//! `runtime/outputs/validate_*/result.json`. This test drives the harness
//! obligation over a package with a true cross-gene wrong-binding and asserts
//! that (1) the harness writes the verdict into a `validate_*` `result.json`,
//! (2) the CORE rollup picks it up as a domain-validation failure, and (3)
//! `compute_deposit_ready` therefore reads `false` — the DR-2 bridge — while a
//! benign paralog case leaves the rollup passing (DR-10).

use ecaa_workflow_core::deposit_readiness::{
    compute_deposit_ready, scan_domain_validation, CheckStatus, ReexecStatus,
};
use ecaa_workflow_harness::literature_validators::{
    gene_symbol_ensembl_consistent, GENE_SYMBOL_VALIDATE_TASK,
};
use ecaa_workflow_harness::validators::ValidatorOutcome;
use std::path::Path;
use tempfile::TempDir;

fn scaffold(root: &Path, truth: &str, claims: &str) {
    let ctx = root.join("runtime/outputs/contextualize_findings_with_literature");
    std::fs::create_dir_all(&ctx).unwrap();
    std::fs::write(ctx.join("claims_evidence_matrix.csv"), claims).unwrap();
    let pw = root.join("runtime/outputs/pathway_enrichment/intermediates");
    std::fs::create_dir_all(&pw).unwrap();
    std::fs::write(pw.join("ranked_genes.tsv"), truth).unwrap();
}

/// A REQUIRED gene-symbol failure rolls up into `domain_validation` and flips
/// `compute_deposit_ready` to false (DR-2), via the `validate_*` result.json
/// bridge that `scan_domain_validation` already reads.
#[test]
fn required_gene_symbol_failure_reaches_domain_rollup() {
    let dir = TempDir::new().unwrap();
    // CRISPLD2 (chr16) bound to ACSL5's Ensembl (chr10) — an unrelated locus.
    scaffold(
        dir.path(),
        "symbol\tgene_id\tstat\nCRISPLD2\tENSG00000103196\t16.7\n",
        "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
    );

    assert!(matches!(
        gene_symbol_ensembl_consistent(dir.path()),
        ValidatorOutcome::Failed { .. }
    ));

    // The harness must have written the verdict into a validate_* result.json.
    let verdict = dir
        .path()
        .join("runtime/outputs")
        .join(GENE_SYMBOL_VALIDATE_TASK)
        .join("result.json");
    assert!(
        verdict.exists(),
        "obligation verdict must be bridged to a validate_* result.json"
    );

    // The CORE rollup reads it as a domain-validation failure.
    let summary = scan_domain_validation(dir.path());
    assert!(
        summary
            .failed_tasks
            .iter()
            .any(|t| t == GENE_SYMBOL_VALIDATE_TASK),
        "scan_domain_validation must see the gene-symbol failure: {summary:?}"
    );
    assert!(!summary.required_failures.is_empty());
    assert!(!summary.passed());

    // …and deposit-readiness is not ready (a required domain obligation failed),
    // even though the run is structurally sound and re-execution is Partial.
    let domain = if summary.passed() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    assert!(!compute_deposit_ready(
        "full",
        CheckStatus::Pass,
        CheckStatus::Pass,
        domain,
        ReexecStatus::Partial
    ));
}

/// A benign same-family paralog (concordant direction) does NOT flip the
/// rollup — `scan_domain_validation` stays passing (DR-10).
#[test]
fn benign_paralog_leaves_domain_rollup_passing() {
    let dir = TempDir::new().unwrap();
    scaffold(
        dir.path(),
        "symbol\tgene_id\tstat\nLRRC37A\tENSG00000176681\t5.0\nLRRC37A2\tENSG00000238083\t4.7\n",
        "finding_id,gene_symbol\nDE_LRRC37A2_ENSG00000238083,LRRC37A\n",
    );

    assert!(matches!(
        gene_symbol_ensembl_consistent(dir.path()),
        ValidatorOutcome::Passed
    ));

    let summary = scan_domain_validation(dir.path());
    assert!(
        summary.passed(),
        "a benign paralog warning must not fail the domain rollup: {summary:?}"
    );
    assert!(summary.required_failures.is_empty());

    let domain = if summary.passed() {
        CheckStatus::Pass
    } else {
        CheckStatus::Fail
    };
    assert!(compute_deposit_ready(
        "full",
        CheckStatus::Pass,
        CheckStatus::Pass,
        domain,
        ReexecStatus::Partial
    ));
}

/// Unresolved-symbol rows ("NA") carry no real symbol↔Ensembl binding, so they
/// must be SKIPPED — not collapsed to the first `NA → Ensembl` in the truth map
/// and then false-flagged as cross-gene wrong-bindings. Reproduces the
/// 2026-07-23 himes deposit domain-validation failure: two DE loci whose symbol
/// org.Hs.eg.db could not resolve ("NA") each mismatched the first "NA" Ensembl.
#[test]
fn unresolved_na_symbols_are_skipped_not_false_flagged() {
    let dir = TempDir::new().unwrap();
    // Truth table + matrix both carry several genes whose symbol is "NA"
    // (unresolved); a real gene (CRISPLD2 → its correct Ensembl) is consistent.
    scaffold(
        dir.path(),
        "symbol\tgene_id\tstat\n\
         NA\tENSG00000002079\t3.0\n\
         NA\tENSG00000006114\t-2.0\n\
         NA\tENSG00000056661\t-1.0\n\
         CRISPLD2\tENSG00000103196\t16.7\n",
        "finding_id,gene_symbol\n\
         ENSG00000006114,NA\n\
         ENSG00000056661,NA\n\
         ENSG00000103196,CRISPLD2\n",
    );

    assert!(
        matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Passed
        ),
        "unresolved NA-symbol rows must be skipped, not flagged as wrong-bindings"
    );
    let summary = scan_domain_validation(dir.path());
    assert!(
        summary.passed(),
        "NA-symbol rows must not fail the domain rollup: {summary:?}"
    );
    assert!(summary.required_failures.is_empty());
}
