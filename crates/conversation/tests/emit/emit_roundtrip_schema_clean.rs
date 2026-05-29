//! Layer 1 + 3 of the ECAA emit-validation hardening plan.
//!
//! Layer 1: empirically prove that every real `emit_with_conversation_log`
//! call produces schema-conformant sidecars for the 8 ECAA subgraphs
//! defined in `docs/ecaa-spec/subgraph-schemas/*.schema.json`.
//!
//! Layer 3: continuously enforce in CI. The test runs in the default
//! `cargo test` discovery for `ecaa-workflow-conversation` so the
//! standard `make test` / `make all` gate sees a schema regression
//! immediately.
//!
//! No external Python deps. `jsonschema` is a workspace-pinned pure-Rust
//! crate; schemas are loaded from the in-repo spec directory.
//!
//! The schemas at `docs/ecaa-spec/subgraph-schemas/` are the source of
//! truth. If this test fails, EITHER the schemas drifted OR the emit
//! code regressed. The fix lives wherever the spec doc points.
//!
//! The 8 sidecars / schemas mapping (per `audit_proof::loader`):
//!
//! | schema           | runtime file                            | shape       |
//! |------------------|-----------------------------------------|-------------|
//! | intent           | intake-conversation.jsonl               | array(JSONL)|
//! | decision         | decisions.jsonl                         | array(JSONL)|
//! | execution        | validation-reports.jsonl                | array(JSONL)|
//! | evidence         | proofs.jsonl                            | array(JSONL)|
//! | equivalence      | verifier-decisions.jsonl                | array(JSONL)|
//! | failure          | assumptions.jsonl                       | array(JSONL)|
//! | claim            | claim-verification.json                 | object      |
//! | audit-proof      | audit-proof-report.json                 | object      |
//!
//! Some sidecars are optional at emit time (post-harness ones are appended
//! by the harness at task completion). When a sidecar is missing we treat
//! it as the empty array — that is the correct ECAA "no rows yet" state
//! at emit-time and validates cleanly against the spec's `type: array`.

use jsonschema::JSONSchema;
use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use serde_json::{json, Value};
use serial_test::serial;
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

fn spec_schemas_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("docs")
        .join("ecaa-spec")
        .join("subgraph-schemas")
}

async fn boot_session_with_dag() -> Session {
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

/// Compile a schema by file stem from `docs/ecaa-spec/subgraph-schemas/`.
fn compile_subgraph_schema(stem: &str) -> JSONSchema {
    let path = spec_schemas_dir().join(format!("{stem}.schema.json"));
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("schema not found: {}", path.display()));
    let value: Value =
        serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {}", path.display(), e));
    JSONSchema::compile(&value).unwrap_or_else(|e| panic!("compile {}: {}", path.display(), e))
}

/// Load one of the JSONL sidecars and return its rows as a JSON array.
/// Missing file → empty array (the correct ECAA "no rows yet" state).
fn load_jsonl_as_array(path: &Path) -> Value {
    if !path.exists() {
        return json!([]);
    }
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    let rows: Vec<Value> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str::<Value>(l)
                .unwrap_or_else(|e| panic!("parse {}: {}", path.display(), e))
        })
        .collect();
    Value::Array(rows)
}

/// Load a single-object sidecar. Missing file → None (skipped at validation).
fn load_object(path: &Path) -> Option<Value> {
    if !path.exists() {
        return None;
    }
    let raw =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    Some(
        serde_json::from_str::<Value>(&raw)
            .unwrap_or_else(|e| panic!("parse {}: {}", path.display(), e)),
    )
}

/// Validate `value` against `schema`. Returns `Ok(())` on success or a
/// human-readable error string aggregating every violation.
fn validate(schema: &JSONSchema, value: &Value) -> Result<(), String> {
    match schema.validate(value) {
        Ok(()) => Ok(()),
        Err(errors) => Err(errors
            .map(|e| format!("  - {}: {}", e.instance_path, e))
            .collect::<Vec<_>>()
            .join("\n")),
    }
}

/// One ECAA sidecar slot. Shape determines whether to load it as an array
/// (JSONL) or single object (JSON).
struct Sidecar {
    schema_stem: &'static str,
    file: &'static str,
    /// `true` → JSONL (load as array, validate against array-typed schema).
    /// `false` → single JSON object.
    is_jsonl: bool,
    /// `true` → required; missing file is a test failure.
    /// `false` → optional; missing file → array(JSONL) is `[]` or object
    /// validation is skipped.
    required_at_emit: bool,
}

const SIDECARS: &[Sidecar] = &[
    Sidecar {
        schema_stem: "intent",
        file: "intake-conversation.jsonl",
        is_jsonl: true,
        required_at_emit: true,
    },
    Sidecar {
        schema_stem: "decision",
        file: "decisions.jsonl",
        is_jsonl: true,
        required_at_emit: true,
    },
    Sidecar {
        schema_stem: "execution",
        file: "validation-reports.jsonl",
        is_jsonl: true,
        // Post-harness; emit-time it's the empty array.
        required_at_emit: false,
    },
    Sidecar {
        schema_stem: "evidence",
        file: "proofs.jsonl",
        is_jsonl: true,
        required_at_emit: false,
    },
    Sidecar {
        schema_stem: "equivalence",
        file: "verifier-decisions.jsonl",
        is_jsonl: true,
        required_at_emit: false,
    },
    Sidecar {
        schema_stem: "failure",
        file: "assumptions.jsonl",
        is_jsonl: true,
        required_at_emit: false,
    },
    Sidecar {
        schema_stem: "claim",
        file: "claim-verification.json",
        is_jsonl: false,
        required_at_emit: false,
    },
    Sidecar {
        schema_stem: "audit-proof",
        file: "audit-proof-report.json",
        is_jsonl: false,
        required_at_emit: true,
    },
];

/// Run the 8-schema validation pass over a package root. Returns the
/// `(passed, failed_messages)` tuple — exposed for use by both the
/// `real_emit_passes_all_8_schemas` assertion test and the
/// `scripts/refresh-real-fixture.sh` keep-output binary path.
fn validate_all_eight(pkg_root: &Path) -> (usize, Vec<(String, String)>) {
    let runtime = pkg_root.join("runtime");
    let mut passed = 0usize;
    let mut failed: Vec<(String, String)> = Vec::new();
    for sidecar in SIDECARS {
        let schema = compile_subgraph_schema(sidecar.schema_stem);
        let path = runtime.join(sidecar.file);
        let value: Option<Value> = if sidecar.is_jsonl {
            // JSONL: always load as array (missing → empty array, which
            // is a valid instance of `type: array`).
            Some(load_jsonl_as_array(&path))
        } else if sidecar.required_at_emit {
            // Required single-object sidecar — must exist.
            let v = load_object(&path).unwrap_or_else(|| {
                panic!(
                    "required sidecar missing after real emit: {}",
                    path.display()
                )
            });
            Some(v)
        } else {
            // Optional single-object sidecar.
            load_object(&path)
        };
        let Some(v) = value else {
            // Optional + missing — not counted toward pass or fail.
            continue;
        };
        match validate(&schema, &v) {
            Ok(()) => passed += 1,
            Err(msg) => failed.push((sidecar.schema_stem.to_string(), msg)),
        }
    }
    (passed, failed)
}

/// Layer 1 — empirically prove a real emit produces 8 schema-conformant
/// sidecars. Sets `ECAA_VALIDATE_ON_EMIT=schema_only` so the test runs
/// against the pure-Rust schema gate (no external Python deps).
#[tokio::test]
#[serial]
async fn real_emit_passes_all_8_schemas() {
    // The env var is informational here — the actual schema-validation
    // work happens in this test. The variable is set so any future
    // emit-time schema gate can read it without re-routing through a
    // different mode.
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "schema_only");

    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .expect("emit_with_conversation_log must succeed");

    let (passed, failed) = validate_all_eight(dir.path());

    assert!(
        failed.is_empty(),
        "schema validation failures after real emit:\n{}",
        failed
            .iter()
            .map(|(stem, msg)| format!("[{stem}]\n{msg}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    assert_eq!(
        passed, 8,
        "expected all 8 ECAA subgraph schemas to pass after a real emit; got {passed}"
    );
}

/// Layer 1 — after a real emit the `audit-proof-report.json` invariants
/// must all be `pass` or `warn` (never `fail`). `unverified` is a known
/// state on a fresh emit (no harness has run yet) and is treated as
/// acceptable here — the contract is "no Fail verdicts on a clean emit".
#[tokio::test]
#[serial]
async fn real_emit_audit_proof_invariants_pass_or_warn() {
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "schema_only");

    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .expect("emit_with_conversation_log must succeed");

    let report_path = dir.path().join("runtime").join("audit-proof-report.json");
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(&report_path)
            .unwrap_or_else(|e| panic!("read {}: {}", report_path.display(), e)),
    )
    .expect("audit-proof-report.json must be valid JSON");

    let verdicts = report
        .get("verdicts")
        .and_then(|v| v.as_array())
        .expect("audit-proof-report.json must have a verdicts array");
    assert_eq!(
        verdicts.len(),
        6,
        "expected 6 invariant verdicts in audit-proof-report.json"
    );

    let mut offenders: Vec<String> = Vec::new();
    for v in verdicts {
        let id = v.get("id").and_then(|i| i.as_str()).unwrap_or("<unknown>");
        let status = v
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("<missing>");
        // snake_case serialization of `InvariantStatus`.
        match status {
            "pass" | "warn" | "unverified" => {}
            "fail" => offenders.push(format!("{id} = fail: {v}")),
            other => offenders.push(format!("{id} = unknown status `{other}`: {v}")),
        }
    }
    assert!(
        offenders.is_empty(),
        "audit-proof verdicts must be pass/warn/unverified after a real emit:\n{}",
        offenders.join("\n")
    );
}

// `keep_output` entry point used by `scripts/refresh-real-fixture.sh`.
// Marked `#[ignore]` so it never runs under default `cargo test`.
// `cargo test -- --ignored emit_to_kept_dir_for_fixture_refresh` performs
// the emit + schema validation and prints the kept directory path on
// stdout. The script then copies it to the staging fixture location.
#[tokio::test]
#[serial]
#[ignore = "fixture-refresh helper; opt-in via --ignored"]
async fn emit_to_kept_dir_for_fixture_refresh() {
    std::env::set_var("ECAA_VALIDATE_ON_EMIT", "schema_only");

    // The output dir lives under the system temp root but is NOT auto-cleaned —
    // `keep()` consumes the TempDir guard. The refresh script picks up the
    // printed path and copies the package into the conformance fixture tree.
    let dir = tempdir().unwrap().keep();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, &dir, &config_dir())
        .await
        .expect("emit_with_conversation_log must succeed");

    // Print the path BEFORE validation so a failing run still leaves a
    // diagnosable artifact on disk.
    println!("KEPT_PACKAGE_DIR={}", dir.display());

    let (passed, failed) = validate_all_eight(&dir);
    assert!(failed.is_empty(), "validation failures: {failed:?}");
    assert_eq!(passed, 8, "expected 8 passes; got {passed}");
}
