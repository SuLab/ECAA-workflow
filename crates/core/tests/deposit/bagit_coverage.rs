//! RCA I-7 — BagIt manifest covers evidence files.
//!
//! The deposited `611cf5ee` package had `runtime/decisions.jsonl`,
//! `runtime/proofs.jsonl`, `runtime/assumptions.jsonl`,
//! `runtime/audit-proof-report.json`, and `runtime/security-policy.json` all
//! physically present on disk with ZERO `manifest-sha512.txt` entries —
//! substantive evidence artifacts a deposit consumer needs integrity-covered,
//! silently left off the payload manifest. `DEPOSIT-READINESS.json` stays
//! intentionally excluded (it is a documented, BagIt-manifest-excluded
//! mutable meta file — see CLAUDE.md's Deposit verification section).
//!
//! Deviation from the brief's literal single-emit test: `audit-proof-
//! report.json` carries a spec-documented wall-clock `evaluated_at`
//! (`crates/core/src/emitter/ecaa.rs::write_audit_proof_report`), so
//! manifesting it at a fresh EMIT would make `manifest-sha512.txt` itself
//! wall-clock-dependent and break the compose-twice byte-reproducibility
//! gate (`emitter::tests::emit_package_whole_package_byte_reproducible`).
//! It is therefore manifested at RESEAL only (the post-execution at-rest
//! surface, which is where the RCA's `611cf5ee` archive was actually
//! inspected) — this test drives a reseal via the public
//! `regenerate_bagit_manifest` to cover that file too, matching how a real
//! run reaches the post-execution state the RCA describes.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::dag::{current_dag_schema_version, Task, TaskId, DAG};
use ecaa_workflow_core::emitter::{emit_package, regenerate_bagit_manifest, EmitConfig};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;

fn minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "bagit-coverage integration smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        intake_text: "bagit-coverage test fixture".into(),
        ..Default::default()
    }
}

fn one_task_dag() -> DAG {
    let task: Task = serde_json::from_value(serde_json::json!({
        "kind": "computation",
        "state": {"status": "pending"},
        "depends_on": [],
        "assignee": "agent",
        "description": "fetch raw count matrix",
        "spec": {"edam_operation": "operation:3223"}
    }))
    .expect("minimal task deserializes");
    let mut tasks: BTreeMap<TaskId, Task> = BTreeMap::new();
    tasks.insert("data_acquisition".to_string().into(), task);
    let mut dag = DAG {
        version: "1.0".into(),
        schema_version: current_dag_schema_version(),
        workflow_id: "test-bagit-coverage".into(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
        execution_order: Vec::new(),
    };
    dag.rebuild_reverse_deps();
    dag
}

fn policies_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

fn emit_sample_package() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let clf = minimal_classification();
    let dag = one_task_dag();
    emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir(),
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
        sme_parameter_overrides: None,
        sme_validation_bounds: None,
        edge_kinds: None,
    })
    .expect("emit_package must succeed");
    tmp
}

/// Parse `manifest-sha512.txt` into `(relative_path, sha512_hex)` pairs.
fn read_manifest(pkg: &Path) -> Vec<(String, String)> {
    let body = std::fs::read_to_string(pkg.join("manifest-sha512.txt")).expect("BagIt manifest");
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            let (hex, path) = l
                .split_once(char::is_whitespace)
                .expect("manifest line shape");
            (path.trim().to_string(), hex.to_string())
        })
        .collect()
}

#[test]
fn manifest_covers_evidence_files_at_emit() {
    let pkg = emit_sample_package();
    let man = read_manifest(pkg.path());

    for f in [
        "runtime/proofs.jsonl",
        "runtime/decisions.jsonl",
        "runtime/assumptions.jsonl",
        "runtime/security-policy.json",
    ] {
        assert!(
            man.iter().any(|(p, _)| p == f),
            "{f} must be in the payload manifest (present on disk: {}); manifest:\n{man:?}",
            pkg.path().join(f).exists()
        );
    }

    // DEPOSIT-READINESS.json stays intentionally excluded (mutable meta).
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
}

#[test]
fn manifest_covers_audit_proof_report_after_reseal() {
    let pkg = emit_sample_package();
    let root = pkg.path();

    // At a fresh EMIT, audit-proof-report.json's wall-clock `evaluated_at`
    // keeps it out of the manifest (see module doc).
    let man = read_manifest(root);
    assert!(!man
        .iter()
        .any(|(p, _)| p == "runtime/audit-proof-report.json"));

    // The post-execution reseal — the state the RCA's `611cf5ee` archive was
    // actually inspected in — must cover it.
    regenerate_bagit_manifest(root, &WallClock).expect("regenerate_bagit_manifest");
    let man = read_manifest(root);
    assert!(
        man.iter().any(|(p, _)| p == "runtime/audit-proof-report.json"),
        "runtime/audit-proof-report.json must be in the payload manifest after reseal; manifest:\n{man:?}"
    );
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
}
