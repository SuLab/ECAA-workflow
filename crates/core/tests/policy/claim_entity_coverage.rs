//! Coverage guards for the interpretation policy's `verifiableEntities`
//! entity-recognition surface:
//!
//! 1. `entityColumns` must carry the gene-ID aliases (`gene_id`,
//!    `feature_id`, `ensembl_id`, `ensembl_gene_id`) so a DE result table
//!    whose entity header is `gene_id` (e.g. `de_results.tsv`) actually
//!    loads for claim verification (Workstream A, Task A1).
//! 2. Report-control ALLCAPS tokens (PASS/FAIL/GATING/EVIDENCE/YES/NO/…)
//!    must NOT be extracted as gene-symbol entities, while a real gene
//!    symbol on the same line still survives (Workstream C, Task C1).

use ecaa_workflow_core::claim_extractor::{extract_claims, ExtractorConfig};
use std::fs;
use std::path::{Path, PathBuf};

fn policies_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

/// A1: the four gene-ID aliases must be present in `entityColumns` so that
/// `de_results.tsv` (header `gene_id`) is loadable by the claim verifier.
#[test]
fn entity_columns_include_gene_id_aliases() {
    let raw = fs::read_to_string(policies_dir().join("interpretation-policy.json"))
        .expect("interpretation policy readable");
    let policy: serde_json::Value = serde_json::from_str(&raw).expect("policy parses");
    let cfg = ExtractorConfig::from_policy(&policy).expect("extractor config builds");

    for col in ["gene_id", "feature_id", "ensembl_id", "ensembl_gene_id"] {
        assert!(
            cfg.entity_columns.iter().any(|c| c == col),
            "entityColumns must include `{col}` so de_results.tsv (header gene_id) loads; \
             got {:?}",
            cfg.entity_columns
        );
    }
}

/// C1: report-control ALLCAPS tokens must be excluded from entity extraction,
/// while a real gene symbol on the same line still extracts.
#[test]
fn excludes_report_control_words_not_genes() {
    let raw = fs::read_to_string(policies_dir().join("interpretation-policy.json"))
        .expect("interpretation policy readable");
    let policy: serde_json::Value = serde_json::from_str(&raw).expect("policy parses");
    let cfg = ExtractorConfig::from_policy(&policy).expect("extractor config builds");

    let claims = extract_claims(
        "PASS GATING EVIDENCE YES NO FAIL but CRISPLD2 was upregulated (Table S1).",
        &cfg,
    );
    let entities: Vec<&str> = claims.iter().map(|c| c.entity.as_str()).collect();

    for noise in ["PASS", "GATING", "EVIDENCE", "YES", "NO", "FAIL"] {
        assert!(
            !entities.contains(&noise),
            "report-control word `{noise}` must be excluded from extraction (got {:?})",
            entities
        );
    }
    assert!(
        entities.contains(&"CRISPLD2"),
        "real gene CRISPLD2 must still extract (got {:?})",
        entities
    );
}
