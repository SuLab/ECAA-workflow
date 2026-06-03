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
