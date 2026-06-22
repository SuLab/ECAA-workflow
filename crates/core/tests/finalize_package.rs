//! Integration coverage for `ecaa_workflow_core::finalize::finalize_package`.
//!
//! Copies a checked-in emitted-but-unexecuted fixture (one completed
//! confirmatory `differential_expression` task whose `result.json` carries a
//! matching structured claim, plus a per-package interpretation policy whose
//! `verifiableEntities.expected` block names that stage) into a tempdir, runs
//! the standalone finalize path with a 32-byte secret, and asserts the package
//! is finalized: 1+ task processed, the HMAC-signed verdict sink written and
//! reflecting 1+ verified claim, AND the plaintext operator/UI-visible
//! `runtime/claim-verification.json` refreshed so its `n_checked` is 1+
//! (the assertion Task 9 Step 3 / Task 6 Step 5 probe with `jq '.n_checked'`).
//!
//! The signed-sink PATH asserted here is the real one
//! `ecaa_workflow_core::claim_sink::persist_signed_verdicts` writes
//! (`claim_sink::SIGNED_SINK_REL`), not a guess. The plaintext path is the
//! emit-time stub `runtime/claim-verification.json` that
//! `core::finalize::finalize_task` now refreshes in place post-execution via
//! `claim_sink::refresh_plaintext_sidecar` — previously a standalone run left
//! it at `n_checked: 0` (RISK A). The signed sink stays the trust surface; the
//! plaintext is the populated human-readable view.

use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_sink::SIGNED_SINK_REL;
use serde_json::Value;
use std::path::Path;

/// Recursively copy `src` → `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

#[test]
fn finalize_package_populates_signed_sink_and_checks_claims() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    // The finalize path reads the BASE interpretation policy + extractor config
    // from `config_dir/downstream-policy/`; point it at the repo's real shipped
    // config (CARGO_MANIFEST_DIR is crates/core, so ../../config is repo root).
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    let secret = [7u8; 32];
    let summary = ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package");

    assert!(
        summary.tasks_finalized >= 1,
        "expected ≥1 completed task to be finalized, got {}",
        summary.tasks_finalized
    );

    // The HMAC-signed verdict sink must exist at the canonical path.
    let sink_path = root.join(SIGNED_SINK_REL);
    assert!(
        sink_path.exists(),
        "signed verdict sink must be written at {}",
        sink_path.display()
    );

    // The sink verifies with the same secret and records ≥1 checked claim.
    let writer = AuditWriter::with_secret(secret);
    let raw = std::fs::read_to_string(&sink_path).unwrap();
    // One independently-signed JSONL row per finalized task; this fixture has
    // exactly one finalized task → one row.
    let line = raw.lines().next().expect("signed sink has ≥1 row");
    let signed: serde_json::Value = serde_json::from_str(line).unwrap();
    let inner = writer
        .verify_row(&signed)
        .expect("signed sink must verify with the finalize secret");
    let n_checked = inner["n_checked"].as_u64().expect("n_checked present");
    assert!(
        n_checked >= 1,
        "finalize must check ≥1 claim; signed-sink n_checked = {}",
        n_checked
    );

    // RISK A: the plaintext operator/UI-visible sidecar must ALSO be refreshed
    // in place (no longer the empty emit-time stub) so `jq '.n_checked'` >= 1 —
    // the acceptance assertion Task 9 Step 3 / Task 6 Step 5 probe after a
    // standalone harness run. finalize_task rewrites it from the recomputed
    // report via claim_sink::refresh_plaintext_sidecar.
    let plaintext_path = root.join("runtime/claim-verification.json");
    let plaintext: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plaintext_path).unwrap()).unwrap();
    let plaintext_n_checked = plaintext["n_checked"]
        .as_u64()
        .expect("plaintext n_checked present");
    assert!(
        plaintext_n_checked >= 1,
        "finalize must refresh the plaintext claim-verification.json; \
         n_checked = {} (was left an empty stub before the RISK A fix)",
        plaintext_n_checked
    );
    // The refreshed counts must match the verdict rows on disk (internal
    // consistency of the rewritten flat report).
    let n_verdicts = plaintext["verdicts"]
        .as_array()
        .expect("verdicts array present")
        .len() as u64;
    assert_eq!(
        plaintext_n_checked, n_verdicts,
        "plaintext n_checked must equal the verdict-row count"
    );

    // No coverage recall gap: the structured claim addresses the Required
    // manifest entry, so the package finalizes clean.
    assert!(
        summary.coverage_gaps.is_empty(),
        "expected clean coverage, got gaps: {:?}",
        summary.coverage_gaps
    );
}

/// After finalize, every produced result table registered into the RO-Crate
/// `@graph` must point back to its producing stage through standard PROV
/// relations: the output File/Dataset node carries `wasGeneratedBy` referencing
/// a `CreateAction` whose `result` is the output and whose `object` (PROV
/// `used`) references the producing task's input(s) — derived from the task's
/// `ParameterConnection` edges already in the graph (here `#step-data_import`
/// → `#step-differential_expression`).
#[test]
fn finalize_emits_per_output_was_generated_by_create_action() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    let secret = [7u8; 32];
    ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package");

    let descriptor = root.join("ro-crate-metadata.json");
    let doc: Value =
        serde_json::from_str(&std::fs::read_to_string(&descriptor).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().expect("@graph array");

    // The produced result table for the differential_expression stage.
    let output_id = "runtime/outputs/differential_expression/de_results.tsv";

    // 1. The output File/Dataset node carries `wasGeneratedBy`.
    let output_node = graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(output_id))
        .unwrap_or_else(|| panic!("produced output table {output_id} must be a @graph node"));
    let action_ref = output_node["wasGeneratedBy"]["@id"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("output node {output_id} must carry wasGeneratedBy.@id; node = {output_node}")
        });

    // 2. The referenced CreateAction exists with the producing step as
    // `instrument`, the output as `result`, and the task's input(s) as `object`.
    let action = graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(action_ref))
        .unwrap_or_else(|| panic!("CreateAction {action_ref} referenced by wasGeneratedBy missing"));

    let types: Vec<&str> = action["@type"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .or_else(|| action["@type"].as_str().map(|s| vec![s]))
        .unwrap_or_default();
    assert!(
        types.contains(&"CreateAction") || types.contains(&"prov:Activity"),
        "action {action_ref} must be a CreateAction/prov:Activity; @type = {:?}",
        action["@type"]
    );

    assert_eq!(
        action["result"]["@id"].as_str(),
        Some(output_id),
        "CreateAction.result must reference the produced output"
    );
    assert_eq!(
        action["instrument"]["@id"].as_str(),
        Some("#step-differential_expression"),
        "CreateAction.instrument must reference the producing workflow step"
    );

    // 3. The `object` (PROV used) references the task's input(s), derived from
    // the ParameterConnection edge `#step-data_import` →
    // `#step-differential_expression`.
    let objects = action["object"]
        .as_array()
        .expect("CreateAction.object must be an array of input @id refs");
    let object_ids: Vec<&str> = objects
        .iter()
        .filter_map(|o| o["@id"].as_str())
        .collect();
    assert!(
        object_ids.contains(&"#step-data_import"),
        "CreateAction.object must reference the task input #step-data_import; got {object_ids:?}"
    );
}

/// Locate the CreateAction node that produced the differential_expression
/// output table in a finalized package descriptor.
fn de_output_create_action(root: &Path) -> Value {
    let descriptor = root.join("ro-crate-metadata.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&descriptor).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().expect("@graph array").clone();
    let output_id = "runtime/outputs/differential_expression/de_results.tsv";
    let output_node = graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(output_id))
        .unwrap_or_else(|| panic!("produced output {output_id} must be a @graph node"));
    let action_ref = output_node["wasGeneratedBy"]["@id"].as_str().unwrap();
    graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(action_ref))
        .cloned()
        .unwrap_or_else(|| panic!("CreateAction {action_ref} must exist"))
}

/// FAITHFUL TWIN (B1): when the task carries a `.container-state.json` recording
/// a real `ended_at` and executor image, the produced output's `CreateAction`
/// carries THAT EXACT `endTime`, a real `agent` referencing an executor entity
/// built from the recorded image, and that executor entity is itself a node in
/// the `@graph`. Only recorded values appear — and there is no recorded
/// `startTime`, so it is honestly omitted.
#[test]
fn finalize_create_action_uses_recorded_container_state_agent_and_end_time() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    // Write a recorded container-state sidecar for the producing task BEFORE
    // finalize, with a known ended_at + executor image.
    let ended_at = "2026-05-05T12:34:56Z";
    let image = "ghcr.io/scripps/scripps-bio-base:1.4.4";
    let task_dir = root.join("runtime/outputs/differential_expression");
    std::fs::create_dir_all(&task_dir).unwrap();
    std::fs::write(
        task_dir.join(".container-state.json"),
        serde_json::to_vec(&serde_json::json!({
            "task_id": "differential_expression",
            "exit_code": 0,
            "image": image,
            "runtime": "docker",
            "session_id": "s-1",
            "backend": "aws",
            "ended_at": ended_at
        }))
        .unwrap(),
    )
    .unwrap();

    let secret = [7u8; 32];
    ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package");

    let action = de_output_create_action(&root);

    // endTime is the EXACT recorded ended_at.
    assert_eq!(
        action["endTime"].as_str(),
        Some(ended_at),
        "CreateAction.endTime must be the recorded .container-state.json ended_at; action={action:#?}"
    );
    // No fabricated startTime (the sidecar records none).
    assert!(
        action.get("startTime").is_none(),
        "CreateAction.startTime must be omitted (no recorded start timestamp); action={action:#?}"
    );
    // agent references a real executor entity present in the @graph.
    let agent_id = action["agent"]["@id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateAction must carry agent.@id; action={action:#?}"));
    let descriptor = root.join("ro-crate-metadata.json");
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(&descriptor).unwrap()).unwrap();
    let graph = doc["@graph"].as_array().unwrap();
    let agent_node = graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(agent_id))
        .unwrap_or_else(|| panic!("agent entity {agent_id} must exist in @graph"));
    assert_eq!(
        agent_node["softwareVersion"].as_str(),
        Some(image),
        "the executor agent entity must record the real container image; node={agent_node:#?}"
    );
}

/// FAITHFUL TWIN (B1): with NO `.container-state.json` (the default fixture
/// shape), the CreateAction honestly OMITS `agent` and `endTime` rather than
/// fabricating them — provenance never invents a time or executor.
#[test]
fn finalize_create_action_omits_agent_and_end_time_when_no_container_state() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    let secret = [7u8; 32];
    ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package");

    let action = de_output_create_action(&root);
    assert!(
        action.get("agent").is_none(),
        "absent .container-state.json must yield no fabricated agent; action={action:#?}"
    );
    assert!(
        action.get("endTime").is_none(),
        "absent .container-state.json must yield no fabricated endTime; action={action:#?}"
    );
}

/// FAITHFUL TWIN (B2): after a full finalize, every embedded `InvariantVerdict`
/// node in the descriptor `@graph` EQUALS the authoritative status in the at-rest
/// `runtime/audit-proof-report.json`. The re-injection reconciles the emit-time
/// embedded verdicts with the post-exec recomputed report so the two never
/// silently disagree.
#[test]
fn finalize_embedded_invariant_verdicts_equal_at_rest_report() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    let secret = [7u8; 32];
    ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package");

    // Authoritative report: invariant_id -> status.
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("runtime/audit-proof-report.json")).unwrap(),
    )
    .unwrap();
    let mut report_status: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for v in report["verdicts"].as_array().expect("report verdicts") {
        let id = v["id"].as_str().expect("verdict id").to_string();
        let status = v["status"].as_str().expect("verdict status").to_string();
        report_status.insert(id, status);
    }
    assert!(
        !report_status.is_empty(),
        "the at-rest report must carry verdicts to compare against"
    );

    // Embedded InvariantVerdict nodes in the descriptor.
    let doc: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("ro-crate-metadata.json")).unwrap(),
    )
    .unwrap();
    let graph = doc["@graph"].as_array().unwrap();
    let mut embedded_seen = 0;
    for node in graph {
        if node["@type"].as_str() != Some("InvariantVerdict") {
            continue;
        }
        embedded_seen += 1;
        let inv = node["invariant_id"].as_str().expect("embedded invariant_id");
        let verdict = node["verdict"].as_str().expect("embedded verdict");
        let authoritative = report_status
            .get(inv)
            .unwrap_or_else(|| panic!("embedded verdict {inv} has no at-rest counterpart"));
        assert_eq!(
            verdict, authoritative,
            "embedded InvariantVerdict {inv} = {verdict} must EQUAL at-rest report status {authoritative}"
        );
    }
    assert_eq!(
        embedded_seen,
        report_status.len(),
        "every at-rest verdict must have a reconciled embedded node"
    );
}

