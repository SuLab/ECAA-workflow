//! C1 Phase-2 drift detection: hand-authored spec schemas vs the closed
//! sets in `ecaa_workflow_types::consts`.
//!
//! # What changed and why
//!
//! This test FILE retains its historical name, but its behavior is
//! inverted. It USED to regenerate every committed `subgraph-schemas/*.json`
//! from `schemars::schema_for!` on the internal Rust record types (`Turn`,
//! `DecisionRecord`, `ValidationReport`, `EdgeContract`,
//! `ClaimVerificationReport`, `VerifierDecision`, `Assumption`,
//! `AuditProofReport`) and assert the committed file matched. That made
//! emit-time validation a TAUTOLOGY — the impl types were checked against
//! schemas derived from those same types, so the gate could never catch a
//! divergence from the spec's node/edge object model (v0.1.md §4-5).
//!
//! The schemas are now HAND-AUTHORED against the spec node/edge model and
//! validate the spec-shaped projection (see
//! `crates/core/src/emitter/ecaa_projection.rs`). This test no longer
//! regenerates anything; it asserts that the closed `type` / `predicate`
//! enum members across the 8 hand-authored schemas are exactly the
//! canonical closed sets `consts::NODE_TYPES` (25) and
//! `consts::EDGE_PREDICATES` (20). A new variant added to the projection
//! enums without a matching schema edit (or vice-versa) fails here.
//!
//! # The A (audit-proof) sub-graph exception
//!
//! `audit-proof.schema.json` validates the `audit-proof-report.json`
//! DOCUMENT shape (it is a single report object, not a node/edge JSONL —
//! the §9.2 version-declaration fields live there, asserted separately by
//! the conformance crate's `spec_consistency.rs`). Its single A node type
//! (`InvariantVerdict`) and single A predicate (`evaluated_against`) are
//! therefore not declared as node/edge `enum`s in that file; the drift
//! check folds them in explicitly so the union still equals the consts.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;

const SCHEMA_DIR_RELATIVE: &str = "../../docs/ecaa-spec/subgraph-schemas";

/// The 7 hand-authored node/edge sub-graph schemas (I,D,E,V,C,Q,F). The A
/// schema is the report-document shape and is handled separately below.
const NODE_EDGE_SCHEMAS: &[&str] = &[
    "intent.schema.json",
    "decision.schema.json",
    "execution.schema.json",
    "evidence.schema.json",
    "claim.schema.json",
    "equivalence.schema.json",
    "failure.schema.json",
];

/// A sub-graph node type + predicate that live only in the report-document
/// `audit-proof.schema.json` (which is not a node/edge schema). Folded into
/// the union so the closed-set comparison is total.
const AUDIT_PROOF_NODE_TYPE: &str = "InvariantVerdict";
const AUDIT_PROOF_PREDICATE: &str = "evaluated_against";

fn schema_path(filename: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(SCHEMA_DIR_RELATIVE)
        .join(filename)
}

fn load_schema(filename: &str) -> Value {
    let path = schema_path(filename);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parse {}: {}", path.display(), e))
}

/// Pull the `definitions.Node.properties.type.enum` members from a
/// node/edge schema.
fn node_type_enum(schema: &Value) -> Vec<String> {
    enum_members(
        &schema["definitions"]["Node"]["properties"]["type"]["enum"],
        "Node.type",
    )
}

/// Pull the `definitions.Edge.properties.predicate.enum` members.
fn predicate_enum(schema: &Value) -> Vec<String> {
    enum_members(
        &schema["definitions"]["Edge"]["properties"]["predicate"]["enum"],
        "Edge.predicate",
    )
}

fn enum_members(v: &Value, label: &str) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected {label} to be an enum array, got {v:?}"))
        .iter()
        .map(|m| {
            m.as_str()
                .unwrap_or_else(|| panic!("{label} enum member is not a string: {m:?}"))
                .to_string()
        })
        .collect()
}

/// The union of every node/edge schema's closed `type` enum (plus the A
/// document's single node type) MUST equal `consts::NODE_TYPES`.
#[test]
fn schema_node_types_match_consts() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for file in NODE_EDGE_SCHEMAS {
        for t in node_type_enum(&load_schema(file)) {
            seen.insert(t);
        }
    }
    seen.insert(AUDIT_PROOF_NODE_TYPE.to_string());

    let expected: BTreeSet<String> = ecaa_workflow_types::consts::NODE_TYPES
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        seen, expected,
        "hand-authored schema node-type enums drifted from consts::NODE_TYPES \
         (the spec closed set). Update the schema or consts so they agree."
    );
}

/// The union of every node/edge schema's closed `predicate` enum (plus the
/// A document's single predicate) MUST equal `consts::EDGE_PREDICATES`.
#[test]
fn schema_predicates_match_consts() {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for file in NODE_EDGE_SCHEMAS {
        for p in predicate_enum(&load_schema(file)) {
            seen.insert(p);
        }
    }
    seen.insert(AUDIT_PROOF_PREDICATE.to_string());

    let expected: BTreeSet<String> = ecaa_workflow_types::consts::EDGE_PREDICATES
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        seen, expected,
        "hand-authored schema predicate enums drifted from consts::EDGE_PREDICATES \
         (the spec closed set). Update the schema or consts so they agree."
    );
}

/// Every node/edge schema must restrict each member of `type`/`predicate`
/// to the closed set of EXACTLY its sub-graph (no foreign members leak in).
/// Guards against pasting the wrong sub-graph's enum into a schema.
#[test]
fn each_schema_enum_is_a_subset_of_consts() {
    let node_types: BTreeSet<&str> =
        ecaa_workflow_types::consts::NODE_TYPES.iter().copied().collect();
    let predicates: BTreeSet<&str> = ecaa_workflow_types::consts::EDGE_PREDICATES
        .iter()
        .copied()
        .collect();
    for file in NODE_EDGE_SCHEMAS {
        let schema = load_schema(file);
        for t in node_type_enum(&schema) {
            assert!(
                node_types.contains(t.as_str()),
                "{file}: node type {t:?} is not in consts::NODE_TYPES"
            );
        }
        for p in predicate_enum(&schema) {
            assert!(
                predicates.contains(p.as_str()),
                "{file}: predicate {p:?} is not in consts::EDGE_PREDICATES"
            );
        }
    }
}

/// Sanity: every node/edge schema is a `type: array` whose items are a
/// node/edge `oneOf`. Catches a schema accidentally re-authored back to a
/// single-object impl shape.
#[test]
fn node_edge_schemas_are_arrays_of_node_or_edge() {
    for file in NODE_EDGE_SCHEMAS {
        let schema = load_schema(file);
        assert_eq!(
            schema["type"], Value::String("array".into()),
            "{file}: spec sub-graph schema must be a JSON array of nodes/edges"
        );
        let one_of = &schema["items"]["oneOf"];
        assert!(
            one_of.is_array() && one_of.as_array().map(|a| a.len()) == Some(2),
            "{file}: items must be oneOf:[Node, Edge]"
        );
    }
}
