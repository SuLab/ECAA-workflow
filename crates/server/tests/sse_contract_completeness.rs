//! Contract gate: every Rust `SsePayload` variant tag must appear in the
//! TypeScript SSE union and handler map.

use std::fs;

fn rust_variant_tags() -> Vec<&'static str> {
    vec![
        "tool_call_started",
        "tool_call_finished",
        "assistant_token_delta",
        "state_advanced",
        "harness_progress",
        "infra_error",
        "package_amended",
        "task_completed_reviewable",
        "harness_sizing_pilot_started",
        "harness_sizing_pilot_complete",
        "harness_sizing_pilot_skipped",
        "harness_stall_detected",
        "harness_resize_recommended",
        "harness_version_diff",
        "resync_required",
        "dashboard_summary_failed",
        "turn_appended",
        "harness_executor_selected",
        "harness_progress_health",
        "harness_orphans_reaped",
        "harness_heartbeat_stalled",
        "proposal_received",
        "proposal_gate_advanced",
        "proposal_promoted",
        "proposal_rejected",
    ]
}

#[test]
fn every_sse_variant_has_a_ts_union_member() {
    let ts_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ui/src/api/chatStream.ts");
    let ts = fs::read_to_string(&ts_path)
        .unwrap_or_else(|e| panic!("read {}: {}", ts_path.display(), e));

    let mut missing = Vec::new();
    for tag in rust_variant_tags() {
        let single = format!("type: '{tag}'");
        let double = format!("type: \"{tag}\"");
        if !ts.contains(&single) && !ts.contains(&double) {
            missing.push(tag);
        }
    }
    assert!(
        missing.is_empty(),
        "SSE variants missing from ui/src/api/chatStream.ts: {missing:?}"
    );
}

#[test]
fn every_sse_variant_has_a_handler() {
    let ts_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../ui/src/hooks/useSseChatEvents.ts");
    let ts = fs::read_to_string(&ts_path)
        .unwrap_or_else(|e| panic!("read {}: {}", ts_path.display(), e));

    let mut missing = Vec::new();
    for tag in rust_variant_tags() {
        if !ts.contains(&format!("{tag}:")) {
            missing.push(tag);
        }
    }
    assert!(
        missing.is_empty(),
        "SSE variants missing from useSseChatEvents.ts HANDLERS map: {missing:?}"
    );
}
