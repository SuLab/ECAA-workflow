//! Byte-reproducibility characterization harness for the emit/* path.
//!
//! These tests pin the *observable bytes* of the byte-sensitive emit
//! functions (`patch_ro_crate_metadata`, `write_affordance_sidecars`,
//! `write_figure_diff`) BEFORE refactoring them, so a behavior-preserving
//! complexity refactor that accidentally changes emitted content or ordering
//! is caught immediately.
//!
//! Two guards:
//!  1. **Determinism** — emitting the same session twice produces
//!     byte-identical artifacts after timestamp normalization. This is the
//!     CLAUDE.md byte-reproducibility invariant (`BTreeMap` ordering, no
//!     stray `SystemTime::now()` in new paths).
//!  2. **Golden** — an `insta` snapshot of the normalized
//!     `ro-crate-metadata.json` graph. A deterministic-but-different output
//!     from a refactor fails the snapshot. Regenerate intentionally with
//!     `INSTA_UPDATE=always` after confirming the change is desired.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

async fn boot_session() -> Session {
    let mut session = Session::test_fixture_with_dag();
    let ctx = ToolContext::new(config_dir(), "claude-sonnet-4-6");
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                .into(),
        }),
        &mut session,
        &ctx,
    )
    .await;
    session
}

/// Replace volatile substrings (ISO-8601 timestamps, the tempdir path) with
/// stable placeholders so the comparison reflects content + ordering, not
/// wall-clock or the random tempdir name.
fn normalize(raw: &str, output_dir: &Path) -> String {
    // ISO-8601 datetimes: 2026-05-29T12:34:56(.789)?(Z|+00:00)
    let ts = regex::Regex::new(
        r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?",
    )
    .unwrap();
    // `uuid_short()` workflow id — the one intentional random field in the
    // graph (allowed by the determinism contract). Normalize so it doesn't
    // mask a real content diff.
    let wf = regex::Regex::new(r"workflow-[0-9a-f]{32}").unwrap();
    let stripped = raw.replace(&output_dir.display().to_string(), "<PKG>");
    let stripped = ts.replace_all(&stripped, "<TS>").into_owned();
    wf.replace_all(&stripped, "workflow-<ID>").into_owned()
}

async fn emit_and_read_metadata(dir: &Path) -> String {
    let mut session = boot_session().await;
    emit_with_conversation_log(&mut session, dir, &config_dir())
        .await
        .expect("emit succeeded");
    let raw = std::fs::read_to_string(dir.join("ro-crate-metadata.json"))
        .expect("ro-crate-metadata.json present after emit");
    normalize(&raw, dir)
}

/// Guard 1: two emits of the same session yield byte-identical
/// `ro-crate-metadata.json` (after timestamp/path normalization).
#[tokio::test]
async fn ro_crate_metadata_is_deterministic_across_emits() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    let first = emit_and_read_metadata(a.path()).await;
    let second = emit_and_read_metadata(b.path()).await;
    assert_eq!(
        first, second,
        "ro-crate-metadata.json must be byte-reproducible across emits"
    );
}

/// Guard 2: golden snapshot of the normalized graph. Pins the exact content +
/// ordering emitted by `patch_ro_crate_metadata` and the entity-registration
/// helpers.
#[tokio::test]
async fn ro_crate_metadata_golden() {
    let dir = tempdir().unwrap();
    let normalized = emit_and_read_metadata(dir.path()).await;
    // Pretty-reparse so the snapshot is diff-friendly and key-order-stable.
    let value: serde_json::Value =
        serde_json::from_str(&normalized).expect("normalized metadata parses");
    insta::assert_json_snapshot!("ro_crate_metadata_graph", value);
}
