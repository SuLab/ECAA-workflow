//! RP-8 / §G-C1 — the source-owned reporting-correctness validator folds
//! into the deposit-readiness domain rollup.
//!
//! The deposited `611cf5ee` package shipped a report that stated `10,085`
//! gene sets tested (the loaded count) when only `5,056` were actually
//! tested, described a below-background effect-abundance ratio as "above",
//! and captioned a single-column log2FC heatmap as an eight-sample matrix —
//! all of which passed the run's own agent-authored (per-run, not source)
//! validators (RP-8). This test drives the source-owned
//! [`reporting_invariants`] checklist through the deposit-readiness rollup
//! ([`deposit_readiness::scan_domain_validation`] +
//! [`deposit_readiness::write_deposit_readiness`]) and asserts a REQUIRED
//! reporting-correctness failure flips `deposit_ready` false and the
//! Layer-3 gate refuses the package — while a warn-only prose finding does
//! not block.
//!
//! It recomputes from the package's own runtime outputs, so it does not
//! depend on (and never edits) any per-run `runtime/outputs/**` script.

use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::deposit_readiness::{self, CheckStatus, ReexecStatus, Tier1Validation};
use ecaa_workflow_core::replay::report::ReverifyResult;
use std::path::Path;
use tempfile::TempDir;

/// A structurally-sound (`ro_crate`/`bagit` both `Pass`) Tier-1 result —
/// the "computationally completed" baseline the RCA observed.
fn tier1_pass() -> Tier1Validation {
    Tier1Validation {
        ro_crate: CheckStatus::Pass,
        bagit: CheckStatus::Pass,
        reverify: ReverifyResult {
            checks: Vec::new(),
            reader_matches_writer: true,
        },
        detail: None,
    }
}

fn write(outputs: &Path, rel: &str, body: &str) {
    let path = outputs.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn pathway_results_tsv(collections: &[(&str, usize)]) -> String {
    let mut s = String::from("collection\tpathway\tpval\tpadj\tES\tNES\tsize\tleadingEdge\n");
    for (coll, n) in collections {
        for i in 0..*n {
            s.push_str(&format!(
                "{coll}\t{coll}_SET_{i}\t0.01\t0.02\t0.5\t1.5\t100\tA|B\n"
            ));
        }
    }
    s
}

fn outputs_of(tmp: &TempDir) -> std::path::PathBuf {
    let outputs = tmp.path().join("runtime").join("outputs");
    std::fs::create_dir_all(&outputs).unwrap();
    outputs
}

/// Seed the RP-2 defect: `pathway_results.tsv` has 5 tested rows, but
/// `pathway_summary.json` reports the loaded count (10085 total).
fn seed_rp2_defect(outputs: &Path) {
    write(
        outputs,
        "pathway_enrichment/pathway_results.tsv",
        &pathway_results_tsv(&[("HALLMARK", 2), ("GO_BP", 3)]),
    );
    write(
        outputs,
        "pathway_enrichment/pathway_summary.json",
        &serde_json::json!({
            "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 7538, "total": 10085 }
        })
        .to_string(),
    );
}

/// A correctly-reported pathway summary (tested == recomputed rowcount).
fn seed_clean_pathways(outputs: &Path) {
    write(
        outputs,
        "pathway_enrichment/pathway_results.tsv",
        &pathway_results_tsv(&[("HALLMARK", 2), ("GO_BP", 3)]),
    );
    write(
        outputs,
        "pathway_enrichment/pathway_summary.json",
        &serde_json::json!({
            "gene_sets_tested": { "HALLMARK": 2, "GO_BP": 3, "total": 5 },
            "collections": ["HALLMARK", "GO_BP"]
        })
        .to_string(),
    );
    write(
        outputs,
        "pathway_enrichment/result.json",
        &serde_json::json!({
            "gene_sets_collections": ["HALLMARK", "GO_BP"]
        })
        .to_string(),
    );
}

/// RP-2 through `scan_domain_validation`: the recomputed rowcount mismatch
/// must roll up under the synthetic `reporting_invariants` task and fail
/// the domain-validation summary.
#[test]
fn rp2_loaded_count_folds_into_domain_rollup() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    seed_rp2_defect(&outputs);

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        !summary.passed(),
        "an RP-2 recomputed-count mismatch must fail the domain rollup: {summary:?}"
    );
    assert!(
        summary
            .failed_tasks
            .iter()
            .any(|t| t == "reporting_invariants"),
        "the reporting validator must fold in under its synthetic task id: {summary:?}"
    );
    assert!(
        summary
            .required_failures
            .iter()
            .any(|f| f.contains("reporting_invariants") && f.contains("RP-2")),
        "the rolled-up failure must name RP-2: {:?}",
        summary.required_failures
    );
}

/// End-to-end through the readiness attestation writer: an RP-2 REQUIRED
/// failure must flip `deposit_ready` false + `domain_validation` Fail, and
/// the Layer-3 gate must refuse the package even without `--strict`.
#[test]
fn rp2_defect_flips_deposit_not_ready_and_gate_refuses() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    seed_rp2_defect(&outputs);

    deposit_readiness::write_deposit_readiness(
        tmp.path(),
        "full",
        &tier1_pass(),
        ReexecStatus::Partial,
        None,
        None,
        &WallClock,
    )
    .expect("writing attestation");

    let dr = deposit_readiness::read_deposit_readiness(tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        dr.ro_crate,
        CheckStatus::Pass,
        "package is structurally sound"
    );
    assert_eq!(dr.bagit, CheckStatus::Pass);
    assert_eq!(
        dr.domain_validation,
        CheckStatus::Fail,
        "the recomputed RP-2 mismatch must surface as a domain-validation failure: {dr:?}"
    );
    assert!(
        !dr.deposit_ready,
        "a REQUIRED reporting-correctness failure must block deposit even though the \
         run is computationally complete: {dr:?}"
    );

    let err = deposit_readiness::check_deposit_readiness(tmp.path(), false)
        .expect_err("Layer-3 gate must refuse a package with a REQUIRED reporting failure");
    assert!(
        format!("{err:#}").contains("domain-correctness"),
        "gate error must name the domain-validation failure: {err:#}"
    );
}

/// A correct report must read fully deposit-ready (no false positive), and
/// the reporting validator that ran-and-passed must appear in the domain
/// summary's `checked_tasks`.
#[test]
fn clean_report_stays_deposit_ready() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    seed_clean_pathways(&outputs);
    write(
        &outputs,
        "final_reporting/final_report.md",
        "DESeq2 negative binomial GLM (`~ cell + dex`). 5 gene sets tested.\n",
    );

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        summary.passed(),
        "a clean report must pass the rollup: {summary:?}"
    );
    assert!(
        summary
            .checked_tasks
            .iter()
            .any(|t| t == "reporting_invariants"),
        "a validator that ran and passed must be recorded as checked: {summary:?}"
    );

    deposit_readiness::write_deposit_readiness(
        tmp.path(),
        "full",
        &tier1_pass(),
        ReexecStatus::Partial,
        None,
        None,
        &WallClock,
    )
    .expect("writing attestation");
    let dr = deposit_readiness::read_deposit_readiness(tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(dr.domain_validation, CheckStatus::Pass);
    assert!(
        dr.deposit_ready,
        "a clean report must read deposit-ready: {dr:?}"
    );
    assert!(deposit_readiness::check_deposit_readiness(tmp.path(), false).is_ok());
}

/// Over-block guard through the rollup: a scientifically-correct package
/// whose gene counts are serialized as JSON floats / numeric strings and
/// whose collection labels differ only in case/separator from the TSV must
/// NOT false-block the domain rollup (hardened RP-2/RP-4 tolerances), while
/// the real defects are still caught by the dedicated fold tests above.
#[test]
fn float_counts_and_label_format_do_not_false_block_rollup() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    // TSV collection labels lower-case; summary keys upper-case + hyphen.
    write(
        &outputs,
        "pathway_enrichment/pathway_results.tsv",
        &pathway_results_tsv(&[("hallmark", 2), ("go_bp", 3)]),
    );
    write(
        &outputs,
        "pathway_enrichment/pathway_summary.json",
        &serde_json::json!({
            "gene_sets_tested": { "HALLMARK": 2, "GO-BP": 3, "total": 5 },
            "collections": ["HALLMARK", "GO-BP"]
        })
        .to_string(),
    );
    // Mapping counts serialized as a JSON float (17190.0) and a numeric
    // string ("5160") — both must count as sourced.
    write(
        &outputs,
        "pathway_enrichment/result.json",
        &serde_json::json!({
            "n_genes_ranked": 17190.0,
            "n_genes_unmapped": "5160",
            "gene_sets_collections": ["HALLMARK", "GO_BP"]
        })
        .to_string(),
    );
    write(
        &outputs,
        "final_reporting/final_report.md",
        "17,190 successfully mapped; 5,160 unmapped. 5 gene sets tested.\n",
    );

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        summary.passed(),
        "float/string counts + a pure label-format difference must not false-block \
         the domain rollup: {summary:?}"
    );
    assert!(
        summary
            .checked_tasks
            .iter()
            .any(|t| t == "reporting_invariants"),
        "the reporting validator must still have run: {summary:?}"
    );
}

/// RP-5 (Required): a caption asserting an 8-sample shape for the
/// single-column log2FC `top_features_heatmap` must block deposit.
#[test]
fn rp5_caption_shape_mismatch_folds_into_rollup() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    write(
        &outputs,
        "differential_expression/figures/top_features_heatmap.png",
        "PNG",
    );
    write(
        &outputs,
        "differential_expression/result.json",
        &serde_json::json!({ "contrast": "dex_trt_vs_untrt" }).to_string(),
    );
    write(
        &outputs,
        "final_reporting/final_report.md",
        "- **top_features_heatmap**: expression heatmap of top DE genes across 8 samples.\n",
    );

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        !summary.passed(),
        "an RP-5 caption/shape mismatch must fail the rollup: {summary:?}"
    );
    assert!(
        summary.required_failures.iter().any(|f| f.contains("RP-5")),
        "the rolled-up failure must name RP-5: {:?}",
        summary.required_failures
    );
}

/// RC-COUNT (Required, §comprehensive-reporting): a `report-data.json`
/// headline count that disagrees with the value recomputed directly from
/// the declared source artifact (via the `assemble_report_data` task's
/// `report_schemas`) must fold into the domain rollup under the synthetic
/// `reporting_invariants` task and flip `deposit_ready` false — the
/// enforcement layer that would have caught the himes 3,993-vs-4,017
/// count drift. Confirms `scan_domain_validation` folds the RC-* Required
/// findings with no invariant-id filter (no `deposit_readiness` change).
#[test]
fn rc_count_source_mismatch_folds_into_rollup_and_blocks_deposit() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);

    // Source of truth: 4 rows are significant (padj < 0.05); 1 is not.
    write(
        &outputs,
        "differential_expression/de_results.tsv",
        "gene\tlog2FoldChange\tpadj\n\
         A\t2.0\t0.001\n\
         B\t-3.0\t0.002\n\
         C\t1.0\t0.01\n\
         D\t-1.0\t0.04\n\
         E\t0.1\t0.9\n",
    );
    // report-data.json UNDERSTATES the count (2, not the true 4). No
    // direction_split, so RC-IDENTITY is inapplicable and RC-COUNT is
    // isolated as the sole required failure.
    write(
        &outputs,
        "reporting/report-data.json",
        &serde_json::json!({
            "artifacts": [{
                "stage_id": "differential_expression",
                "artifact": "de_results.tsv",
                "n_total": 5,
                "n_significant": 2,
                "direction_split": null,
                "effect_distribution": null,
                "significant_entities": [],
                "significant_table_path": "runtime/outputs/differential_expression/de_results.significant.tsv",
                "full_table_path": "runtime/outputs/differential_expression/de_results.full.tsv",
                "spilled_to_attachment_only": false
            }],
            "literature": null
        })
        .to_string(),
    );
    // WORKFLOW.json carries the schema the assembler declared, exactly as
    // the emitter stamps it onto the assemble_report_data task spec.
    std::fs::write(
        tmp.path().join("WORKFLOW.json"),
        serde_json::json!({
            "tasks": {
                "assemble_report_data": {
                    "spec": {
                        "report_schemas": {
                            "differential_expression": {
                                "artifact": "de_results.tsv",
                                "entity_column": "gene",
                                "significance": { "column": "padj", "threshold": 0.05, "comparator": "lt" },
                                "signed_effect_column": "log2FoldChange"
                            }
                        }
                    }
                }
            }
        })
        .to_string(),
    )
    .unwrap();

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        !summary.passed(),
        "an RC-COUNT source mismatch must fail the domain rollup: {summary:?}"
    );
    assert!(
        summary
            .required_failures
            .iter()
            .any(|f| f.contains("reporting_invariants") && f.contains("RC-COUNT")),
        "the rolled-up required failure must name RC-COUNT (proves the recompute ran, \
         not some unrelated check): {:?}",
        summary.required_failures
    );

    deposit_readiness::write_deposit_readiness(
        tmp.path(),
        "full",
        &tier1_pass(),
        ReexecStatus::Partial,
        None,
        None,
        &WallClock,
    )
    .expect("writing attestation");
    let dr = deposit_readiness::read_deposit_readiness(tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        dr.domain_validation,
        CheckStatus::Fail,
        "RC-COUNT mismatch must surface as a domain-validation failure: {dr:?}"
    );
    assert!(
        !dr.deposit_ready,
        "a REQUIRED RC-COUNT failure must block deposit: {dr:?}"
    );
    assert!(
        deposit_readiness::check_deposit_readiness(tmp.path(), false).is_err(),
        "the Layer-3 gate must refuse a package whose report count disagrees with source"
    );
}

/// A warn-only prose finding (RP-9 "linear mixed model" label) must NOT
/// block deposit: the domain rollup still passes and the warning is
/// surfaced separately.
#[test]
fn warn_only_method_label_does_not_block() {
    let tmp = TempDir::new().unwrap();
    let outputs = outputs_of(&tmp);
    write(
        &outputs,
        "final_reporting/final_report.md",
        "All results were produced under a linear mixed model (`~ cell + dex`).\n",
    );

    let summary = deposit_readiness::scan_domain_validation(tmp.path());
    assert!(
        summary.passed(),
        "a warn-only RP-9 finding must NOT block the rollup: {summary:?}"
    );
    assert!(
        summary
            .reporting_warnings
            .iter()
            .any(|w| w.contains("RP-9")),
        "the warning must still be surfaced: {:?}",
        summary.reporting_warnings
    );

    deposit_readiness::write_deposit_readiness(
        tmp.path(),
        "full",
        &tier1_pass(),
        ReexecStatus::Partial,
        None,
        None,
        &WallClock,
    )
    .expect("writing attestation");
    let dr = deposit_readiness::read_deposit_readiness(tmp.path())
        .unwrap()
        .unwrap();
    assert_eq!(
        dr.domain_validation,
        CheckStatus::Pass,
        "warn-only must not fail domain"
    );
    assert!(
        dr.deposit_ready,
        "warn-only must stay deposit-ready: {dr:?}"
    );
    let detail = dr.detail.unwrap_or_default();
    assert!(
        detail.contains("reporting-correctness warning") && detail.contains("RP-9"),
        "the attestation detail must record the advisory warning: {detail}"
    );
}
