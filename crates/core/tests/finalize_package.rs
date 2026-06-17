//! Integration coverage for `ecaa_workflow_core::finalize::finalize_package`.
//!
//! Copies a checked-in emitted-but-unexecuted fixture (one completed
//! confirmatory `differential_expression` task whose `result.json` carries a
//! matching structured claim, plus a per-package interpretation policy whose
//! `verifiableEntities.expected` block names that stage) into a tempdir, runs
//! the standalone finalize path with a 32-byte secret, and asserts the package
//! is finalized: ≥1 task processed, the HMAC-signed verdict sink written, and
//! the sink's `n_checked` reflects ≥1 verified claim.
//!
//! The signed-sink PATH asserted here is the real one
//! `ecaa_workflow_core::claim_sink::persist_signed_verdicts` writes
//! (`claim_sink::SIGNED_SINK_REL`), not a guess. The emit-time plaintext
//! `runtime/claim-verification.json` stub is deliberately NOT asserted on —
//! finalize never rewrites it; the recomputed verdict counts live only in the
//! signed sink (the loader reads the sink, not the agent-writable stub).

use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::claim_sink::SIGNED_SINK_REL;
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

    // No coverage recall gap: the structured claim addresses the Required
    // manifest entry, so the package finalizes clean.
    assert!(
        summary.coverage_gaps.is_empty(),
        "expected clean coverage, got gaps: {:?}",
        summary.coverage_gaps
    );
}
