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
