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
    let ts =
        regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})?")
            .unwrap();
    // `uuid_short()` workflow id — the one intentional random field in the
    // graph (allowed by the determinism contract). Normalize so it doesn't
    // mask a real content diff.
    let wf = regex::Regex::new(r"workflow-[0-9a-f]{32}").unwrap();
    // Validation timing is wall-clock and legitimately non-deterministic;
    // normalize it so `runtime/validation-summary.json`'s deterministic
    // fields stay covered without the timing field masking a real diff.
    let dur = regex::Regex::new(r#""duration_ms":\s*\d+"#).unwrap();
    let stripped = raw.replace(&output_dir.display().to_string(), "<PKG>");
    let stripped = ts.replace_all(&stripped, "<TS>").into_owned();
    let stripped = wf.replace_all(&stripped, "workflow-<ID>").into_owned();
    dur.replace_all(&stripped, r#""duration_ms": <DUR>"#).into_owned()
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

/// Relative paths whose content is intentionally NOT byte-reproducible across
/// emits. These carry per-emit wall-clock timestamps (`audit-proof-report.json`
/// — a spec-documented exclusion, see `docs/ecaa-spec/operations.md`),
/// host-varying env capture (`determinism-shim.json`), session-replay logs
/// (`intake-conversation.jsonl`, `decisions.jsonl`), or content keyed off the
/// per-session `OsRng` HMAC secret (`decisions.jsonl.mac` — the MAC of the
/// already-excluded `decisions.jsonl`). They are excluded from the cross-emit
/// determinism diff. Mirrors the documented non-deterministic surface and the
/// BagIt manifest exclusion set in `crates/core/src/emitter/bagit.rs`.
const NON_DETERMINISTIC_ALLOWLIST: &[&str] = &[
    "runtime/audit-proof-report.json",
    "runtime/intake-conversation.jsonl",
    "runtime/decisions.jsonl",
    "runtime/verifier-decisions.jsonl",
    "runtime/determinism-shim.json",
    "bco.json",
    // Derivative of an allowlisted file: an HMAC-SHA256 over `decisions.jsonl`
    // keyed by the session's per-emit `OsRng` secret, so it is non-deterministic
    // across two freshly-booted sessions even though the signed content is.
    "runtime/decisions.jsonl.mac",
    // M2 — written by the harness at runtime; carries dispatch-time
    // `started_at` + per-run `epoch`, so it is not byte-reproducible
    // across emits. Excluded from the cross-emit determinism diff.
    "runtime/invocations.jsonl",
];

/// True when `rel` is on the documented non-deterministic allowlist and must
/// be skipped by the cross-emit determinism diff.
fn is_non_deterministic(rel: &str) -> bool {
    NON_DETERMINISTIC_ALLOWLIST.contains(&rel)
}

/// M2 — runtime/invocations.jsonl carries dispatch-time timestamps + a
/// per-run epoch, so it must be on the non-deterministic allowlist and
/// never enter the cross-emit byte-diff.
#[test]
fn invocations_jsonl_is_on_non_deterministic_allowlist() {
    assert!(
        is_non_deterministic("runtime/invocations.jsonl"),
        "invocations.jsonl must be excluded from the byte-diff baseline"
    );
}

/// Emit the fixture session into `dir` and return a stable, normalized map of
/// every emitted *deterministic* artifact: relative-path → normalized content.
/// The non-deterministic allowlist is filtered out. Paths are
/// forward-slash-normalized and the map is a `BTreeMap` so iteration order is
/// stable regardless of host filesystem walk order.
fn collect_emitted_files(dir: &Path) -> std::collections::BTreeMap<String, String> {
    use std::collections::BTreeMap;

    let mut out: BTreeMap<String, String> = BTreeMap::new();
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let entries = std::fs::read_dir(&cur)
            .unwrap_or_else(|e| panic!("read_dir {}: {e}", cur.display()));
        for entry in entries {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let rel = path
                .strip_prefix(dir)
                .expect("emitted path under package root")
                .to_string_lossy()
                .replace('\\', "/");
            if is_non_deterministic(&rel) {
                continue;
            }
            // Read as bytes first; only text artifacts get normalized. Any
            // non-UTF8 artifact (e.g. copied binary libs) is compared via a
            // lossy string — still byte-stable across emits of the same inputs.
            let raw = match std::fs::read(&path) {
                Ok(b) => String::from_utf8_lossy(&b).into_owned(),
                Err(e) => panic!("read {}: {e}", path.display()),
            };
            out.insert(rel, normalize(&raw, dir));
        }
    }
    out
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

/// Guard 1b: two emits of the same session yield byte-identical content for
/// EVERY deterministic emitted artifact — not just `ro-crate-metadata.json`.
/// This widens the determinism net to `WORKFLOW.json`, the per-task
/// `runtime/outputs/<task_id>/task-spec.json` sidecars, and
/// `runtime/security-policy.json`, plus everything else that isn't on the
/// documented non-deterministic allowlist. A determinism regression in any of
/// these (stray `SystemTime::now()`, `HashMap` ordering, host-path leak) now
/// fails here instead of slipping through because only the RO-Crate metadata
/// was pinned.
#[tokio::test]
async fn all_deterministic_emitted_files_are_byte_identical_across_emits() {
    let a = tempdir().unwrap();
    let b = tempdir().unwrap();
    // Emit the SAME session twice (this guard's stated intent). A per-session
    // id legitimately varies between two `boot_session()`s and is normalized
    // out of ro-crate-metadata by Guard 1; re-emitting a single session
    // isolates GENUINE non-determinism (SystemTime / HashMap order / host path).
    let mut session = boot_session().await;
    emit_with_conversation_log(&mut session, a.path(), &config_dir())
        .await
        .expect("first emit succeeded");
    emit_with_conversation_log(&mut session, b.path(), &config_dir())
        .await
        .expect("second emit succeeded");
    let first = collect_emitted_files(a.path());
    let second = collect_emitted_files(b.path());

    // The two emits must cover the exact same set of relative paths. A diff in
    // the *file set* (a sidecar that appears in one emit but not the other) is
    // itself a determinism bug, so assert the key sets match before comparing
    // contents.
    let first_keys: Vec<&String> = first.keys().collect();
    let second_keys: Vec<&String> = second.keys().collect();
    assert_eq!(
        first_keys, second_keys,
        "the set of emitted deterministic files must be identical across emits"
    );

    // Sanity-check that the widened coverage actually includes the artifacts
    // this guard exists to protect, so a future emit-layout change that drops
    // them can't silently make the test vacuous.
    assert!(
        first.contains_key("WORKFLOW.json"),
        "WORKFLOW.json must be emitted and covered by the determinism diff"
    );
    assert!(
        first.contains_key("runtime/security-policy.json"),
        "runtime/security-policy.json must be covered by the determinism diff"
    );
    assert!(
        first
            .keys()
            .any(|k| k.starts_with("runtime/outputs/") && k.ends_with("/task-spec.json")),
        "at least one per-task runtime/outputs/<id>/task-spec.json must be covered"
    );

    // Per-file byte-identity (after timestamp/path/uuid normalization).
    for (rel, content_a) in &first {
        let content_b = second
            .get(rel)
            .unwrap_or_else(|| panic!("{rel} present in first emit but missing in second"));
        assert_eq!(
            content_a, content_b,
            "{rel} must be byte-reproducible across emits (normalized)"
        );
    }
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
