//! v3 P7 — backward-compat tests for `schema_version` IR fields.
//!
//! The promotion from `u32` to `semver::Version` is gated by the
//! `crate::migration::schema_version_serde` adapter, which accepts both
//! bare `u64` JSON values (legacy) and canonical SemVer strings on
//! read. Each test in this file feeds one shape into one IR type's
//! deserializer and asserts the resulting `schema_version` value.

use ecaa_workflow_core::workflow_contracts::workflow_intent::WorkflowIntent;
use semver::Version;

/// Legacy u32 → `<n>.0.0` SemVer.
#[test]
fn workflow_intent_deserializes_u32_legacy() {
    let legacy = r#"{
        "id": "s_legacy",
        "schema_version": 3,
        "goal": "test"
    }"#;
    let intent: WorkflowIntent = serde_json::from_str(legacy).expect("legacy u32 deserializes");
    assert_eq!(intent.schema_version, Version::new(3, 0, 0));
}

/// Modern SemVer string deserializes as-is.
#[test]
fn workflow_intent_deserializes_semver_string() {
    let modern = r#"{
        "id": "s_modern",
        "schema_version": "3.1.0",
        "goal": "test"
    }"#;
    let intent: WorkflowIntent = serde_json::from_str(modern).expect("semver string deserializes");
    assert_eq!(intent.schema_version, Version::new(3, 1, 0));
}

/// Outbound JSON always emits the canonical SemVer string form (no
/// regressing back to `u32`).
#[test]
fn workflow_intent_serializes_as_semver_string() {
    let intent = WorkflowIntent {
        id: "s_writeback".into(),
        schema_version: Version::new(2, 5, 7),
        goal: "test".into(),
        ..Default::default()
    };
    let v = serde_json::to_value(&intent).expect("serialize");
    let observed = v
        .get("schema_version")
        .and_then(|x| x.as_str())
        .expect("schema_version is a string");
    assert_eq!(observed, "2.5.7");
}

/// Default constructor matches `migration::current_workflow_intent_version()`.
#[test]
fn workflow_intent_default_schema_version_matches_canonical_constant() {
    let intent = WorkflowIntent::default();
    assert_eq!(
        intent.schema_version,
        ecaa_workflow_core::migration::current_workflow_intent_version()
    );
}

/// Pre-rights-bit ROCs that omit the field land at the default.
#[test]
fn workflow_intent_missing_field_uses_default() {
    let without_version = r#"{
        "id": "s_no_version",
        "goal": "test"
    }"#;
    let intent: WorkflowIntent =
        serde_json::from_str(without_version).expect("missing field deserializes");
    assert_eq!(
        intent.schema_version,
        ecaa_workflow_core::migration::current_workflow_intent_version()
    );
}
