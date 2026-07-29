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
        literature_evidence: None,
        matched_pvalue_keyword: None,
        linear_fold: None,
        aggregate_kind: None,
        aggregate_column: None,
        aggregate_rowset: None,
        aggregate_value: None,
        collection: None,
        term: None,
        keyed_column: None,
        keyed_value: None,
    };
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_pending: 0,
        n_suspicious: 0,
        verdicts: vec![ClaimVerdict {
            claim,
            status: ClaimStatus::Verified,
            strength: ClaimStrength::default(),
            audit: None,
        }],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(root, task, &rep, None, w).unwrap();
}

/// Build a package whose `@graph` registers a figure but writes the produced
/// result table into a SUBDIR of the task output dir
/// (`runtime/outputs/<task>/tables/<file>`) — the shape that the direct-children
/// scanner misses. Mirrors an agent that nests its result tables one level down.
fn write_package_with_subdir_table(root: &Path, table_basename: &str, task: &str) {
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

    // The produced result table lives one directory deeper than the scanner's
    // historical direct-children level.
    let tables_dir = root.join(format!("runtime/outputs/{task}/tables"));
    fs::create_dir_all(&tables_dir).unwrap();
    fs::write(
        tables_dir.join(table_basename),
        "gene\tlog2FC\tpadj\nTGFB1\t2.1\t0.001\n",
    )
    .unwrap();
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
    let registered = ecaa_workflow_core::ro_crate::register_produced_output_tables(root).unwrap();
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

    // The post-execution reseal (SealMode::Reseal) extends the payload manifest
    // to cover runtime/outputs/, so the produced result table is now hashed in
    // the at-rest integrity surface (in addition to being a V `@graph` entity).
    // Verify its manifest row matches the table's live bytes.
    let table_rel = format!("runtime/outputs/{task}/{table}");
    let table_bytes = fs::read(root.join(&table_rel)).unwrap();
    let mut th = <sha2::Sha512 as sha2::Digest>::new();
    sha2::Digest::update(&mut th, &table_bytes);
    let table_hex = hex::encode(sha2::Digest::finalize(th));
    assert!(
        manifest_after
            .lines()
            .any(|l| l.starts_with(&table_hex) && l.ends_with(&format!("  {table_rel}"))),
        "re-sealed manifest must include the produced output table with a row matching its \
         live bytes; table_hex={table_hex}\nmanifest=\n{manifest_after}"
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
    write_production_shaped_package(
        root,
        "differential_expression.tsv",
        "differential_expression",
    );
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

/// (1) Atomic descriptor write: registering tables round-trips through the
/// atomic writer (write-tmp -> fsync -> rename). The regression we guard is
/// that the descriptor stays parseable JSON with the new entity present and
/// no `.tmp` siblings linger after the crash-safe rename.
#[test]
fn register_tables_round_trips_via_atomic_write() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    write_production_shaped_package(root, "differential_expression.tsv", task);

    let added = ecaa_workflow_core::ro_crate::register_produced_output_tables(root).unwrap();
    assert_eq!(added, 1, "the produced table is registered");

    // The atomic writer renames a `.<uuid>.tmp` into place: no temp sibling of
    // the descriptor may survive.
    let lingering: Vec<_> = fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("ro-crate-metadata.json.") && n.ends_with(".tmp"))
        .collect();
    assert!(
        lingering.is_empty(),
        "atomic write must leave no .tmp sibling; found {lingering:?}"
    );

    // The descriptor still round-trips to JSON with the table entity present.
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("ro-crate-metadata.json")).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().unwrap();
    assert!(
        graph.iter().any(|e| e["@id"]
            == json!("runtime/outputs/differential_expression/differential_expression.tsv")),
        "the registered table entity must round-trip in @graph"
    );
}

/// (2) Subdirectory-robust registration + matching: a verified, table-backed
/// claim whose table the agent wrote in a SUBDIR of the task output dir must
/// still resolve in `cross_graph_integrity` (Inv 5) after finalize. The runtime
/// verifier records `source_table` by basename, so the C->V `supported_by`
/// reference must resolve against the table registered at its real (nested)
/// relative `@id`.
#[test]
fn verified_claim_with_subdir_table_resolves_after_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";
    write_package_with_subdir_table(root, table, task);

    let clock = ecaa_workflow_core::clock::WallClock;
    let added = ecaa_workflow_core::ro_crate::finalize_evidence_registration(root, &clock).unwrap();
    assert_eq!(
        added, 1,
        "the table nested under runtime/outputs/<task>/tables/ must be registered"
    );

    // The table must be registered at its REAL nested relative @id.
    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("ro-crate-metadata.json")).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().unwrap();
    assert!(
        graph.iter().any(|e| e["@id"]
            == json!("runtime/outputs/differential_expression/tables/differential_expression.tsv")),
        "the nested table must be registered at its real relative @id; @graph=\n{graph:#?}"
    );
    // The figures/ subdir must NOT be walked as a table (it is an ImageObject).
    assert!(
        !graph.iter().any(|e| e["@id"].as_str().is_some_and(|s| s
            .starts_with("runtime/outputs/differential_expression/figures/")
            && (s.ends_with(".tsv") || s.ends_with(".csv")))),
        "figures/ must stay excluded from table registration"
    );

    let w = AuditWriter::for_session();
    persist_verified_table_claim(root, task, table, &w);
    let (status, detail) = cross_graph_status(root, &w);
    assert_eq!(
        status,
        InvariantStatus::Pass,
        "a verified claim whose table lives in a subdir must resolve C->V in Inv 5; \
         got {status:?} (detail: {detail:?})"
    );
}

/// (3) C-subgraph back-fill: after finalize on a package with a verified,
/// table-backed claim in the SIGNED sink, `ro-crate-metadata.json`'s @graph must
/// carry a Claim (C) node AND a `supported_by` edge to the registered V table
/// entity — projected from the signed sink, not the empty plaintext stub.
#[test]
fn finalize_backfills_claim_nodes_into_graph_from_signed_sink() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";
    write_production_shaped_package(root, table, task);

    // The signed sink carries the verified verdict (the plaintext
    // claim-verification.json is never written, mirroring production).
    let w = AuditWriter::for_session();
    persist_verified_table_claim(root, task, table, &w);

    // Production passes the session writer so the back-fill can HMAC-verify and
    // project the signed-sink verdicts into the @graph.
    let clock = ecaa_workflow_core::clock::WallClock;
    ecaa_workflow_core::ro_crate::finalize_evidence_registration_with_verifier(
        root,
        &clock,
        Some(&w),
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("ro-crate-metadata.json")).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().unwrap();

    // A first-class Claim node was projected from the signed-sink verdict.
    let claim_node = graph
        .iter()
        .find(|e| e["@type"] == json!("Claim"))
        .unwrap_or_else(|| panic!("@graph must carry a Claim node; @graph=\n{graph:#?}"));
    assert_eq!(
        claim_node["status"],
        json!("verified"),
        "the back-filled Claim node must carry the verified status"
    );

    // ... and a supported_by edge to a V evidence node.
    let has_supported_by_edge = graph.iter().any(|e| {
        e.get("supported_by").is_some() && e["@type"] == json!("Claim")
            || e["predicate"] == json!("supported_by")
    });
    assert!(
        has_supported_by_edge,
        "@graph must carry a supported_by edge from the Claim to a V table entity; \
         @graph=\n{graph:#?}"
    );
}

/// FAITHFUL TWIN (B2): after the C-subgraph back-fill, the embedded `Claim`
/// node's `supported_by` `@id` must reference the REAL registered output File
/// entity (the table's `@id`), which is itself a node in the SAME `@graph` — not
/// a synthetic `V:<basename>` handle that resolves to nothing. The Claim text is
/// no longer empty either: it carries the recorded excerpt / matched entity.
#[test]
fn backfilled_claim_supported_by_resolves_to_real_graph_node_and_text_populated() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";
    write_production_shaped_package(root, table, task);

    let w = AuditWriter::for_session();
    // A claim with a real excerpt so we can assert the node text populates.
    let claim = Claim {
        entity: "TGFB1".into(),
        direction: None,
        effect_size: None,
        pvalue: None,
        source_table: Some(table.into()),
        excerpt: "TGFB1 is strongly upregulated in the treated cohort.".into(),
        contract: ClaimContract::NumericTableLookup,
        literature_evidence: None,
        matched_pvalue_keyword: None,
        linear_fold: None,
        aggregate_kind: None,
        aggregate_column: None,
        aggregate_rowset: None,
        aggregate_value: None,
        collection: None,
        term: None,
        keyed_column: None,
        keyed_value: None,
    };
    let rep = ClaimVerificationReport {
        n_checked: 1,
        n_verified: 1,
        n_mismatch: 0,
        n_unverifiable: 0,
        n_pending: 0,
        n_suspicious: 0,
        verdicts: vec![ClaimVerdict {
            claim,
            status: ClaimStatus::Verified,
            strength: ClaimStrength::default(),
            audit: None,
        }],
        runtime_decision_log_path: None,
    };
    persist_signed_verdicts(root, task, &rep, None, &w).unwrap();

    let clock = ecaa_workflow_core::clock::WallClock;
    ecaa_workflow_core::ro_crate::finalize_evidence_registration_with_verifier(
        root,
        &clock,
        Some(&w),
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("ro-crate-metadata.json")).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().unwrap();

    let node_ids: std::collections::BTreeSet<&str> =
        graph.iter().filter_map(|e| e["@id"].as_str()).collect();

    let claim_node = graph
        .iter()
        .find(|e| e["@type"] == json!("Claim"))
        .unwrap_or_else(|| panic!("@graph must carry a Claim node; @graph=\n{graph:#?}"));

    // Text is populated from the recorded excerpt (no longer the empty default).
    assert_eq!(
        claim_node["text"],
        json!("TGFB1 is strongly upregulated in the treated cohort."),
        "embedded Claim text must carry the recorded excerpt, not the empty default"
    );

    // Every supported_by @id resolves to a REAL @graph node (the registered
    // output File), so the embedded edge is not dangling.
    let refs = claim_node["supported_by"].as_array().unwrap_or_else(|| {
        panic!("Claim node must carry a supported_by array; node={claim_node:#?}")
    });
    assert!(
        !refs.is_empty(),
        "supported_by must be non-empty for a verified claim"
    );
    for r in refs {
        let target = r["@id"]
            .as_str()
            .unwrap_or_else(|| panic!("supported_by entry must be {{@id}}; got {r}"));
        assert!(
            node_ids.contains(target),
            "supported_by @id {target} must resolve to a real @graph node; \
             node_ids={node_ids:?}"
        );
        // And it must be the REAL output File path, not a synthetic V: handle.
        assert!(
            target.starts_with("runtime/outputs/"),
            "supported_by must reference the real registered File @id, not a V: handle; got {target}"
        );
    }

    // The integrity invariant agrees: it passes when the embedded ref resolves.
    let (status, detail) = cross_graph_status(root, &w);
    assert_eq!(
        status,
        InvariantStatus::Pass,
        "resolved embedded supported_by must pass cross_graph_integrity; detail={detail:?}"
    );
}

/// FAITHFUL TWIN (B2): a DANGLING embedded `Claim.supported_by` `@id` — one that
/// names no `@graph` node — MUST FAIL `cross_graph_integrity`. This proves the
/// invariant ACTUALLY validates embedded referential integrity (the gap the
/// task closes): before the fix an embedded dangling id passed unchecked.
#[test]
fn dangling_embedded_claim_supported_by_fails_cross_graph_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let task = "differential_expression";
    let table = "differential_expression.tsv";
    write_production_shaped_package(root, table, task);

    // Register the real table so the package has at least one resolvable output.
    ecaa_workflow_core::ro_crate::register_produced_output_tables(root).unwrap();

    // Hand-inject an embedded Claim node whose supported_by names a NON-EXISTENT
    // output File — exactly the dangling-id shape the projector must never emit,
    // and which the invariant must now catch.
    let descriptor = root.join("ro-crate-metadata.json");
    let mut doc: serde_json::Value =
        serde_json::from_slice(&fs::read(&descriptor).unwrap()).unwrap();
    doc["@graph"].as_array_mut().unwrap().push(json!({
        "@id": "C:differential_expression_claim-0",
        "@type": "Claim",
        "status": "verified",
        "text": "TGFB1 upregulated",
        "supported_by": [{ "@id": "runtime/outputs/differential_expression/DOES_NOT_EXIST.tsv" }]
    }));
    fs::write(&descriptor, serde_json::to_vec_pretty(&doc).unwrap()).unwrap();

    // A signed sink must exist for cross_graph to load claims; persist a verdict
    // (its own supported_by resolves — the FAILURE comes from the embedded node).
    let w = AuditWriter::for_session();
    persist_verified_table_claim(root, task, table, &w);

    let (status, detail) = cross_graph_status(root, &w);
    assert_eq!(
        status,
        InvariantStatus::Fail,
        "a dangling embedded Claim supported_by @id must FAIL cross_graph_integrity"
    );
    let detail = detail.unwrap_or_default();
    assert!(
        detail.contains("DOES_NOT_EXIST.tsv"),
        "the failure detail must name the dangling embedded reference; got: {detail}"
    );
}
