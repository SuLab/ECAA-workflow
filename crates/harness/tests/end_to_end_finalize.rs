//! End-to-end coverage for the harness's standalone end-of-run finalize.
//!
//! TEST APPROACH: the real end-of-run path lives inside the private `run_loop`
//! in `main.rs` (an `after.is_complete()` block), which an integration test
//! cannot drive (the binary's loop is not part of the lib's public surface, and
//! the `dry-run`/`MockExecutor` harness path is only reachable from in-`main.rs`
//! tests). The brief authorizes the extracted-helper approach: the
//! finalize-at-end logic is lifted into the public, testable
//! `ecaa_workflow_harness::end_of_run_finalize::finalize_completed_package`, and
//! `run_loop` calls THAT. This test drives the helper directly against a
//! checked-in emitted-but-unexecuted fixture (a copy of the Task-3
//! `finalize-min-pkg`: one completed confirmatory `differential_expression`
//! task whose `result.json` carries a matching structured claim) with
//! `ECAA_AUDIT_SECRET` set, and asserts the package was self-finalized exactly
//! as a session-backed server run would finalize it incrementally.
//!
//! Runs ONLY under `--features dry-run` (the brief's harness test feature), so
//! it lives behind `#![cfg(feature = "dry-run")]` and is invoked with
//! `cargo test -p ecaa-workflow-harness --features dry-run --test end_to_end_finalize`.
#![cfg(feature = "dry-run")]

use ecaa_workflow_harness::end_of_run_finalize::finalize_completed_package;
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
fn standalone_run_self_finalizes_the_package() {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/finalize-min-pkg");
    // The finalize path reads the BASE interpretation policy + extractor config
    // from `config_dir/downstream-policy/`; point it at the repo's real shipped
    // config (CARGO_MANIFEST_DIR is crates/harness, so ../../config is repo root).
    let config_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config");

    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);

    // A valid 64-hex-char secret (32 bytes) — the same shape
    // `ecaa-workflow-audit-proof --secret` requires.
    std::env::set_var("ECAA_AUDIT_SECRET", "7".repeat(64));
    // Don't let an ambient ECAA_CONFIG_DIR from the runner shadow our explicit
    // config_dir argument (the helper resolves config_dir separately for the
    // binary path; here we pass it in directly, but the env read inside
    // audit_secret/decisions is independent).
    finalize_completed_package(&root, &config_dir);
    std::env::remove_var("ECAA_AUDIT_SECRET");

    // 1. The HMAC-signed verdict sink must exist (de-vacuifies audit-proof
    //    Inv 1/5). Path is the real one `claim_sink::persist_signed_verdicts`
    //    writes.
    let signed_sink = root.join("runtime/verification-reports/claim-verification.signed.json");
    assert!(
        signed_sink.exists(),
        "signed verdict sink must be written at {}",
        signed_sink.display()
    );

    // 2. The plaintext operator/UI-visible sidecar must be refreshed in place
    //    so `jq '.n_checked'` >= 1 (was an empty emit-time stub before this
    //    standalone finalize).
    let plaintext = root.join("runtime/claim-verification.json");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&plaintext).unwrap()).unwrap();
    let n_checked = v["n_checked"].as_u64().expect("n_checked present");
    assert!(
        n_checked >= 1,
        "standalone finalize must refresh claim-verification.json; n_checked = {}",
        n_checked
    );

    // 3. decisions.jsonl handling: the fixture has no decisions.jsonl
    //    (Task 5 not yet implemented), so an empty/absent log is correct — the
    //    finalize must NOT have created a non-empty one. Assert what's true now:
    //    the file is absent (we never write it) and finalize still succeeded.
    let decisions = root.join("runtime/decisions.jsonl");
    assert!(
        !decisions.exists(),
        "finalize must not fabricate runtime/decisions.jsonl (Task 5 owns populating it)"
    );

    // 4. The BagIt manifest must have been re-sealed over the produced outputs:
    //    it exists and references a `runtime/outputs/` path.
    let manifest = root.join("manifest-sha512.txt");
    let manifest_body = std::fs::read_to_string(&manifest)
        .expect("manifest-sha512.txt must exist after reseal");
    assert!(
        manifest_body.contains("runtime/outputs/"),
        "reseal must cover a runtime/outputs/ path; manifest:\n{}",
        manifest_body
    );
}
