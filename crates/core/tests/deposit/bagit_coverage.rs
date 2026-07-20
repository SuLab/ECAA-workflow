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
//! DR-4 — `audit-proof-report.json`'s `evaluated_at` was moved onto the
//! deterministic run-epoch clock
//! (`crates/core/src/emitter/ecaa.rs::write_audit_proof_report`), so it is
//! byte-reproducible across two same-input emits and is now BagIt-manifested
//! at EMIT alongside its evidence siblings — not held back to RESEAL only.
//! `manifest_covers_audit_proof_report_at_emit_and_after_reseal` asserts it is
//! covered at both a fresh emit and after a `regenerate_bagit_manifest` reseal.
//! DR-4 also folds in the remaining present-on-disk-but-unmanifested evidence
//! files from the `611cf5ee` deposit (claim-verification.json,
//! validation-reports.jsonl, verifier-decisions.jsonl, validation-summary.json,
//! reexecution.json).

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
        // DR-4 — the deposit-integrity envelope additionally covers the
        // substantive evidence sidecars that the `611cf5ee` deposit left
        // present-on-disk-but-unmanifested.
        "runtime/claim-verification.json",
        "runtime/validation-reports.jsonl",
        "runtime/verifier-decisions.jsonl",
        "runtime/validation-summary.json",
        "runtime/reexecution.json",
        "runtime/audit-proof-report.json",
    ] {
        assert!(
            man.iter().any(|(p, _)| p == f),
            "{f} must be in the payload manifest (present on disk: {}); manifest:\n{man:?}",
            pkg.path().join(f).exists()
        );
    }

    // DEPOSIT-READINESS.json stays intentionally excluded (mutable meta).
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
    // The keyed HMAC over decisions.jsonl is verified with the session
    // secret, not by re-hashing into the payload manifest.
    assert!(!man.iter().any(|(p, _)| p == "runtime/decisions.jsonl.mac"));
}

#[test]
fn manifest_covers_audit_proof_report_at_emit_and_after_reseal() {
    let pkg = emit_sample_package();
    let root = pkg.path();

    // DR-4 — audit-proof-report.json's `evaluated_at` now uses the
    // deterministic run-epoch clock, so it is byte-reproducible and covered
    // by the payload manifest already at a fresh EMIT.
    let man = read_manifest(root);
    assert!(
        man.iter().any(|(p, _)| p == "runtime/audit-proof-report.json"),
        "runtime/audit-proof-report.json must be manifested at emit (DR-4); manifest:\n{man:?}"
    );

    // The post-execution reseal keeps covering it.
    regenerate_bagit_manifest(root, &WallClock).expect("regenerate_bagit_manifest");
    let man = read_manifest(root);
    assert!(
        man.iter().any(|(p, _)| p == "runtime/audit-proof-report.json"),
        "runtime/audit-proof-report.json must be in the payload manifest after reseal; manifest:\n{man:?}"
    );
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
}
