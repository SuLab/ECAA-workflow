//! Task 6 — end-to-end gate: assert that emit+finalize closes all five FAIR
//! metadata gaps identified in the comparison analysis.
//!
//! ## What this test does
//!
//! 1. Copies the `tests/fixtures/finalize-min-pkg` fixture to a tmpdir.
//! 2. Injects a `README.md` File entity into the fixture's
//!    `ro-crate-metadata.json` (representing the entity the real emitter's
//!    `build_metadata` writes — Task 1 registration guard). Also writes a
//!    placeholder `README.md` file so content-integrity can hash it.
//! 3. Writes `runtime/dependency-lock.json` with two R packages so that
//!    `register_software_dependencies` (Task 4) produces ≥2
//!    `SoftwareApplication` entities AND `register_reexecutability_sidecars`
//!    (Task 2) registers the lock as a re-executability signal.
//! 4. Calls `finalize_package`, which internally calls
//!    `finalize_evidence_registration_with_verifier` — the controller that
//!    invokes the five finalize-time steps in order:
//!    register_produced_output_tables → register_reexecutability_sidecars →
//!    register_software_dependencies → register_content_integrity →
//!    register_preview_entity + render_and_write_preview → bagit reseal.
//! 5. Asserts every target postcondition.
//!
//! ## Deferred assertions (covered by unit tests)
//!
//! - `runtime/reexecution.json` and `runtime/cost-ledger.jsonl` registration:
//!   these are presence-gated and the minimal fixture has no reexecution report
//!   or cost ledger. Their individual registration is covered by the unit test
//!   `content_integrity_and_reexec_sidecars_are_registered` in `ro_crate.rs`.
//! - `policies/container.json` sidecar: same — presence-gated, absent from the
//!   minimal fixture. Covered by the `ro_crate.rs` unit test.
//! - README.md registered at emit-time (not finalize-time): covered by the
//!   `readme_is_registered_as_file_and_linked_from_haspart` unit test in
//!   `ro_crate.rs`. We pre-inject it here to make the e2e fixture realistic.

use serde_json::Value;
use std::path::Path;

/// Recursively copy `src` → `dst` (mirrors the helper in finalize_package.rs).
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

/// Inject a `README.md` `File` entity into the fixture's
/// `ro-crate-metadata.json` so that the post-finalize graph contains the
/// entity the real emitter's `build_metadata` writes (Task 1 gap).
/// Also injects `README.md` into the root `hasPart` array.
fn inject_readme_entity(root: &Path) {
    let descriptor = root.join("ro-crate-metadata.json");
    let bytes = std::fs::read(&descriptor).unwrap();
    let mut doc: Value = serde_json::from_slice(&bytes).unwrap();
    let graph = doc["@graph"].as_array_mut().unwrap();

    // Guard: don't double-inject on repeat calls.
    if graph.iter().any(|e| e["@id"] == "README.md") {
        return;
    }

    graph.push(serde_json::json!({
        "@id": "README.md",
        "@type": "File",
        "name": "Package README — human landing page",
        "description": "Human-readable entry point.",
        "encodingFormat": "text/markdown",
        "about": {"@id": "./"}
    }));

    // Link from root hasPart.
    if let Some(root_entity) = graph.iter_mut().find(|e| e["@id"] == "./") {
        let has_part = root_entity["hasPart"]
            .as_array_mut()
            .expect("root hasPart is an array");
        if !has_part.iter().any(|p| p["@id"] == "README.md") {
            has_part.push(serde_json::json!({"@id": "README.md"}));
        }
    }

    let serialized = serde_json::to_vec_pretty(&doc).unwrap();
    std::fs::write(&descriptor, serialized).unwrap();
}

/// Write a minimal `runtime/dependency-lock.json` with two R packages so that:
/// - `register_software_dependencies` produces ≥2 `SoftwareApplication` nodes
///   (Task 4 — software-stack enumeration).
/// - `register_reexecutability_sidecars` registers the lock as a re-executability
///   signal in the `@graph` (Task 2 — accountability sidecars).
fn write_dependency_lock(root: &Path) {
    let lock_path = root.join("runtime/dependency-lock.json");
    std::fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
    let lock = serde_json::json!({
        "schema_version": "1.0",
        "r": [
            {"name": "DESeq2",   "requested": ">=1.40.0", "resolved": "1.50.2"},
            {"name": "apeglm",   "requested": ">=1.24.0", "resolved": "1.24.0"}
        ]
    });
    std::fs::write(&lock_path, serde_json::to_vec_pretty(&lock).unwrap()).unwrap();
}

#[test]
fn emitted_crate_closes_all_fair_gaps() {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    // ── Pre-finalize fixture augmentation ─────────────────────────────────────
    // 1. README.md entity (Task 1: registered at emit-time in real packages).
    inject_readme_entity(&root);
    // Write the actual file so content-integrity can hash it.
    std::fs::write(root.join("README.md"), b"# Test package\n").unwrap();

    // 2. dependency-lock.json (Task 2 sidecar + Task 4 SoftwareApplication).
    write_dependency_lock(&root);

    // ── Run finalize ──────────────────────────────────────────────────────────
    let secret = [42u8; 32];
    ecaa_workflow_core::finalize::finalize_package(
        &root,
        &config_dir,
        ecaa_workflow_core::project_class::ProjectClass::default(),
        &[],
        true,
        Some(&secret),
    )
    .expect("finalize_package must succeed");

    // ── Load the finalised descriptor ─────────────────────────────────────────
    let descriptor_bytes = std::fs::read(root.join("ro-crate-metadata.json"))
        .expect("ro-crate-metadata.json must exist after finalize");
    let doc: Value = serde_json::from_slice(&descriptor_bytes)
        .expect("ro-crate-metadata.json must be valid JSON");
    let graph = doc["@graph"].as_array().expect("@graph must be an array");

    // Collect all @id strings for presence checks.
    let ids: std::collections::BTreeSet<&str> =
        graph.iter().filter_map(|e| e["@id"].as_str()).collect();

    // ── Gap 1: ro-crate-preview.html exists on disk AND is registered (Task 5) ─
    assert!(
        root.join("ro-crate-preview.html").exists(),
        "ro-crate-preview.html must exist on disk after finalize"
    );
    assert!(
        ids.contains("ro-crate-preview.html"),
        "ro-crate-preview.html must be a File entity in @graph; found ids: {ids:?}"
    );

    // ── Gap 2: README.md is a first-class File entity (Task 1) ───────────────
    assert!(
        ids.contains("README.md"),
        "README.md must be registered as a File entity in @graph; found ids: {ids:?}"
    );

    // ── Gap 3: re-executability signal — dependency-lock.json registered
    //           as an accountability sidecar (Task 2) ──────────────────────────
    assert!(
        ids.contains("runtime/dependency-lock.json"),
        "runtime/dependency-lock.json must be registered as a @graph File entity (Task 2 re-executability signal); ids: {ids:?}"
    );

    // ── Gap 4: in-@graph contentSize + sha512 on a known payload File (Task 3) ─
    // `de_results.tsv` is registered as a `File` entity by
    // `register_produced_output_tables`, then annotated by
    // `register_content_integrity`.
    let de_id = "runtime/outputs/differential_expression/de_results.tsv";
    let de_node = graph
        .iter()
        .find(|e| e["@id"].as_str() == Some(de_id))
        .unwrap_or_else(|| {
            panic!("produced output table {de_id} must be a @graph node after finalize")
        });
    assert!(
        de_node
            .get("contentSize")
            .and_then(|v| v.as_u64())
            .is_some(),
        "de_results.tsv must carry contentSize after finalize; node = {de_node:#?}"
    );
    let sha = de_node
        .get("sha512")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| {
            panic!("de_results.tsv must carry sha512 after finalize; node = {de_node:#?}")
        });
    assert_eq!(
        sha.len(),
        128,
        "sha512 must be a 128-hex-char string; got len={}",
        sha.len()
    );

    // ── Gap 5: software-stack enumeration — SoftwareApplication count > 1 (Task 4) ─
    let n_sw = graph
        .iter()
        .filter(|e| {
            e.get("@type").map_or(false, |t| {
                t.as_str() == Some("SoftwareApplication")
                    || t.as_array().map_or(false, |a| {
                        a.iter().any(|v| v.as_str() == Some("SoftwareApplication"))
                    })
            })
        })
        .count();
    assert!(
        n_sw > 1,
        "software stack must be enumerated with >1 SoftwareApplication entity after finalize (Task 4); got n_sw={n_sw}"
    );
}
