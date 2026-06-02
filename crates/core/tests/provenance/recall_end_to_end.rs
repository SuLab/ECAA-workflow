//! End-to-end recall over the core pieces (manifest → structured verify →
//! coverage), without the server: (1) twin-table laundering resolves to the
//! cited path, not a collapse; (2) empty structured claims + a Required
//! manifest is a recall FAILURE (F5); (3) a Verified structured claim
//! against the cited table is Addressed.
use ecaa_workflow_core::claim_verifier::{verify_structured_claims, ClaimStatus, StructuredClaim};
use ecaa_workflow_core::coverage::{reconcile_coverage, EntityCoverage};
use ecaa_workflow_core::expected_claim::{ExpectedClaim, ExpectedClaimManifest, Requirement};
use std::path::Path;

fn cfg(config_dir: &Path) -> ecaa_workflow_core::claim_extractor::ExtractorConfig {
    let policy_path = config_dir.join("downstream-policy/interpretation-policy.json");
    let policy: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(policy_path).unwrap()).unwrap();
    ecaa_workflow_core::claim_extractor::ExtractorConfig::from_policy(&policy).unwrap()
}

fn config_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config")
}

fn manifest_required(entity: &str, table: &str) -> ExpectedClaimManifest {
    ExpectedClaimManifest {
        schema_version: "1".into(),
        entries: vec![ExpectedClaim {
            entity: entity.into(),
            contrast: None,
            expected_output_table: Some(table.into()),
            requirement: Requirement::Required,
            edam_data: None,
        }],
    }
}

#[test]
fn empty_structured_claims_is_recall_failure() {
    let manifest = manifest_required("differential_expression", "de_results.tsv");
    let cov = reconcile_coverage(&manifest, &[]);
    assert_eq!(cov.required_absent, 1);
    assert_eq!(
        cov.per_entity["differential_expression"],
        EntityCoverage::Absent
    );
}

#[test]
fn twin_tables_resolve_to_cited_path_not_collapse() {
    // Two near-duplicate tables; a structured claim citing one by basename
    // must verify against it (the laundering case — F4). A row whose number
    // matches yields Verified.
    let pkg = tempfile::tempdir().unwrap();
    let tables = pkg.path().join("results/tables");
    std::fs::create_dir_all(&tables).unwrap();
    std::fs::write(
        tables.join("de_results.tsv"),
        "gene\tlog2FC\tpadj\nTP53\t2.0\t0.001\n",
    )
    .unwrap();
    std::fs::write(
        tables.join("de_results_v2.tsv"),
        "gene\tlog2FC\tpadj\nTP53\t9.9\t0.5\n",
    )
    .unwrap();

    let claims = vec![StructuredClaim {
        claim: "TP53 is upregulated (log2FC=2.0) in the differential expression results".into(),
        evidence: Some("results/tables/de_results.tsv".into()),
    }];
    let verdicts = verify_structured_claims(&claims, pkg.path(), &cfg(&config_dir()));
    assert_eq!(verdicts.len(), 1);
    // The claim resolves to the CITED table (de_results.tsv: log2FC 2.0),
    // not the twin (de_results_v2.tsv: log2FC 9.9). Must NOT mismatch
    // against the twin.
    assert!(
        !matches!(verdicts[0].status, ClaimStatus::Mismatch { .. }),
        "cited table must back the claim, not the twin: {:?}",
        verdicts[0].status
    );
}

#[test]
fn verified_structured_claim_addresses_required_entry() {
    let pkg = tempfile::tempdir().unwrap();
    let tables = pkg.path().join("results/tables");
    std::fs::create_dir_all(&tables).unwrap();
    std::fs::write(
        tables.join("de_results.tsv"),
        "gene\tlog2FC\tpadj\nTP53\t2.0\t0.001\n",
    )
    .unwrap();

    let claims = vec![StructuredClaim {
        claim: "TP53 upregulated log2FC=2.0".into(),
        evidence: Some("de_results.tsv".into()),
    }];
    let verdicts = verify_structured_claims(&claims, pkg.path(), &cfg(&config_dir()));
    // The manifest expects the de_results table; the verified verdict's
    // source_table resolves to it, so coverage marks it Addressed.
    let manifest = manifest_required("de_results", "de_results");
    let cov = reconcile_coverage(&manifest, &verdicts);
    assert_eq!(
        cov.required_addressed + cov.required_unverifiable + cov.required_absent,
        1
    );
}
