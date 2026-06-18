//! Coverage-gate re-block in the STANDALONE harness path.
//!
//! The server finalizes per-task on each `task_completed` SSE event and, on a
//! Required claim-coverage gap, transitions the task to
//! `Blocked { BlockerKind::ValidationFailed { check: "claim_coverage:<id>" } }`
//! (`crates/server/src/verification.rs`). A standalone harness run (no
//! `--session-id`) emits no events, so that recall/coverage gate never fired —
//! a confirmatory task that authored 0 claims was left `Completed` and its
//! dependents advanced.
//!
//! This drives the harness-side gate helper directly (the `run_loop` it feeds
//! is private and not test-drivable): the helper runs `core::finalize`'s
//! per-task finalize, inspects the returned coverage, and — when enforcement is
//! on — yields the `[claim_coverage]` re-block reason the harness writes into
//! the task's DAG state. The reason round-trips through the core blocker mapper
//! into `BlockerKind::ValidationFailed { check: "claim_coverage:<id>" }`,
//! matching the server byte-for-byte.
//!
//! Fixture: `tests/fixtures/coverage-gap-pkg` — a sibling of the Task-3
//! `finalize-min-pkg` whose completed confirmatory `differential_expression`
//! task carries an EMPTY `claims[]` while the package's
//! `verifiableEntities.expected` still requires one → a Required recall gap.

use ecaa_workflow_core::blocker::{parse_agent_blocker_kind, BlockerKind};
use ecaa_workflow_core::project_class::ProjectClass;
use ecaa_workflow_harness::end_of_run_finalize::coverage_reblock_reason;
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

fn stage_gap_pkg() -> (tempfile::TempDir, std::path::PathBuf) {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/coverage-gap-pkg");
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("pkg");
    copy_tree(&fixture, &root);
    (tmp, root)
}

#[test]
#[serial_test::serial]
fn empty_claims_reblocks_confirmatory_task_as_claim_coverage_validation_failed() {
    let (_tmp, root) = stage_gap_pkg();
    let config_dir = root.join("policies");

    // Enforcement is the DEFAULT (advisory OFF). Match the server: do not set
    // ECAA_HARNESS_CONTRACT_ADVISORY.
    std::env::remove_var("ECAA_HARNESS_CONTRACT_ADVISORY");
    std::env::remove_var("ECAA_CONFIG_DIR");
    // A valid 64-hex-char secret so the per-task signed sink is also written,
    // mirroring the server's per-task finalize.
    std::env::set_var("ECAA_AUDIT_SECRET", "7".repeat(64));
    let secret = ecaa_workflow_harness::end_of_run_finalize::audit_secret_from_env();

    let task_id = "differential_expression";
    let reason = coverage_reblock_reason(
        &root,
        task_id,
        &config_dir,
        ProjectClass::default(),
        &[],
        true, // is_confirmatory
        secret.as_ref(),
    );
    std::env::remove_var("ECAA_AUDIT_SECRET");

    let reason = reason.expect(
        "a confirmatory task with empty claims + a Required expected entry must yield a re-block reason",
    );

    // The marker round-trips through the SAME core mapper the server's blocker
    // promotion uses, producing the exact ValidationFailed shape.
    let kind = parse_agent_blocker_kind("", task_id, &reason, None);
    match kind {
        BlockerKind::ValidationFailed { check, .. } => {
            assert_eq!(
                check,
                format!("claim_coverage:{task_id}"),
                "check must be claim_coverage:<task_id>"
            );
        }
        other => panic!("expected ValidationFailed, got {other:?}"),
    }
}

#[test]
#[serial_test::serial]
fn advisory_mode_leaves_the_task_completed() {
    let (_tmp, root) = stage_gap_pkg();
    let config_dir = root.join("policies");

    // Advisory / warn-only: matches the server's `harness_contract_advisory`
    // branch — the gap is diagnostic only, no re-block.
    std::env::set_var("ECAA_HARNESS_CONTRACT_ADVISORY", "1");
    std::env::remove_var("ECAA_CONFIG_DIR");
    std::env::set_var("ECAA_AUDIT_SECRET", "7".repeat(64));
    let secret = ecaa_workflow_harness::end_of_run_finalize::audit_secret_from_env();

    let task_id = "differential_expression";
    let reason = coverage_reblock_reason(
        &root,
        task_id,
        &config_dir,
        ProjectClass::default(),
        &[],
        true,
        secret.as_ref(),
    );
    std::env::remove_var("ECAA_AUDIT_SECRET");
    std::env::remove_var("ECAA_HARNESS_CONTRACT_ADVISORY");

    assert!(
        reason.is_none(),
        "advisory mode must suppress the re-block (task stays Completed), got {reason:?}"
    );
}
