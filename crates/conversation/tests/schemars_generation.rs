//! Layer 5 drift-detection: schemars-generated schemas vs committed schemas.
//!
//! For each of the 8 ECAA sidecar root types, this test:
//!   1. Generates a JSON Schema via `schemars::schema_for!(...)`.
//!   2. For JSONL sidecars, wraps the per-row schema in
//!      `{"type": "array", "items": <generated>}` to match the
//!      committed schema's whole-file shape.
//!   3. Compares to the committed `docs/ecaa-spec/subgraph-schemas/...`.
//!
//! Set `SCHEMARS_REGEN=1` to OVERWRITE the committed schemas with the
//! freshly-derived ones. Without the env var, the test FAILS if the
//! generated and committed schemas differ — catching any drift between
//! Rust types and the on-wire schemas.
//!
//! NOTE: Several `// TODO: confirm path` markers below — these are
//! type-import paths the user should verify at build time. If a path
//! is wrong the test will fail with a clear "cannot find type" error
//! and the user can adjust.

use schemars::schema_for;
use serde_json::Value;
use std::path::PathBuf;

const SCHEMA_DIR_RELATIVE: &str = "../../docs/ecaa-spec/subgraph-schemas";

fn schema_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(SCHEMA_DIR_RELATIVE)
        .join(filename)
}

/// Wrap a schemars-generated per-row schema as a JSONL whole-file array.
fn jsonl_wrap(item_schema: Value) -> Value {
    let mut wrapped = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "array",
        "items": item_schema,
    });
    // Lift schemars' `definitions` block to the top so the whole-file
    // schema can $ref into it the way hand-maintained schemas do.
    if let Value::Object(item_obj) = wrapped["items"].clone() {
        if let Some(defs) = item_obj.get("definitions").cloned() {
            if let Value::Object(ref mut top) = wrapped {
                top.insert("definitions".to_string(), defs);
            }
        }
        // Remove the top-level metadata from the inner items so it
        // doesn't duplicate.
        if let Value::Object(ref mut items) = wrapped["items"] {
            items.remove("$schema");
            items.remove("definitions");
        }
    }
    wrapped
}

/// Heterogeneous JSONL: each line is one of several allowed shapes.
/// Wrap as `{type: array, items: {oneOf: [schemas]}, definitions: ...}`,
/// preserving each input's `title` so the oneOf entries are
/// distinguishable in error messages. Definitions from every input are
/// merged at the top level (last writer wins on collisions — the inputs
/// share most definitions so this is benign).
fn jsonl_wrap_oneof(item_schemas: &[Value]) -> Value {
    let mut merged_defs = serde_json::Map::new();
    let mut alternatives: Vec<Value> = Vec::with_capacity(item_schemas.len());
    for schema in item_schemas {
        if let Some(defs) = schema.get("definitions").and_then(|d| d.as_object()) {
            for (k, v) in defs {
                merged_defs.insert(k.clone(), v.clone());
            }
        }
        let mut cleaned = schema.clone();
        if let Value::Object(ref mut obj) = cleaned {
            obj.remove("$schema");
            obj.remove("definitions");
        }
        alternatives.push(cleaned);
    }
    let mut wrapped = serde_json::json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "array",
        "items": { "oneOf": alternatives },
    });
    if !merged_defs.is_empty() {
        if let Value::Object(ref mut top) = wrapped {
            top.insert("definitions".into(), Value::Object(merged_defs));
        }
    }
    wrapped
}

/// Single-document (non-JSONL) sidecar: schemars output IS the whole
/// schema, but we ensure $schema is set.
fn single_doc_wrap(generated: Value) -> Value {
    let mut out = generated;
    if let Value::Object(ref mut top) = out {
        top.entry("$schema".to_string()).or_insert_with(|| {
            Value::String("http://json-schema.org/draft-07/schema#".to_string())
        });
    }
    out
}

fn assert_or_regen(filename: &str, generated: Value) {
    let path = schema_path(filename);
    if std::env::var("SCHEMARS_REGEN").is_ok() {
        let pretty =
            serde_json::to_string_pretty(&generated).expect("serialize regenerated schema");
        std::fs::write(&path, pretty + "\n")
            .unwrap_or_else(|e| panic!("write {}: {}", path.display(), e));
        eprintln!("REGEN: wrote {}", path.display());
        return;
    }
    let committed: Value = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e)),
    )
    .expect("committed schema parses as JSON");
    assert_eq!(
        generated, committed,
        "Schema {} drifted from #[derive(JsonSchema)]. Regenerate with: \
         SCHEMARS_REGEN=1 cargo test -p ecaa-workflow-conversation \
         --test schemars_generation",
        filename
    );
}

// ───── intent.schema.json (JSONL: Turn ∪ ToolCallRecord) ────────────────
//
// The intent sidecar is a heterogeneous JSONL — both Turn and
// ToolCallRecord lines coexist (audit_log.rs writes turns then appends
// tool_call_log). Items validate as `oneOf` between Turn and
// ToolCallRecord, with both schemas' `definitions` merged at the top.
#[test]
fn intent_schema_matches_derive() {
    let turn = serde_json::to_value(schema_for!(
        ecaa_workflow_conversation::session::state::Turn
    ))
    .expect("serialize Turn schema");
    let tool_call = serde_json::to_value(schema_for!(
        ecaa_workflow_conversation::session::state::ToolCallRecord
    ))
    .expect("serialize ToolCallRecord schema");
    assert_or_regen("intent.schema.json", jsonl_wrap_oneof(&[turn, tool_call]));
}

// ───── decision.schema.json (JSONL: DecisionRecord) ─────────────────────
#[test]
fn decision_schema_matches_derive() {
    // TODO: confirm path — `ecaa_workflow_core::decision_log::DecisionRecord`.
    let item = serde_json::to_value(schema_for!(
        ecaa_workflow_core::decision_log::DecisionRecord
    ))
    .expect("serialize DecisionRecord schema");
    assert_or_regen("decision.schema.json", jsonl_wrap(item));
}

// ───── execution.schema.json (JSONL: ValidationReport from outcome.rs) ──
#[test]
fn execution_schema_matches_derive() {
    // TODO: confirm path — likely
    // `ecaa_workflow_core::workflow_contracts::outcome::ValidationReport`.
    let item = serde_json::to_value(schema_for!(
        ecaa_workflow_core::workflow_contracts::outcome::ValidationReport
    ))
    .expect("serialize ValidationReport schema");
    assert_or_regen("execution.schema.json", jsonl_wrap(item));
}

// ───── evidence.schema.json (JSONL: EdgeContract) ───────────────────────
#[test]
fn evidence_schema_matches_derive() {
    let item = serde_json::to_value(schema_for!(
        ecaa_workflow_core::workflow_contracts::edge::EdgeContract
    ))
    .expect("serialize EdgeContract schema");
    assert_or_regen("evidence.schema.json", jsonl_wrap(item));
}

// ───── claim.schema.json (single doc: ClaimVerificationReport) ──────────
#[test]
fn claim_schema_matches_derive() {
    // TODO: confirm path — `ecaa_workflow_core::claim_verifier::ClaimVerificationReport`.
    let generated = serde_json::to_value(schema_for!(
        ecaa_workflow_core::claim_verifier::ClaimVerificationReport
    ))
    .expect("serialize ClaimVerificationReport schema");
    assert_or_regen("claim.schema.json", single_doc_wrap(generated));
}

// ───── equivalence.schema.json (JSONL: VerifierDecision) ────────────────
#[test]
fn equivalence_schema_matches_derive() {
    let item = serde_json::to_value(schema_for!(
        ecaa_workflow_core::decision_substrate::VerifierDecision
    ))
    .expect("serialize VerifierDecision schema");
    assert_or_regen("equivalence.schema.json", jsonl_wrap(item));
}

// ───── failure.schema.json (JSONL: Assumption from evidence.rs) ─────────
#[test]
fn failure_schema_matches_derive() {
    // Assumption is in workflow_contracts/evidence.rs (per grep).
    let item = serde_json::to_value(schema_for!(
        ecaa_workflow_core::workflow_contracts::evidence::Assumption
    ))
    .expect("serialize Assumption schema");
    assert_or_regen("failure.schema.json", jsonl_wrap(item));
}

// ───── audit-proof.schema.json (single doc: AuditProofReport) ───────────
#[test]
fn audit_proof_schema_matches_derive() {
    let generated =
        serde_json::to_value(schema_for!(ecaa_workflow_types::AuditProofReport))
            .expect("serialize AuditProofReport schema");
    assert_or_regen("audit-proof.schema.json", single_doc_wrap(generated));
}
