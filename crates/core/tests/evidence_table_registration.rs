//! Regression: a *verified, table-backed* claim must resolve in
//! `cross_graph_integrity` (Inv 5), and the post-execution evidence-registration
//! finalize must leave the package's BagIt manifest self-consistent.
//!
//! Reproduces the defect found in the executed Pasilla package
//! (`…/swfc-packages/ivd-comprehensive-20260429/69c9ca99-…`): the runtime
//! verifier confirms a claim against a result table the agent wrote under
//! `runtime/outputs/<task>/`, recording `supported_by: ["<table>.tsv"]` in the
//! signed sink — but the result table is never registered as a V/`@graph`
//! Evidence node (only `required_figures` become `ImageObject` entities), so
//! the C→V reference dangles and Inv 5 emits a blocking `Fail`.
//!
//! The fix: register produced result tables as V `Dataset`/`File` `@graph`
//! entities under `runtime/outputs/`, emit `supported_by` as the matching
//! package-relative path, and re-seal the BagIt payload manifest (the
//! descriptor is manifested, so mutating it post-emit would otherwise stale
//! `manifest-sha512.txt`).

use ecaa_workflow_core::audit_proof::run_audit_proof_with_verifier;
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_contract::ClaimContract;
use ecaa_workflow_core::claim_extractor::Claim;
use ecaa_workflow_core::claim_sink::persist_signed_verdicts;
use ecaa_workflow_core::claim_verifier::{
    ClaimStatus, ClaimStrength, ClaimVerdict, ClaimVerificationReport,
};
use ecaa_workflow_types::invariants::{InvariantId, InvariantStatus};
use serde_json::json;
use std::fs;
use std::path::Path;

/// Build a package whose `@graph` registers a figure (as production does) but
/// NOT the produced result table, and write the produced table to disk —
/// mirroring the executed Pasilla package shape.
fn write_production_shaped_package(root: &Path, table_basename: &str, task: &str) {
    let fig = format!("runtime/outputs/{task}/figures/volcano.png");
    let graph = json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {"@id": "ro-crate-metadata.json", "@type": "CreativeWork", "about": {"@id": "./"}},
            {"@id": "./", "@type": "Dataset", "hasPart": [{"@id": fig}]},
            {"@id": fig, "@type": ["File", "ImageObject"], "name": "volcano"}
        ]
    });
    fs::write(
        root.join("ro-crate-metadata.json"),
        serde_json::to_vec_pretty(&graph).unwrap(),
    )
    .unwrap();

    // The agent's produced result table actually exists on disk.
    let task_dir = root.join(format!("runtime/outputs/{task}"));
    fs::create_dir_all(&task_dir).unwrap();
    fs::write(
        task_dir.join(table_basename),
        "gene\tlog2FC\tpadj\nTGFB1\t2.1\t0.001\n",
    )
    .unwrap();
}

/// Persist a single verified, table-backed claim to the signed sink, exactly as
/// the runtime verifier records it (`source_table` by basename).
fn persist_verified_table_claim(root: &Path, task: &str, table: &str, w: &AuditWriter) {
    let claim = Claim {
        entity: "TGFB1".into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: Some(table.into()),
        excerpt: String::new(),
        contract: ClaimContract::NumericTableLookup,
    };
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        verdicts: vec![ClaimVerdict {
            claim,
            status: ClaimStatus::Verified,
            strength: ClaimStrength::default(),
        }],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(root, task, &rep, None, w).unwrap();
}

fn cross_graph_status(root: &Path, w: &AuditWriter) -> (InvariantStatus, Option<String>) {
    let validator = ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
    let clock = ecaa_workflow_core::clock::WallClock;
    let report = run_audit_proof_with_verifier(root, &validator, &clock, Some(w)).unwrap();
    let cgi = report
        .verdicts
        .iter()
        .find(|v| v.id == InvariantId::CrossGraphIntegrity)
        .unwrap();
    (cgi.status, cgi.detail.clone())
}

#[test]
fn verified_table_backed_claim_resolves_in_cross_graph_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";

    write_production_shaped_package(root, table, task);
    let w = AuditWriter::for_session();
    persist_verified_table_claim(root, task, table, &w);

    // Post-execution: register the produced result tables as V `@graph`
    // entities (in production this runs in the verify-finalize after the agent
    // has written its tables).
    let registered =
        ecaa_workflow_core::ro_crate::register_produced_output_tables(root).unwrap();
    assert_eq!(
        registered, 1,
        "the produced result table must be registered as a V Evidence entity"
    );

    let (status, detail) = cross_graph_status(root, &w);
    assert_eq!(
        status,
        InvariantStatus::Pass,
        "a verified claim's supported_by must resolve to a registered Evidence (V) node; \
         got {status:?} (detail: {detail:?})"
    );
}

#[test]
fn finalize_registers_tables_and_reseals_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";
    write_production_shaped_package(root, table, task);

    // Seal an emit-time manifest over the PRE-registration descriptor (mirrors
    // the real ordering: manifest sealed at emit, then the descriptor mutated).
    let clock = ecaa_workflow_core::clock::WallClock;
    ecaa_workflow_core::emitter::regenerate_bagit_manifest(root, &clock).unwrap();
    let manifest_before = fs::read_to_string(root.join("manifest-sha512.txt")).unwrap();

    // finalize registers the table AND re-seals the manifest.
    let added = ecaa_workflow_core::ro_crate::finalize_evidence_registration(root, &clock).unwrap();
    assert_eq!(added, 1, "finalize must register the produced result table");

    let manifest_after = fs::read_to_string(root.join("manifest-sha512.txt")).unwrap();
    assert_ne!(
        manifest_before, manifest_after,
        "manifest must be re-sealed after the descriptor is mutated"
    );

    // The re-sealed manifest row for the descriptor matches its live bytes.
    let desc_bytes = fs::read(root.join("ro-crate-metadata.json")).unwrap();
    let mut h = <sha2::Sha512 as sha2::Digest>::new();
    sha2::Digest::update(&mut h, &desc_bytes);
    let live_hex = hex::encode(sha2::Digest::finalize(h));
    assert!(
        manifest_after
            .lines()
            .any(|l| l.starts_with(&live_hex) && l.ends_with("  ro-crate-metadata.json")),
        "manifest row for ro-crate-metadata.json must match live content; \
         live_hex={live_hex}\nmanifest=\n{manifest_after}"
    );

    // The produced-table payload exclusion still holds (runtime/outputs/ is a
    // walk exclusion; the table is a V `@graph` entity, not a manifested file).
    assert!(
        !manifest_after.contains("runtime/outputs"),
        "runtime/outputs must remain excluded from the payload manifest:\n{manifest_after}"
    );

    // And the verified table-backed claim now resolves in Inv 5.
    let w = AuditWriter::for_session();
    persist_verified_table_claim(root, task, table, &w);
    let (status, detail) = cross_graph_status(root, &w);
    assert_eq!(
        status,
        InvariantStatus::Pass,
        "Inv 5 must resolve C→V after finalize; got {status:?} (detail: {detail:?})"
    );
}

#[test]
fn finalize_is_idempotent_and_manifest_stable() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write_production_shaped_package(root, "differential_expression.tsv", "differential_expression");
    let clock = ecaa_workflow_core::clock::WallClock;

    let first = ecaa_workflow_core::ro_crate::finalize_evidence_registration(root, &clock).unwrap();
    assert_eq!(first, 1, "first finalize registers the produced table");
    let m1 = fs::read_to_string(root.join("manifest-sha512.txt")).unwrap();

    let second =
        ecaa_workflow_core::ro_crate::finalize_evidence_registration(root, &clock).unwrap();
    assert_eq!(
        second, 0,
        "second finalize registers nothing new (registration is idempotent)"
    );
    let m2 = fs::read_to_string(root.join("manifest-sha512.txt")).unwrap();
    assert_eq!(
        m1, m2,
        "re-seal must be deterministic: identical payload-manifest bytes on repeat"
    );
}
