//! M1/M2 conformance — authority + invocation provenance invariants.
//!
//! Pins the mechanically-checkable guarantees the provenance story
//! depends on so a future gate change can't silently regress them.

use ecaa_workflow_core::decision_log::{
    DecisionActor, DecisionAuthority, DecisionRecord, DecisionType,
};

/// M1 — the authority field exists, is `#[serde(default)]`, and a legacy
/// record (no authority key) loads as Conversational. Pins the on-disk
/// back-compat invariant that lets old decisions.jsonl keep loading.
#[test]
fn legacy_decision_record_loads_authority_as_conversational() {
    let legacy = r#"{
        "timestamp": "2026-05-13T18:00:00Z",
        "session_id": "s",
        "decision": {"kind": "unblock"},
        "actor": "sme"
    }"#;
    let rec: DecisionRecord = serde_json::from_str(legacy).unwrap();
    assert_eq!(rec.authority, DecisionAuthority::Conversational);
}

/// M1 — authority serializes as the snake_case wire tag the UI binding
/// and any RO-Crate consumer expects.
#[test]
fn authority_wire_tags_are_snake_case() {
    let mut rec = DecisionRecord::new("s", DecisionType::Unblock, DecisionActor::Sme, None);
    rec.authority = DecisionAuthority::SchemaValidated;
    let v: serde_json::Value = serde_json::to_value(&rec).unwrap();
    assert_eq!(v["authority"], "schema_validated");
    rec.authority = DecisionAuthority::Conversational;
    let v: serde_json::Value = serde_json::to_value(&rec).unwrap();
    assert_eq!(v["authority"], "conversational");
}

/// M2 — the validated-invocation provenance object binds the audit
/// fields a reviewer filters on (atom id, port-typed-input satisfaction,
/// sandbox profile) and round-trips through JSONL serialization.
#[test]
fn invocation_record_binds_audit_fields_and_round_trips() {
    use ecaa_workflow_core::atom::{SafetyPolicy, SandboxRequirement};
    use ecaa_workflow_harness::invocation_log::InvocationRecord;

    let mut sp = SafetyPolicy::default();
    sp.sandbox = SandboxRequirement::ProcessIsolation;
    let rec = InvocationRecord::new(
        "qc_preprocessing",
        Some("quality_control"),
        1,
        "run-abc",
        "2026-06-02T00:00:00Z",
        &["data_acquisition".to_string()],
        true,
        &sp,
        Some("ecaa/bio-min:latest"),
    );
    assert!(rec.sandbox_required, "process_isolation sets sandbox_required");
    assert!(rec.port_typed_inputs_satisfied);

    let line = serde_json::to_string(&rec).unwrap();
    let back: InvocationRecord = serde_json::from_str(&line).unwrap();
    assert_eq!(back, rec);
    assert_eq!(back.atom_id.as_deref(), Some("quality_control"));
}
