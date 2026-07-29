use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use std::path::PathBuf;

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("audit-proof")
        .join(name)
}

#[test]
fn loader_reads_existing_fixture_package() {
    let root = fixture_root("minimal-emitted-package");
    let loaded = LoadedPackage::from_root(&root).expect("fixture should load");
    // Minimum expected: at least one decision and one claim row.
    assert!(!loaded.decisions.is_empty());
    assert!(loaded.claims.is_some());
    assert!(loaded.determinism_shim.is_some());
}

#[test]
fn loader_tolerates_missing_optional_sidecars() {
    let root = fixture_root("minimal-no-affordances");
    let loaded = LoadedPackage::from_root(&root).expect("fixture should load");
    assert!(loaded.plot_affordances.is_none());
}

/// End-to-end: the loader reads RO-Crate `@graph` output entities into
/// `output_entities`, and `evidence_coverage` ranges over them — so the
/// post-execution coverage flip (uncovered → Warn; covered → Pass) is
/// reachable through the real `run_audit_proof` path. This is the D.5.1
/// closure: the reader now sees the SAME output source the production writer
/// emits (RO-Crate ImageObject figures), not the `proofs.jsonl::computed_from`
/// field the conversation writer never emits.
#[test]
fn loader_post_execution_evidence_coverage_flip_is_reachable() {
    use ecaa_workflow_core::audit_proof::invariants::evidence_coverage::check_evidence_coverage;
    use ecaa_workflow_core::audit_proof::{output_source::analytical_outputs, InvariantStatus};

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let rt = root.join("runtime");
    std::fs::create_dir_all(&rt).unwrap();

    // A package whose RO-Crate declares one figure output (the production
    // ImageObject shape) and a bare-EdgeContract proofs row (no computed_from).
    let metadata = serde_json::json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {"@id": "ro-crate-metadata.json", "@type": "CreativeWork"},
            {"@id": "./", "@type": "Dataset"},
            {"@id": "runtime/outputs/de/figures/volcano.png",
             "@type": ["File", "ImageObject"]}
        ]
    });
    std::fs::write(
        root.join("ro-crate-metadata.json"),
        serde_json::to_string_pretty(&metadata).unwrap(),
    )
    .unwrap();
    std::fs::write(
        rt.join("proofs.jsonl"),
        "{\"from_node\":\"counts\",\"from_port\":\"out\",\"to_node\":\"de\",\"to_port\":\"in\",\"proof\":{}}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("WORKFLOW.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "tasks": {
                "de": {
                    "spec": {
                        "result_schema": {
                            "artifact": "volcano.png"
                        }
                    }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    // (1) Pre-execution / un-referenced: empty claims → the declared figure is
    //     uncovered → Warn (the output source is the RO-Crate entity).
    std::fs::write(
        rt.join("claim-verification.json"),
        serde_json::to_string(&serde_json::json!({"verdicts": []})).unwrap(),
    )
    .unwrap();
    let pkg = LoadedPackage::from_root(root).unwrap();
    assert_eq!(
        pkg.output_entities
            .iter()
            .filter(|e| e.get("@id").and_then(|v| v.as_str())
                == Some("runtime/outputs/de/figures/volcano.png"))
            .count(),
        1,
        "loader must read the figure output entity from @graph"
    );
    assert_eq!(
        analytical_outputs(&pkg.output_entities, &pkg.proofs).len(),
        1,
        "the figure is the one analytical output"
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn, "uncovered figure → Warn");
    assert_eq!(v.n_inspected, 1);

    // (2) Post-execution / referenced: a verified claim supported_by the figure
    //     → covered → Pass. The flip is reachable.
    std::fs::write(
        rt.join("claim-verification.json"),
        serde_json::to_string(&serde_json::json!({"verdicts": [
            {"claim_id": "c-1", "status": "verified",
             "supported_by": ["runtime/outputs/de/figures/volcano.png"]}
        ]}))
        .unwrap(),
    )
    .unwrap();
    let pkg = LoadedPackage::from_root(root).unwrap();
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass, "referenced figure → Pass");
    assert_eq!(v.n_violations, 0);
}

// ---------------------------------------------------------------------------
// Inv 4 (equivalence_failure) sources its five-class `RerunOutcome` rows from
// `runtime/reexecution.json` (the real production file), not from
// `verifier-decisions.jsonl`. These tests exercise the full loader → invariant
// path on a real on-disk package.
// ---------------------------------------------------------------------------

fn write_reexecution(rt: &std::path::Path, doc: &serde_json::Value) {
    std::fs::write(
        rt.join("reexecution.json"),
        serde_json::to_string_pretty(doc).unwrap(),
    )
    .unwrap();
}

#[test]
fn loader_inv4_fails_on_failed_reexecution_with_no_blocker() {
    use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;
    use ecaa_workflow_core::audit_proof::InvariantStatus;

    let tmp = tempfile::tempdir().unwrap();
    let rt = tmp.path().join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    write_reexecution(
        &rt,
        &serde_json::json!({
            "schema_version": "0.1",
            "bucket_counts": {"failed": 1},
            "per_artifact": [
                {"artifact_path": "results/tables/de.tsv", "bucket": "failed"}
            ],
        }),
    );
    let pkg = LoadedPackage::from_root(tmp.path()).unwrap();
    assert!(
        pkg.reexecution.is_some(),
        "loader must read runtime/reexecution.json"
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Fail,
        "a failed re-execution outcome with no acknowledging blocker must Fail"
    );
}

#[test]
fn loader_inv4_not_fail_when_failed_reexecution_acknowledged() {
    use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;
    use ecaa_workflow_core::audit_proof::InvariantStatus;

    let tmp = tempfile::tempdir().unwrap();
    let rt = tmp.path().join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    write_reexecution(
        &rt,
        &serde_json::json!({
            "schema_version": "0.1",
            "bucket_counts": {"failed": 1},
            "per_artifact": [
                {"artifact_path": "results/tables/de.tsv", "bucket": "failed"}
            ],
        }),
    );
    // F.Blocker acknowledging the diverged artifact (keyed on artifact_path).
    std::fs::write(
        rt.join("assumptions.jsonl"),
        "{\"kind\":\"unprovable_edge\",\"edge_id\":\"results/tables/de.tsv\"}\n",
    )
    .unwrap();
    let pkg = LoadedPackage::from_root(tmp.path()).unwrap();
    let v = check_equivalence_failure(&pkg);
    assert_ne!(
        v.status,
        InvariantStatus::Fail,
        "an acknowledged divergence must NOT Fail"
    );
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn loader_inv4_unverified_when_reexecution_absent() {
    use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;
    use ecaa_workflow_core::audit_proof::InvariantStatus;

    let tmp = tempfile::tempdir().unwrap();
    let rt = tmp.path().join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    // No reexecution.json at all.
    let pkg = LoadedPackage::from_root(tmp.path()).unwrap();
    assert!(pkg.reexecution.is_none());
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn loader_inv4_unverified_when_reexecution_present_but_empty() {
    use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;
    use ecaa_workflow_core::audit_proof::InvariantStatus;

    let tmp = tempfile::tempdir().unwrap();
    let rt = tmp.path().join("runtime");
    std::fs::create_dir_all(&rt).unwrap();
    // Present-but-empty (the first-emit shape).
    write_reexecution(
        &rt,
        &serde_json::json!({
            "schema_version": "0.1",
            "bucket_counts": {},
            "per_artifact": [],
        }),
    );
    let pkg = LoadedPackage::from_root(tmp.path()).unwrap();
    assert!(pkg.reexecution.is_some());
    let v = check_equivalence_failure(&pkg);
    assert_eq!(
        v.status,
        InvariantStatus::Unverified,
        "present-but-empty reexecution.json means no re-execution performed → Unverified"
    );
}
