//! RCA I-10 — per-task domain-validation rollup into deposit-readiness.
//!
//! The deposited `611cf5ee` package had
//! `validate_differential_expression/result.json` recording
//! `validation_passed: false, checks_failed: 1, required_failures:
//! ["differential_expression.response_matches_stated_outcome"]`, yet
//! `DEPOSIT-READINESS.json` read `ro_crate: pass, bagit: pass, reexecution:
//! partial` with no signal of the failed per-task domain check — a run can
//! be "computationally completed" (every stage ran, RO-Crate/BagIt
//! self-validation both pass) while a required domain-correctness check
//! failed, and nothing rolled that failure up into the deposit-level
//! attestation.
//!
//! This drives the REAL export/seal pipeline
//! (`emitter::export_depositable_package`, the same entry point the `export`
//! CLI subcommand uses) over a source package that has a seeded failed
//! `validate_*` self-report, and asserts the resulting
//! `DEPOSIT-READINESS.json` surfaces `domain_validation: fail` +
//! `deposit_ready: false` even though `ro_crate`/`bagit` both pass — and
//! that the Layer-3 `deposit-check` gate ([`deposit_readiness::check_deposit_readiness`])
//! refuses the package even without `--strict`.

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::dag::{current_dag_schema_version, Task, TaskId, DAG};
use ecaa_workflow_core::deposit_readiness::{self, CheckStatus};
use ecaa_workflow_core::emitter::{
    emit_package, export_depositable_package, export_depositable_package_with_profile,
    DepositProfile, EmitConfig,
};
use std::collections::BTreeMap;
use std::path::Path;
use tempfile::TempDir;

fn minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "status-rollup integration smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        intake_text: "status-rollup test fixture".into(),
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
        workflow_id: "test-status-rollup".into(),
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
/// call. No execution — the caller seeds `runtime/outputs/` fixtures.
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

fn write_validate_result(root: &Path, task_id: &str, body: serde_json::Value) {
    let dir = root.join("runtime/outputs").join(task_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("result.json"), body.to_string()).unwrap();
}

/// A "computationally completed" run (every recorded stage's output present,
/// RO-Crate/BagIt self-validation both structurally sound) whose
/// `validate_differential_expression` companion self-reported a failed
/// required domain check must NOT read as deposit-ready, and the downstream
/// gate must refuse it even without `--strict`.
#[test]
fn failed_domain_check_flips_run_to_not_deposit_ready() {
    let src = emit_sample_package();
    // Seed the recording-artifact failure the RCA observed on the real
    // deposited package: an otherwise-correct plain DE-by-condition run
    // whose validate_* companion recorded a failed
    // response_matches_stated_outcome check.
    write_validate_result(
        src.path(),
        "validate_differential_expression",
        serde_json::json!({
            "validation_passed": false,
            "checks_failed": 1,
            "required_failures": ["differential_expression.response_matches_stated_outcome"]
        }),
    );

    let dst = TempDir::new().unwrap();
    export_depositable_package(src.path(), dst.path()).expect("export must succeed");

    let dr = deposit_readiness::read_deposit_readiness(dst.path())
        .expect("reading DEPOSIT-READINESS.json")
        .expect("export must have written an attestation");
    assert_eq!(dr.ro_crate, CheckStatus::Pass, "package is structurally sound");
    assert_eq!(dr.bagit, CheckStatus::Pass, "package is structurally sound");
    assert_eq!(
        dr.domain_validation,
        CheckStatus::Fail,
        "the seeded validate_* self-report must roll up as a domain-validation failure"
    );
    assert!(
        !dr.deposit_ready,
        "a required domain-check failure must block deposit-readiness even though \
         the run is computationally complete: {dr:?}"
    );

    let err = deposit_readiness::check_deposit_readiness(dst.path(), false)
        .expect_err("the Layer-3 gate must refuse a package with a failed domain check");
    assert!(
        format!("{err:#}").contains("domain-correctness"),
        "gate error must name the domain-validation failure: {err:#}"
    );
}

/// The same export pipeline over a package with no domain-validation
/// self-reports at all must read fully deposit-ready (no false positive).
#[test]
fn clean_export_with_no_domain_reports_is_deposit_ready() {
    let src = emit_sample_package();
    let dst = TempDir::new().unwrap();
    export_depositable_package(src.path(), dst.path()).expect("export must succeed");

    let dr = deposit_readiness::read_deposit_readiness(dst.path())
        .expect("reading DEPOSIT-READINESS.json")
        .expect("export must have written an attestation");
    assert_eq!(dr.domain_validation, CheckStatus::Pass);
    assert!(dr.deposit_ready, "a clean export must read deposit-ready: {dr:?}");
    assert!(deposit_readiness::check_deposit_readiness(dst.path(), false).is_ok());
}

/// DR-1 through the real export pipeline. Layer-1 export records
/// `reexecution: not_verified` (the Layer-2 re-execution is driven separately
/// by the CLI export handler). Under the `full` profile that is admitted; but
/// under the `re-executable` profile — whose entire contract is replayability
/// — a package that was never re-executed must NOT read as deposit-ready, and
/// the Layer-3 gate must refuse it even without `--strict`.
#[test]
fn reexecutable_profile_notverified_is_not_deposit_ready() {
    // Baseline: the SAME clean package exported under `full` with a
    // not-verified re-execution IS deposit-ready (NotVerified admitted).
    let src_full = emit_sample_package();
    let dst_full = TempDir::new().unwrap();
    export_depositable_package(src_full.path(), dst_full.path()).expect("full export");
    let dr_full = deposit_readiness::read_deposit_readiness(dst_full.path())
        .unwrap()
        .unwrap();
    assert_eq!(dr_full.profile, "full");
    assert!(
        dr_full.deposit_ready,
        "full + not_verified must stay deposit-ready: {dr_full:?}"
    );
    assert!(deposit_readiness::check_deposit_readiness(dst_full.path(), false).is_ok());

    // Same package, `re-executable` profile: not_verified now blocks on BOTH
    // gates.
    let src_re = emit_sample_package();
    let dst_re = TempDir::new().unwrap();
    export_depositable_package_with_profile(
        src_re.path(),
        dst_re.path(),
        DepositProfile::ReExecutable,
    )
    .expect("re-executable export");
    let dr_re = deposit_readiness::read_deposit_readiness(dst_re.path())
        .unwrap()
        .unwrap();
    assert_eq!(dr_re.profile, "re-executable");
    assert_eq!(dr_re.reexecution, deposit_readiness::ReexecStatus::NotVerified);
    assert!(
        !dr_re.deposit_ready,
        "re-executable + not_verified must NOT read deposit-ready (DR-1): {dr_re:?}"
    );
    let err = deposit_readiness::check_deposit_readiness(dst_re.path(), false)
        .expect_err("Layer-3 gate must refuse a re-executable deposit that was not re-executed");
    assert!(
        format!("{err:#}").contains("re-executable"),
        "gate error must name the re-executable profile: {err:#}"
    );
}
