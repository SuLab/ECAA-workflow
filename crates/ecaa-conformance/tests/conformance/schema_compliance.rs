//! §8.1 schema-compliance bar, co-located in `ecaa-conformance`.
//!
//! A second implementer running ONLY `cargo test -p ecaa-workflow-conformance`
//! must be able to exercise the §8.1 schema-compliance contract without
//! reaching into the conversation crate's emit path (where
//! `validate_schemas_pure_rust` lives). The conformance crate does NOT depend
//! on `ecaa-workflow-conversation`, so this test re-derives the same bar from
//! the parts that ARE in scope:
//!
//!   * the 8 hand-authored JSON Schemas under
//!     `docs/ecaa-spec/subgraph-schemas/`,
//!   * `ecaa_workflow_core::audit_proof::loader::LoadedPackage` (loads an
//!     emitted package), and
//!   * `ecaa_workflow_core::emitter::ecaa_projection` (projects each loaded
//!     sub-graph into the spec node/edge JSON the schema validates),
//!   * `ecaa_workflow_core::schema_helpers::{compile_schema, validate_value}`.
//!
//! This is the same projection-then-validate discipline the emit-time
//! validator uses (see `crates/conversation/src/emit/validation.rs`), restated
//! against a committed package fixture so the bar is reachable from the
//! conformance crate alone.

use ecaa_workflow_core::audit_proof::loader::LoadedPackage;
use ecaa_workflow_core::emitter::ecaa_projection;
use ecaa_workflow_core::schema_helpers::{compile_schema, validate_value};
use serde_json::Value;
use std::path::PathBuf;

use super::_shacl_harness::{fixture_dir, repo_root};

/// The 7 node/edge sub-graphs validated by projecting the loaded package,
/// paired with their hand-authored schema file stem. The A audit-proof report
/// is validated separately as a report DOCUMENT (it is not a `LoadedPackage`
/// field). This mirrors `validation::sidecar_schemas()` letter mapping.
const SUBGRAPH_SCHEMAS: &[(char, &str)] = &[
    ('I', "intent"),
    ('D', "decision"),
    ('E', "execution"),
    ('V', "evidence"),
    ('C', "claim"),
    ('Q', "equivalence"),
    ('F', "failure"),
];

fn schema_path(stem: &str) -> PathBuf {
    repo_root()
        .join("docs")
        .join("ecaa-spec")
        .join("subgraph-schemas")
        .join(format!("{stem}.schema.json"))
}

/// Read + compile a §8.1 schema by file stem, then validate `instance` against
/// it. Returns the `validate_value` result so callers can assert pass/fail.
/// Keeps the `jsonschema::JSONSchema` type local (the conformance crate does
/// not depend on `jsonschema` directly).
fn validate_against_schema(
    stem: &str,
    instance: &Value,
    context: &str,
) -> anyhow::Result<()> {
    let path = schema_path(stem);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading schema {}: {e}", path.display()));
    let schema =
        compile_schema(&raw, stem).unwrap_or_else(|e| panic!("compiling {stem} schema: {e:#}"));
    validate_value(&schema, instance, context)
}

/// Positive bar: the committed `complete-package` fixture projects cleanly and
/// every projected sub-graph plus the audit-proof report document validates
/// against its §8.1 schema. This is the contract a second implementer's
/// package must satisfy.
#[test]
fn complete_package_passes_schema_compliance_bar() {
    let pkg_root = fixture_dir("complete-package");
    assert!(
        pkg_root.join("ro-crate-metadata.json").exists(),
        "complete-package fixture must exist at {}",
        pkg_root.display()
    );
    let pkg = LoadedPackage::from_root(&pkg_root)
        .unwrap_or_else(|e| panic!("loading complete-package fixture: {e:#}"));

    // 7 node/edge sub-graphs: project, then validate the array.
    for (letter, stem) in SUBGRAPH_SCHEMAS {
        let projection = ecaa_projection::project_subgraph(*letter, &pkg);
        let instance = Value::Array(projection);
        validate_against_schema(stem, &instance, &format!("subgraph {letter} ({stem})"))
            .unwrap_or_else(|e| panic!("subgraph {letter} failed §8.1 schema compliance: {e:#}"));
    }

    // A — audit-proof report DOCUMENT (validated whole, not projected).
    let report_path = pkg_root.join("runtime").join("audit-proof-report.json");
    let report_raw = std::fs::read_to_string(&report_path)
        .unwrap_or_else(|e| panic!("reading audit-proof-report.json: {e}"));
    let report: Value = serde_json::from_str(&report_raw).expect("audit-proof-report.json parse");
    validate_against_schema("audit-proof", &report, "audit-proof report document")
        .unwrap_or_else(|e| panic!("audit-proof report failed §8.1 schema compliance: {e:#}"));
}

/// Negative bar: a malformed sub-graph projection (a node with an out-of-spec
/// `type` and a non-prefixed `id`) must FAIL §8.1 schema validation. This
/// guards against a vacuous positive — proving the schema rejects bad input,
/// not just accepts everything. We synthesize the malformed instance rather
/// than corrupting a fixture so the test is hermetic.
#[test]
fn malformed_subgraph_fails_schema_compliance_bar() {
    // A node missing the required `V:` id prefix and carrying a `type` outside
    // the closed §5.4 enum. Either violation alone is sufficient; both ensure
    // the failure is unambiguous.
    let malformed = Value::Array(vec![serde_json::json!({
        "id": "not-a-valid-evidence-id",
        "type": "TotallyBogusNodeType",
        "props": {}
    })]);
    let result = validate_against_schema("evidence", &malformed, "malformed evidence subgraph");
    assert!(
        result.is_err(),
        "malformed evidence sub-graph must fail §8.1 schema validation, but it passed"
    );
}

/// Negative bar (whole-document): a malformed audit-proof report (missing the
/// required version-declaration shape) must FAIL the A schema. Complements the
/// projection negative case above for the report-document path.
#[test]
fn malformed_audit_proof_report_fails_schema_compliance_bar() {
    let malformed = serde_json::json!({ "not_a_report": true });
    let result = validate_against_schema("audit-proof", &malformed, "malformed audit-proof report");
    assert!(
        result.is_err(),
        "malformed audit-proof report must fail §8.1 schema validation, but it passed"
    );
}
