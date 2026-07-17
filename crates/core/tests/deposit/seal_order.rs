//! RCA I-2 — seal order + post-seal hash recheck.
//!
//! The deposited `611cf5ee` package recorded a `WORKFLOW.json` `sha512` in
//! `ro-crate-metadata.json` that did NOT match the on-disk file (RO-Crate
//! `70b5541…` vs actual/`manifest-sha512.txt` `23748e4…`): the descriptor's
//! embedded per-file content hashes were captured, the payload was mutated
//! again, and the BagIt manifest was regenerated afterward — but the
//! descriptor's embedded hashes were never refreshed.
//!
//! `ecaa_workflow_core::ro_crate::register_content_integrity` is the writer
//! (embeds `{contentSize, sha512}` onto every `File` `@graph` entity);
//! `ecaa_workflow_core::emitter::regenerate_bagit_manifest` is the reseal
//! primitive every post-execution call site (`finalize.rs`, the harness
//! end-of-run snapshot, the repair loop) uses to bring a mutated package
//! back to a self-consistent BagIt manifest. The fix enforces seal order AT
//! THAT PRIMITIVE: `regenerate_bagit_manifest` must refresh the RO-Crate's
//! embedded content hashes BEFORE it recomputes `manifest-sha512.txt`, and
//! must refuse to reseal a package whose recorded hashes still disagree
//! with the payload afterward.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::dag::{current_dag_schema_version, Task, TaskId, DAG};
use ecaa_workflow_core::emitter::{emit_package, regenerate_bagit_manifest, EmitConfig};
use ecaa_workflow_core::{deposit_readiness, ro_crate};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;

fn minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "seal-order integration smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        intake_text: "seal-order test fixture".into(),
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
        workflow_id: "test-seal-order".into(),
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

/// Emit a real, minimal package via the public `emit_package` entry point —
/// the same core surface `intake`/`build` and the conversation emit wrapper
/// call. No execution — this is the compiler's emit/seal path only.
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

fn sha512_file(path: &Path) -> String {
    use sha2::{Digest, Sha512};
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut h = Sha512::new();
    h.update(&bytes);
    format!("{:x}", h.finalize())
}

/// Reproduces the exact I-2 mechanism using only the public seal primitives:
/// 1. A mid-run finalize step embeds content hashes into the RO-Crate
///    `@graph` (the real call site: `ro_crate::finalize_evidence_registration`).
/// 2. A LATER task-state transition mutates a manifested payload file
///    (`WORKFLOW.json` is rewritten on every harness state transition).
/// 3. A LATER bare reseal (the real call sites: `finalize.rs`'s trailing
///    reseal, the harness end-of-run snapshot, the repair loop) regenerates
///    the BagIt manifest.
///
/// After the fix, step 3's `regenerate_bagit_manifest` must itself refresh
/// the embedded content hashes before resealing, so every hash the
/// descriptor records equals the sealed payload — never a value captured
/// before the step-2 mutation.
#[test]
fn ro_crate_recorded_hashes_match_payload_after_seal() {
    let pkg = emit_sample_package();
    let root = pkg.path();

    ro_crate::register_content_integrity(root).expect("register_content_integrity");

    // Simulate a later WORKFLOW.json state-transition rewrite.
    let workflow_path = root.join("WORKFLOW.json");
    let mut content = std::fs::read_to_string(&workflow_path).unwrap();
    content.push('\n');
    std::fs::write(&workflow_path, &content).unwrap();

    // Simulate the later bare reseal.
    regenerate_bagit_manifest(root, &WallClock).expect("regenerate_bagit_manifest");

    let recorded = ro_crate::recorded_content_hashes(root);
    assert!(
        !recorded.is_empty(),
        "expected the RO-Crate to carry embedded content-integrity hashes"
    );
    for (path, hex) in &recorded {
        let actual = sha512_file(&root.join(path));
        assert_eq!(
            hex, &actual,
            "RO-Crate hash for {path} must match the sealed payload after reseal"
        );
    }
}

/// The post-seal recheck primitive itself: it must be silent on a
/// consistently-sealed package and must catch — by name — a payload file
/// mutated after its hash was recorded, without requiring a reseal to
/// detect the drift.
#[test]
fn post_seal_recheck_fails_when_ro_crate_hash_disagrees_with_payload() {
    let pkg = emit_sample_package();
    let root = pkg.path();
    ro_crate::register_content_integrity(root).expect("register_content_integrity");

    assert!(
        deposit_readiness::recheck_ro_crate_content_hashes(root)
            .unwrap()
            .is_empty(),
        "a freshly-sealed package must have zero hash mismatches"
    );
    assert!(deposit_readiness::assert_ro_crate_hashes_match_payload(root).is_ok());

    // Tamper a manifested payload file WITHOUT re-sealing — the exact
    // finalization-order failure I-2 describes.
    let workflow_path = root.join("WORKFLOW.json");
    let mut content = std::fs::read_to_string(&workflow_path).unwrap();
    content.push('\n');
    std::fs::write(&workflow_path, &content).unwrap();

    let mismatches = deposit_readiness::recheck_ro_crate_content_hashes(root).unwrap();
    assert!(
        mismatches.iter().any(|m| m.path == "WORKFLOW.json"),
        "recheck must catch the stale WORKFLOW.json hash: {mismatches:?}"
    );
    assert!(
        deposit_readiness::assert_ro_crate_hashes_match_payload(root).is_err(),
        "the hard gate must refuse a package with a stale RO-Crate content hash"
    );
}
