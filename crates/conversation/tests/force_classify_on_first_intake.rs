//! Regression tests for the fix that forces
//! `classify_intake` on the first Intake turn via Anthropic
//! Messages `tool_choice`.
//!
//! Without the force, Sonnet 4.6 was drifting to conversational
//! acknowledgment and fabricating "backend classifier unavailable"
//! excuses.

use scripps_workflow_conversation::anthropic::{build_messages_payload, ToolChoice, TurnRequest};
use scripps_workflow_conversation::model_policy::ModelId;
use std::sync::Arc;

fn empty_request() -> TurnRequest {
    TurnRequest {
        system_prompt: vec![],
        conversation: Arc::new(vec![]),
        tool_schemas: vec![serde_json::json!({"name":"classify_intake"})],
        model: ModelId::Sonnet46,
        temperature: 0.7,
        max_tokens: 4096,
        tool_exchange: vec![],
        tool_choice: None,
    }
}

#[test]
fn payload_omits_tool_choice_when_none() {
    let req = empty_request();
    let payload = build_messages_payload(&req);
    assert!(
        payload.get("tool_choice").is_none(),
        "tool_choice should be absent when TurnRequest.tool_choice is None"
    );
}

#[test]
fn payload_carries_tool_choice_when_forced() {
    let mut req = empty_request();
    req.tool_choice = Some(ToolChoice::Tool("classify_intake".to_string()));
    let payload = build_messages_payload(&req);
    let choice = payload
        .get("tool_choice")
        .expect("tool_choice should be present");
    assert_eq!(choice["type"], "tool");
    assert_eq!(choice["name"], "classify_intake");
}

#[test]
fn tool_choice_serializes_with_minimal_shape() {
    // Anthropic Messages rejects extra keys on tool_choice; verify
    // the rendered JSON has exactly two fields and no nulls.
    let choice = ToolChoice::Tool("propose_summary_confirmation".to_string());
    let json = choice.to_anthropic_payload();
    let obj = json.as_object().expect("payload is an object");
    assert_eq!(
        obj.len(),
        2,
        "tool_choice object must have exactly type+name"
    );
    assert_eq!(obj["type"], "tool");
    assert_eq!(obj["name"], "propose_summary_confirmation");
}

#[test]
fn non_exhaustive_marker_prevents_match_explosion() {
    // The enum is `#[non_exhaustive]` so downstream consumers can't
    // exhaustively match against it and lock the workspace into a
    // SemVer-breaking change every time a new variant lands
    // (e.g. Anthropic adding `{"type":"any"}` support).
    let choice = ToolChoice::Tool("classify_intake".to_string());
    // The match below is intentionally explicit and includes the
    // catch-all so the codebase compiles when a future variant is
    // added without touching this test.
    match &choice {
        ToolChoice::Tool(name) => assert_eq!(name, "classify_intake"),
        // Catch-all required by `#[non_exhaustive]`; future variants
        // (e.g. ToolChoice::Any) will land here without breaking.
        _ => unreachable!("only Tool variant exists in v0.1"),
    }
}
