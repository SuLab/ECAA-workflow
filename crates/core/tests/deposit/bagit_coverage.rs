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
//! DR-4 folds in the remaining present-on-disk-but-unmanifested evidence
//! files from the `611cf5ee` deposit. The DETERMINISTIC ones
//! (claim-verification.json, validation-reports.jsonl, reexecution.json) are
//! BagIt-manifested at EMIT alongside proofs/decisions/assumptions/
//! security-policy.
//!
//! FACET-1 — three evidence sidecars are NON-deterministic before a run and so
//! are covered in the DEPOSIT (reseal) manifest but excluded from the pre-run
//! EMIT skeleton: `verifier-decisions.jsonl` (destructively drained from the
//! compose-time substrate — empty on an in-process re-emit),
//! `validation-summary.json` (wall-clock `duration_ms`), and
//! `audit-proof-report.json` (verdicts range over the two above). Manifesting
//! them at EMIT leaked their non-determinism into Payload-Oxum +
//! manifest-sha512.txt and broke emit byte-reproducibility. The correct
//! invariant is "covered in the deposit (reseal); excluded from the pre-run
//! emit manifest" — `manifest_covers_evidence_files_at_emit` asserts the emit
//! coverage of the deterministic set + the reseal coverage of the full
//! evidence set, and
//! `manifest_covers_audit_proof_report_excluded_at_emit_covered_after_reseal`
//! pins the emit-exclude / reseal-cover contract for the report.

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

/// The three evidence sidecars that are non-deterministic before a run
/// (FACET-1): excluded from the pre-run EMIT manifest, covered on RESEAL.
const NON_DETERMINISTIC_AT_EMIT: &[&str] = &[
    "runtime/verifier-decisions.jsonl",
    "runtime/validation-summary.json",
    "runtime/audit-proof-report.json",
];

/// The deterministic DR-4 evidence sidecars covered at EMIT (and reseal).
const DETERMINISTIC_EVIDENCE_AT_EMIT: &[&str] = &[
    "runtime/proofs.jsonl",
    "runtime/decisions.jsonl",
    "runtime/assumptions.jsonl",
    "runtime/security-policy.json",
    // DR-4 — the deposit-integrity envelope additionally covers the
    // deterministic evidence sidecars the `611cf5ee` deposit left
    // present-on-disk-but-unmanifested.
    "runtime/claim-verification.json",
    "runtime/validation-reports.jsonl",
    "runtime/reexecution.json",
];

#[test]
fn manifest_covers_evidence_files_at_emit() {
    let pkg = emit_sample_package();
    let man = read_manifest(pkg.path());

    // The deterministic evidence set is manifested at EMIT.
    for f in DETERMINISTIC_EVIDENCE_AT_EMIT {
        assert!(
            man.iter().any(|(p, _)| p == f),
            "{f} must be in the EMIT payload manifest (present on disk: {}); manifest:\n{man:?}",
            pkg.path().join(f).exists()
        );
    }

    // FACET-1 — the three non-deterministic-at-emit sidecars are EXCLUDED from
    // the pre-run emit manifest (they still exist on disk), so their
    // non-determinism cannot leak into Payload-Oxum / manifest-sha512.txt and
    // break emit byte-reproducibility.
    for f in NON_DETERMINISTIC_AT_EMIT {
        assert!(
            pkg.path().join(f).exists(),
            "{f} must still be present on disk at emit"
        );
        assert!(
            !man.iter().any(|(p, _)| p == f),
            "{f} must be EXCLUDED from the EMIT payload manifest (FACET-1); manifest:\n{man:?}"
        );
    }

    // DEPOSIT-READINESS.json stays intentionally excluded (mutable meta).
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
    // The keyed HMAC over decisions.jsonl is verified with the session
    // secret, not by re-hashing into the payload manifest.
    assert!(!man.iter().any(|(p, _)| p == "runtime/decisions.jsonl.mac"));

    // DR-4 deposit guard — after a RESEAL (the deposit finalize surface + the
    // Layer-1 re-verify input) the manifest MUST cover the WHOLE evidence set,
    // including the three non-deterministic-at-emit sidecars and the
    // re-verify input (`claim-verification.json`).
    regenerate_bagit_manifest(pkg.path(), &WallClock).expect("regenerate_bagit_manifest");
    let reseal = read_manifest(pkg.path());
    for f in DETERMINISTIC_EVIDENCE_AT_EMIT
        .iter()
        .chain(NON_DETERMINISTIC_AT_EMIT.iter())
    {
        assert!(
            reseal.iter().any(|(p, _)| p == f),
            "{f} must be covered by the RESEAL (deposit) payload manifest (DR-4); manifest:\n{reseal:?}"
        );
    }
    // The re-verify input stays covered at reseal — DR-4's whole point.
    assert!(
        reseal
            .iter()
            .any(|(p, _)| p == "runtime/claim-verification.json"),
        "claim-verification.json (Layer-1 re-verify input) must stay covered at reseal (DR-4)"
    );
    // DEPOSIT-READINESS.json stays excluded even after reseal (mutable meta).
    assert!(!reseal.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
}

#[test]
fn manifest_covers_audit_proof_report_excluded_at_emit_covered_after_reseal() {
    let pkg = emit_sample_package();
    let root = pkg.path();

    // FACET-1 — audit-proof-report.json's verdicts range over
    // verifier-decisions.jsonl (destructively drained on the conversation emit
    // path) so it is non-deterministic before a run and is EXCLUDED from the
    // pre-run emit manifest, though still present on disk.
    assert!(
        root.join("runtime/audit-proof-report.json").exists(),
        "runtime/audit-proof-report.json must be present on disk at emit"
    );
    let man = read_manifest(root);
    assert!(
        !man.iter().any(|(p, _)| p == "runtime/audit-proof-report.json"),
        "runtime/audit-proof-report.json must be EXCLUDED from the EMIT manifest (FACET-1); manifest:\n{man:?}"
    );

    // The post-execution reseal (the deposit finalize surface) DOES cover it.
    regenerate_bagit_manifest(root, &WallClock).expect("regenerate_bagit_manifest");
    let man = read_manifest(root);
    assert!(
        man.iter().any(|(p, _)| p == "runtime/audit-proof-report.json"),
        "runtime/audit-proof-report.json must be in the payload manifest after reseal; manifest:\n{man:?}"
    );
    assert!(!man.iter().any(|(p, _)| p == "DEPOSIT-READINESS.json"));
}
