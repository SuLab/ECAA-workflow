//! Tier F property tests for F14: every LLM-originated proposal is
//! validated against its JSON Schema before any state transition.
//!
//! The closed-vocabulary invariant is necessary but not sufficient:
//! even a registered tool must come with a schema entry the
//! dispatcher can use to validate args. This property pins the
//! consistency between the `Tool` enum and the static
//! `tool_schemas()` registry — every variant has a corresponding
//! schema with the same `name` field, and every schema names a
//! registered variant.
//!
//! Side-call proposers (auto-title, remediation_proposer) produce
//! structured output that is JSON-Schema-validated by the dedicated
//! deterministic endpoints in `server::chat_routes`; that path is
//! covered by their own per-endpoint tests. This property focuses
//! on the closed-vocabulary half of F14.

use scripps_workflow_conversation::tool_schemas;
use scripps_workflow_conversation::tools::Tool;

#[test]
fn every_tool_variant_has_a_schema_entry() {
    let variants = Tool::all_variants_for_tests();
    let variant_names: std::collections::BTreeSet<&'static str> =
        variants.iter().map(|t| t.name()).collect();

    let schemas = tool_schemas();
    let schema_names: std::collections::BTreeSet<String> = schemas
        .iter()
        .filter_map(|s| {
            s.get("name")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
        })
        .collect();

    // Every Tool variant must have a schema entry.
    for v in &variant_names {
        assert!(
            schema_names.contains(*v),
            "Tool variant `{}` has no entry in tool_schemas() — \
             dispatcher cannot validate args, F14 contract broken",
            v
        );
    }

    // Every schema entry must correspond to a registered Tool
    // variant (no orphan schemas pretending to be tools).
    for s in &schema_names {
        assert!(
            variant_names.contains(s.as_str()),
            "tool schema `{}` does not map to a Tool variant — \
             stale schema entry, F14 contract broken",
            s
        );
    }
}

#[test]
fn every_schema_has_an_input_schema_object() {
    let schemas = tool_schemas();
    for s in &schemas {
        let name = s
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("<unnamed>");
        let input_schema = s
            .get("input_schema")
            .unwrap_or_else(|| panic!("tool `{}` schema missing input_schema field", name));
        assert!(
            input_schema.is_object(),
            "tool `{}` input_schema must be a JSON object; F14 contract \
             needs an object so the dispatcher can validate args",
            name
        );
        // additionalProperties false is injected on every object-typed
        // sub-schema before emission (Anthropic 2026-05 strict-schema
        // requirement). Mirror the same check here so we catch a
        // future raw_tool_schemas entry that bypasses the injection.
        let ap = input_schema.get("additionalProperties");
        assert_eq!(
            ap,
            Some(&serde_json::Value::Bool(false)),
            "tool `{}` input_schema must declare additionalProperties=false \
             (Anthropic strict-schema requirement)",
            name
        );
    }
}
