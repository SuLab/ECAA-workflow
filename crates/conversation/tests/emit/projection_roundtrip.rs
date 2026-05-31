//! C1 Phase-2 round-trip: project a real package → validate every
//! sub-graph against its hand-authored spec schema (positive), then mutate
//! one projected node's `type` to a value outside the closed set and prove
//! validation FAILS (negative).
//!
//! The negative test is the one that would have caught the original
//! tautology: when the schemas were derived from the impl types, no impl
//! value could ever produce an out-of-closed-set `type`, so the schema
//! could never reject one.

use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::emitter::ecaa_projection::project_subgraph;
use jsonschema::JSONSchema;
use serde_json::{json, Value};
use std::path::PathBuf;

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("ecaa-conformance/tests/fixtures/minimal-package")
        .canonicalize()
        .expect("minimal-package fixture must exist")
}

fn schema(filename: &str) -> JSONSchema {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/ecaa-spec/subgraph-schemas")
        .join(filename);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    let value: Value = serde_json::from_str(&raw).expect("schema parses");
    JSONSchema::compile(&value).expect("schema compiles")
}

/// (letter, schema filename) for the 7 node/edge sub-graphs (A is the
/// report document, exercised separately).
const SUBGRAPHS: &[(char, &str)] = &[
    ('I', "intent.schema.json"),
    ('D', "decision.schema.json"),
    ('E', "execution.schema.json"),
    ('V', "evidence.schema.json"),
    ('C', "claim.schema.json"),
    ('Q', "equivalence.schema.json"),
    ('F', "failure.schema.json"),
];

fn schema_errors(compiled: &JSONSchema, instance: &Value) -> Vec<String> {
    // Mirror the production validator's `collect_schema_errors`: consume the
    // borrowing error iterator inside a helper that returns owned strings, so
    // the iterator's borrow of `compiled`/`instance` never escapes the call.
    match compiled.validate(instance) {
        Ok(()) => Vec::new(),
        Err(errors) => errors.map(|e| e.to_string()).collect(),
    }
}

fn assert_valid(letter: char, filename: &str, projection: &[Value]) {
    let compiled = schema(filename);
    let instance = Value::Array(projection.to_vec());
    let msgs = schema_errors(&compiled, &instance);
    if !msgs.is_empty() {
        panic!(
            "sub-graph {letter} projection failed {filename}: {}\nprojection: {}",
            msgs.join("; "),
            serde_json::to_string_pretty(&instance).unwrap()
        );
    }
}

#[test]
fn positive_every_subgraph_projection_validates() {
    let pkg = LoadedPackage::from_root(&fixture_root()).expect("load minimal package");
    for (letter, filename) in SUBGRAPHS {
        let projection = project_subgraph(*letter, &pkg);
        assert_valid(*letter, filename, &projection);
    }
}

#[test]
fn intent_projection_has_required_node_types() {
    let pkg = LoadedPackage::from_root(&fixture_root()).expect("load minimal package");
    let i = project_subgraph('I', &pkg);
    let types: Vec<&str> = i
        .iter()
        .filter_map(|n| n.get("type").and_then(Value::as_str))
        .collect();
    // §5.1 cardinality: ≥1 Question, exactly one Modality, ≥1 ExpectedOutput.
    assert!(types.contains(&"Question"), "I must have a Question; got {types:?}");
    assert!(types.contains(&"Modality"), "I must have a Modality; got {types:?}");
    assert!(
        types.contains(&"ExpectedOutput"),
        "I must have an ExpectedOutput; got {types:?}"
    );
}

#[test]
fn execution_projection_emits_workflow_step() {
    let pkg = LoadedPackage::from_root(&fixture_root()).expect("load minimal package");
    let e = project_subgraph('E', &pkg);
    // The minimal fixture's proofs.jsonl carries one EdgeContract
    // (input_fastq → task_qc_001), which projects to a WorkflowStep node.
    assert!(
        e.iter().any(|n| n.get("type").and_then(Value::as_str) == Some("WorkflowStep")),
        "E projection must contain a WorkflowStep; got {e:?}"
    );
}

#[test]
fn negative_node_with_non_closed_type_fails_validation() {
    let pkg = LoadedPackage::from_root(&fixture_root()).expect("load minimal package");
    let mut i = project_subgraph('I', &pkg);
    assert!(!i.is_empty(), "intent projection should be non-empty");
    // Tamper: replace the first node's closed `type` with a value outside
    // the §5.1 closed set. This is exactly the corruption the old
    // schemars-derived schema could never reject (no impl value could
    // produce it). The hand-authored schema MUST reject it now.
    i[0]["type"] = json!("NotARealNodeType");
    let compiled = schema("intent.schema.json");
    let instance = Value::Array(i);
    assert!(
        compiled.validate(&instance).is_err(),
        "a node typed outside the closed §5.1 set MUST fail the hand-authored schema; \
         instance: {}",
        serde_json::to_string_pretty(&instance).unwrap()
    );
}

#[test]
fn negative_edge_with_non_closed_predicate_fails_validation() {
    // Build a minimal D sub-graph projection with a tampered predicate.
    let tampered = json!([
        {"id": "D:method_x", "type": "MethodChoice", "props": {"rationale": "thirty-plus characters of justification here."}},
        {"source_id": "D:method_x", "target_id": "D:deseq2", "predicate": "not_a_real_predicate"}
    ]);
    let compiled = schema("decision.schema.json");
    assert!(
        compiled.validate(&tampered).is_err(),
        "an edge with a predicate outside the closed §5.2 set MUST fail the schema"
    );
}

#[test]
fn positive_handauthored_decision_node_validates() {
    let valid = json!([
        {"id": "D:method_de", "type": "MethodChoice", "props": {"stage": "de", "rationale": "DESeq2 chosen per protocol; meets the 30-char minimum."}},
        {"source_id": "D:method_de", "target_id": "D:deseq2", "predicate": "chooses"}
    ]);
    let compiled = schema("decision.schema.json");
    assert!(
        compiled.validate(&valid).is_ok(),
        "a closed-set D node+edge must validate"
    );
}
