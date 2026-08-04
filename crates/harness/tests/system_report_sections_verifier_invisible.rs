//! The system-owned report sections the harness injects at end-of-run must be
//! INVISIBLE to claim extraction.
//!
//! `ensure_system_report_sections` is the last content step of the end-of-run
//! convergence block, so it rewrites `report.md` / `final_report.md` AFTER the
//! claim verifier has already computed and signed the verdicts for those tasks.
//! That ordering is only harmless while the injected blocks contribute no
//! claims: the moment one does, the signed verdict sink stops describing the
//! bytes the package actually ships, and an honest offline re-verify can
//! disagree with the recorded verdict through no fault of the data.
//!
//! The complete-significant-entities block is invisible structurally —
//! `claim_extractor::strip_system_generated_blocks` blanks it, and both
//! extraction entry points call that. The data-provenance block is NOT
//! stripped; it is invisible only because every line it renders is either a
//! markdown heading, an HTML comment, a pipe-table row (the sentence scanner
//! skips those), or a table whose fixed `| Field | Value |` /
//! `| Input | Kind | Registered root | Files |` header resolves no
//! entity/effect/p-value role. Nothing in the renderer enforces that, so these
//! tests hold the renderers to it: add a claim-bearing prose line — or an
//! entity-shaped column header — to either block and this file fails instead of
//! silently making the recorded verdicts stale.

use ecaa_workflow_core::claim_extractor::{
    extract_claims, extract_markdown_table_claims, ExtractorConfig,
};
use ecaa_workflow_core::project_class::ProjectClass;
use ecaa_workflow_harness::end_of_run_finalize::{
    ensure_data_provenance_section, ensure_full_significant_tables, ensure_system_report_sections,
};
use std::path::{Path, PathBuf};

/// The repo's real downstream-policy directory, so the assertion runs against
/// the production entity patterns rather than a permissive stub.
fn config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
        .join("downstream-policy")
}

fn extractor_config() -> ExtractorConfig {
    let dir = config_dir();
    let raw = std::fs::read(dir.join("interpretation-policy.json"))
        .expect("config/downstream-policy/interpretation-policy.json must be readable");
    let policy: serde_json::Value =
        serde_json::from_slice(&raw).expect("interpretation-policy.json must parse");
    ExtractorConfig::from_policy_for_class(&policy, &dir, ProjectClass::default())
        .expect("the base interpretation policy must build an ExtractorConfig")
}

/// Every claim the verifier would extract from `text`, as stable sort keys.
/// Covers BOTH extraction entry points, because the injected blocks are
/// markdown tables and only `extract_markdown_table_claims` mines those.
fn claim_keys(text: &str, cfg: &ExtractorConfig) -> Vec<String> {
    let mut keys: Vec<String> = extract_claims(text, cfg)
        .into_iter()
        .map(|c| format!("sentence|{}|{}", c.entity, c.excerpt))
        .collect();
    keys.extend(
        extract_markdown_table_claims(text, cfg)
            .into_iter()
            .map(|c| format!("table|{}|{}", c.entity, c.excerpt)),
    );
    keys.sort();
    keys
}

/// Agent-authored narrative that DOES yield claims, so an assertion of
/// "the claim set did not change" cannot pass vacuously.
const AGENT_NARRATIVE: &str = "\
# Report

DUSP1 was upregulated with log2FC 2.41 and padj 1.2e-08 in the treated arm.

| Gene | log2FC | padj |
| --- | --- | --- |
| KLF15 | 4.12 | 3.0e-12 |
| CRISPLD2 | -1.85 | 4.4e-05 |
";

/// Write the acquisition record + `report-data.json` that make BOTH injections
/// fire, plus the two terminal reports carrying `AGENT_NARRATIVE`.
fn scaffold(root: &Path) {
    let outputs = root.join("runtime").join("outputs");
    for stage in ["data_acquisition", "reporting", "final_reporting"] {
        std::fs::create_dir_all(outputs.join(stage)).expect("stage dir");
    }

    // Acquisition provenance with every optional field populated, so the
    // rendered block is the widest one the emitter can produce.
    std::fs::write(
        outputs.join("data_acquisition/per_accession_summary.json"),
        serde_json::json!({
            "accession": "GSE52778",
            "study_title": "DUSP1 and KLF15 response in airway smooth muscle",
            "publication": {
                "journal": "PLOS ONE",
                "year": 2014,
                "doi": "10.1371/journal.pone.0099625",
                "pmid": "24926665",
                "first_author": "Himes"
            },
            "organism": "Homo sapiens",
            "source_package": "airway (Bioconductor)",
            "package_version": "1.30.0",
            "n_samples": 8
        })
        .to_string(),
    )
    .expect("per_accession_summary.json");

    std::fs::write(
        root.join("runtime").join("inputs.json"),
        serde_json::json!({
            "inputs": [{
                "input_id": "2c31f5197ef748e4",
                "label": "upload-ddbadbec",
                "kind": "uploaded_files",
                "root_path": "/tmp/uploads/ddbadbec",
                "files": ["counts.tsv", "coldata.csv"]
            }]
        })
        .to_string(),
    )
    .expect("inputs.json");

    std::fs::write(
        outputs.join("reporting/report-data.json"),
        serde_json::json!({
            "artifacts": [{
                "stage_id": "differential_expression",
                "artifact": "de_results.tsv",
                "n_total": 100,
                "n_significant": 3,
                "direction_split": null,
                "effect_distribution": null,
                "significant_entities": [
                    {"entity": "ENSG00000120129", "effect": 2.41,
                     "significance": 1.2e-08, "literature": {"status": "novel"}},
                    {"entity": "ENSG00000163884", "effect": 4.12,
                     "significance": 3.0e-12, "literature": {"status": "novel"}},
                    {"entity": "ENSG00000103196", "effect": -1.85,
                     "significance": 4.4e-05, "literature": {"status": "novel"}}
                ],
                "significant_table_path":
                    "runtime/outputs/differential_expression/de_results.significant.tsv",
                "full_table_path":
                    "runtime/outputs/differential_expression/de_results.full.tsv",
                "spilled_to_attachment_only": false
            }],
            "literature": null
        })
        .to_string(),
    )
    .expect("report-data.json");

    for rel in ["reporting/report.md", "final_reporting/final_report.md"] {
        std::fs::write(outputs.join(rel), AGENT_NARRATIVE).expect("report");
    }
}

fn read_report(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join("runtime").join("outputs").join(rel)).expect("report")
}

#[test]
fn injecting_the_system_report_sections_does_not_change_the_extracted_claim_set() {
    let cfg = extractor_config();
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    scaffold(root);

    let before = claim_keys(AGENT_NARRATIVE, &cfg);
    assert!(
        !before.is_empty(),
        "the fixture narrative must yield claims, otherwise this test is vacuous"
    );

    assert!(
        ensure_system_report_sections(root),
        "both system-owned sections must have been injected for this fixture"
    );

    for rel in ["reporting/report.md", "final_reporting/final_report.md"] {
        let text = read_report(root, rel);
        assert!(
            text.contains("Complete significant-entities tables")
                && text.contains("## Data provenance"),
            "precondition: {rel} must carry BOTH injected blocks"
        );
        assert_eq!(
            claim_keys(&text, &cfg),
            before,
            "injecting the system-owned sections changed the claim set the verifier \
             extracts from {rel}. The injections run AFTER the signed verdict sink is \
             written, so the recorded verdicts would no longer describe the shipped \
             bytes: either keep the injected block claim-free, or move the injection \
             ahead of verification."
        );
    }
}

#[test]
fn each_system_report_section_is_independently_claim_free() {
    let cfg = extractor_config();
    let before = claim_keys(AGENT_NARRATIVE, &cfg);

    // Full-significant-entities block alone.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    scaffold(tmp.path());
    assert!(
        ensure_full_significant_tables(tmp.path()),
        "the complete-table block must have been injected"
    );
    assert_eq!(
        claim_keys(&read_report(tmp.path(), "reporting/report.md"), &cfg),
        before,
        "the complete-significant-entities block must contribute no claim (it is \
         rendered FROM report-data.json, so mining it would also be circular)"
    );

    // Data-provenance block alone.
    let tmp = tempfile::TempDir::new().expect("tempdir");
    scaffold(tmp.path());
    assert!(
        ensure_data_provenance_section(tmp.path()),
        "the data-provenance block must have been injected"
    );
    assert_eq!(
        claim_keys(&read_report(tmp.path(), "reporting/report.md"), &cfg),
        before,
        "the data-provenance block must contribute no claim: it is a system-owned \
         statement of record, not an assertion the analysis makes about its data"
    );
}

#[test]
fn re_injection_is_a_byte_level_no_op_so_a_second_finalize_closes_the_window() {
    let tmp = tempfile::TempDir::new().expect("tempdir");
    let root = tmp.path();
    scaffold(root);

    assert!(
        ensure_system_report_sections(root),
        "first pass must inject"
    );
    let after_first: Vec<String> = ["reporting/report.md", "final_reporting/final_report.md"]
        .iter()
        .map(|rel| read_report(root, rel))
        .collect();

    assert!(
        !ensure_system_report_sections(root),
        "a second pass must report no modification — otherwise every re-finalize \
         would rewrite the narrative again after re-verifying it, and the recorded \
         verdicts could never catch up with the shipped bytes"
    );
    for (rel, expected) in ["reporting/report.md", "final_reporting/final_report.md"]
        .iter()
        .zip(&after_first)
    {
        assert_eq!(
            &read_report(root, rel),
            expected,
            "re-injection must be byte-identical for {rel}"
        );
    }
}
