//! FAITHFUL TWIN (A) for the root `dateCreated` = 2061 bug.
//!
//! Emit a real package with the run epoch `SOURCE_DATE_EPOCH=1782097294`
//! (2026-06-22) and assert that the root `./` Dataset `dateCreated`:
//!   1. parses to that calendar date (NOT a hash-projected far-future
//!      value like 2061), and
//!   2. is CONSISTENT with the BagIt `Bagging-Date` — both anchored to
//!      the same genuine run epoch, not to two different clocks.
//!
//! Before the fix, `dateCreated` came from `frozen_clock_from_intake`
//! (the intake-hash projection, which can land in 2061) while
//! `Bagging-Date` was pinned to 2026-01-01, so the two disagreed and the
//! root date could be decades in the future. The fix anchors the root
//! `dateCreated` to the same run epoch (`run_epoch_clock`) `Bagging-Date`
//! uses; this test guards that emit-path fix directly.
//!
//! This lives in an integration-test binary (not the lib unit tests)
//! because anchoring to `SOURCE_DATE_EPOCH` requires mutating process
//! env, and `unsafe { std::env::set_var }` is denied in the lib crate.

// Workspace lint is `unsafe_code = "deny"`. This module uses
// `unsafe { std::env::set_var / remove_var }` to scope SOURCE_DATE_EPOCH
// around a single emit (unsafe in Rust 2024 because the env table is not
// thread-safe). The mutation is serialized via `serial_test` on the
// SOURCE_DATE_EPOCH key and restored before any assertion; the bounded
// waiver is scoped to this module only.
#![allow(unsafe_code)]

use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::dag::{current_dag_schema_version, Task, TaskId, DAG};
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::collections::BTreeMap;
use tempfile::TempDir;

fn minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "date-consistency integration smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        intake_text: "ro-crate date-consistency test fixture".into(),
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
        workflow_id: "test-date-consistency".into(),
        current_task: None,
        tasks,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    };
    dag.rebuild_reverse_deps();
    dag
}

#[serial_test::serial(SOURCE_DATE_EPOCH)]
#[test]
fn root_date_created_anchored_to_run_epoch_and_matches_bagging_date() {
    let policies_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy");
    let clf = minimal_classification();
    let dag = one_task_dag();

    // Snapshot + restore SOURCE_DATE_EPOCH so concurrent tests aren't
    // affected. Safety: single-process test runner; restored before any
    // assertion can panic so the var never leaks.
    let prior = std::env::var("SOURCE_DATE_EPOCH").ok();
    unsafe {
        std::env::set_var("SOURCE_DATE_EPOCH", "1782097294"); // 2026-06-22
    }

    let tmp = TempDir::new().unwrap();
    let emit_result = emit_package(&EmitConfig {
        objective: None,
        output_dir: tmp.path(),
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
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
        edge_kinds: None,
    });

    let meta = std::fs::read_to_string(tmp.path().join("ro-crate-metadata.json"));
    let bag_info = std::fs::read_to_string(tmp.path().join("bag-info.txt"));

    // Restore env BEFORE assertions so a panic can't leak the var.
    unsafe {
        match prior {
            Some(v) => std::env::set_var("SOURCE_DATE_EPOCH", v),
            None => std::env::remove_var("SOURCE_DATE_EPOCH"),
        }
    }

    emit_result.expect("emit_package must succeed");
    let meta: serde_json::Value =
        serde_json::from_str(&meta.expect("ro-crate-metadata.json present")).unwrap();
    let bag_info = bag_info.expect("bag-info.txt present");

    let root_date = meta
        .get("@graph")
        .and_then(|g| g.as_array())
        .and_then(|g| {
            g.iter()
                .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
        })
        .and_then(|root| root.get("dateCreated"))
        .and_then(|v| v.as_str())
        .expect("root ./ dateCreated present")
        .to_string();

    let parsed =
        chrono::DateTime::parse_from_rfc3339(&root_date).expect("dateCreated parses as RFC-3339");
    assert_eq!(
        parsed
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%d")
            .to_string(),
        "2026-06-22",
        "root dateCreated must anchor to the SOURCE_DATE_EPOCH run date, not a hash projection; got {root_date}"
    );
    assert!(
        bag_info.contains("Bagging-Date: 2026-06-22"),
        "Bagging-Date must anchor to the same run epoch as dateCreated; got:\n{bag_info}"
    );
}
