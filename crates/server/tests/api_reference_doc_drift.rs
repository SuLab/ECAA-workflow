//! Doc-as-contract gate for `docs/api-reference.md`.
//!
//! Doc/code drift in HTTP wire shapes (e.g. `docs/api-reference.md`
//! showing `POST /turn` body as `{ "text": "..." }` while
//! `SendTurnRequest.message` deserializes from `message`) is easy for a
//! contributor to copy into the UI's optimistic-append fetcher and only
//! surfaces when an integration test runs end-to-end. This test pins
//! the most-visible request shapes by extracting `json` code blocks
//! from the doc and checking field names against the live wire types.
//!
//! ## Scope
//!
//! The test reads `docs/api-reference.md` once, locates the
//! request-body bullets / code blocks for these high-traffic
//! endpoints, and asserts the expected field names appear inside:
//!
//! * `POST /turn` — `SendTurnRequest.message` (NOT `text`).
//! * `POST /confirm|/reject|/unblock|/branch` — optional `rationale`
//! field on `CheckpointDecisionRequest`.
//! * `POST /start-execution` — `agent_path` + `max_iterations` on
//! `StartExecutionRequest`.
//! * `POST /sme-selection` — `selection` + `rationale` shape.
//!
//! The assertions are intentionally narrow — substring-matches inside
//! a regex-located code block — because that's the smallest sufficient
//! gate against the `text` → `message` style of drift. Tightening to
//! full JSON parsing + serde round-trip is possible but would burn
//! cycles on doc-prose churn without catching meaningful drifts the
//! current shape misses.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR for this test = `crates/server`. Ascend two
    // levels to reach the workspace root where `docs/` lives.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("canonicalize repo root")
}

fn read_api_reference() -> String {
    let path = repo_root().join("docs").join("api-reference.md");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Find the section of the doc that documents `endpoint_anchor` and
/// return it as a substring slice up to (but not including) the next
/// `### ` heading. Used to scope assertions per endpoint instead of
/// matching the entire doc — that prevents an accidental `message`
/// mention in a future endpoint's example from silently masking a
/// regression in the `POST /turn` body.
fn section_for(doc: &str, endpoint_anchor: &str) -> String {
    let start = doc
        .find(endpoint_anchor)
        .unwrap_or_else(|| panic!("api-reference.md missing anchor: {endpoint_anchor}"));
    let tail = &doc[start..];
    // The next `### ` (heading) terminates the section. If we run off
    // the end of the doc, the rest of the file is the section.
    let end_relative = tail[endpoint_anchor.len()..]
        .find("\n### ")
        .map(|pos| pos + endpoint_anchor.len())
        .unwrap_or(tail.len());
    tail[..end_relative].to_string()
}

#[test]
#[ignore = "docs/api-reference.md not in OSS repo"]
fn api_reference_request_shapes_match_wire_types() {
    let doc = read_api_reference();

    // ── POST /turn — SendTurnRequest.message ──────────────────────
    let turn_section = section_for(&doc, "### `POST /api/chat/session/:id/turn`");
    assert!(
        turn_section.contains("\"message\""),
        "\n`docs/api-reference.md` POST /turn section is missing the \
         `\"message\"` field name. The wire type `SendTurnRequest` in \
         `crates/server/src/chat_routes/wire_types.rs` deserializes from \
         `message`, NOT `text`. Update the request-body example.\n\n\
         Section contents:\n{turn_section}\n"
    );
    assert!(
        !turn_section.contains("\"text\":") && !turn_section.contains("\"text\" "),
        "\n`docs/api-reference.md` POST /turn section still documents \
         a `text` field. The actual wire type is `message`. Replace \
         the example with `{{ \"message\": \"...\" }}`.\n\nSection contents:\n{turn_section}\n"
    );

    // ── POST /confirm|/reject|/unblock — CheckpointDecisionRequest.rationale ─
    let confirm_section = section_for(
        &doc,
        "### `POST /api/chat/session/:id/confirm`, `/reject`, `/unblock`",
    );
    assert!(
        confirm_section.contains("rationale"),
        "\n`docs/api-reference.md` confirm/reject/unblock section \
         is missing the `rationale` field. `CheckpointDecisionRequest` \
         in `wire_types.rs` accepts `rationale` as the optional body.\n\n\
         Section contents:\n{confirm_section}\n"
    );

    // ── POST /branch — CheckpointDecisionRequest.rationale ────────
    let branch_section = section_for(&doc, "### `POST /api/chat/session/:id/branch`");
    assert!(
        branch_section.contains("rationale"),
        "\n`docs/api-reference.md` POST /branch section is missing the \
         `rationale` field. The endpoint accepts the same \
         `CheckpointDecisionRequest` body as /confirm.\n\n\
         Section contents:\n{branch_section}\n"
    );

    // ── POST /start-execution — StartExecutionRequest ─────────────
    let exec_section = section_for(&doc, "### `POST /api/chat/session/:id/start-execution`");
    assert!(
        exec_section.contains("agent_path") && exec_section.contains("max_iterations"),
        "\n`docs/api-reference.md` start-execution section is missing \
         `agent_path` and/or `max_iterations` — the two optional fields \
         on `StartExecutionRequest` in `wire_types.rs`.\n\n\
         Section contents:\n{exec_section}\n"
    );

    // ── POST /sme-selection — selection + rationale ───────────────
    let sme_section = section_for(
        &doc,
        "### `POST /api/chat/session/:id/task/:task_id/sme-selection`",
    );
    assert!(
        sme_section.contains("selection") && sme_section.contains("rationale"),
        "\n`docs/api-reference.md` sme-selection section is missing \
         `selection` and/or `rationale`.\n\n\
         Section contents:\n{sme_section}\n"
    );
}

/// Additional response-shape coverage. The original
/// test pinned REQUEST bodies; this one pins RESPONSE bodies for the
/// endpoints whose docs drifted the most during the 2026-05 audit.
///
/// Each assertion is a substring match scoped to the per-endpoint
/// section; a future drift (e.g. someone restoring the bogus
/// `child_session_id` field) lights up the relevant assertion.
#[test]
#[ignore = "docs/api-reference.md not in OSS repo"]
fn api_reference_response_shapes_match_wire_types() {
    let doc = read_api_reference();

    // ── GET /state — SessionStateSnapshot has 9 fields; doc must
    // name the load-bearing ones (session_id, blocked_tasks,
    // parent_session_id, last_activity, title).
    let state_section = section_for(&doc, "### `GET /api/chat/session/:id/state`");
    for field in [
        "session_id",
        "blocked_tasks",
        "parent_session_id",
        "last_activity",
        "title",
    ] {
        assert!(
            state_section.contains(field),
            "\n`docs/api-reference.md` GET /state section is missing \
             SessionStateSnapshot field `{field}`. Wire type: \
             `crates/server/src/chat_routes/wire_types.rs::SessionStateSnapshot`.\n\n\
             Section contents:\n{state_section}\n"
        );
    }

    // ── GET /transcript — bare-array shape (NOT `{ turns: [...] }`)
    let transcript_section = section_for(&doc, "### `GET /api/chat/session/:id/transcript`");
    assert!(
        !transcript_section.contains("\"turns\""),
        "\n`docs/api-reference.md` GET /transcript section still claims \
         the response is wrapped as `{{ \"turns\": [...] }}`. The handler \
         returns `session.conversation.clone()` — a bare JSON array. \
         Update the doc.\n\nSection contents:\n{transcript_section}\n"
    );
    assert!(
        transcript_section.contains("bare JSON array") || transcript_section.contains("bare array"),
        "\n`docs/api-reference.md` GET /transcript section must call out \
         the bare-array response shape so client authors don't wrap \
         their decode.\n\nSection contents:\n{transcript_section}\n"
    );

    // ── POST /branch — `branched_session_id` (NOT `child_session_id`)
    let branch_section = section_for(&doc, "### `POST /api/chat/session/:id/branch`");
    assert!(
        branch_section.contains("branched_session_id"),
        "\n`docs/api-reference.md` POST /branch section must document \
         the response field as `branched_session_id`. The handler in \
         `crates/server/src/chat_routes/branches.rs::branch_session_inner` \
         returns `{{ \"branched_session_id\": <uuid> }}`.\n\n\
         Section contents:\n{branch_section}\n"
    );
    assert!(
        !branch_section.contains("\"child_session_id\""),
        "\n`docs/api-reference.md` POST /branch section still mentions \
         the non-existent `child_session_id` field. The actual field is \
         `branched_session_id`.\n\nSection contents:\n{branch_section}\n"
    );

    // ── GET /sessions?parent=… — bare array, not `{ children: [...] }`
    let sessions_section = section_for(&doc, "### `GET /api/chat/sessions?parent=");
    assert!(
        !sessions_section.contains("\"children\""),
        "\n`docs/api-reference.md` GET /sessions?parent section still \
         claims the response is `{{ \"children\": [...] }}`. \
         `list_sessions_by_parent` returns a bare JSON array of \
         summaries.\n\nSection contents:\n{sessions_section}\n"
    );

    // ── GET /execution — ExecutionStatusResponse has 9 fields; doc
    // must name the load-bearing ones.
    let exec_section = section_for(&doc, "### `GET /api/chat/session/:id/execution`");
    for field in [
        "pgid",
        "package_dir",
        "agent_command",
        "status",
        "paused_at",
        "stop_requested_at",
    ] {
        assert!(
            exec_section.contains(field),
            "\n`docs/api-reference.md` GET /execution section is missing \
             ExecutionStatusResponse field `{field}`. Wire type: \
             `wire_types.rs::ExecutionStatusResponse`.\n\n\
             Section contents:\n{exec_section}\n"
        );
    }
}

/// Confirm the load-bearing wire type really does deserialize from
/// `message` and would fail on `text`. This pins the assumption the
/// doc-drift test above is built on — if the wire type ever swaps
/// back to `text`, this test goes red FIRST and points at the right
/// remediation (update the wire type, then the doc).
#[test]
fn send_turn_request_field_is_message_not_text() {
    use ecaa_workflow_server::chat_routes::SendTurnRequest;

    // `message` (correct) — must deserialize.
    let good = r#"{ "message": "hi" }"#;
    let parsed: Result<SendTurnRequest, _> = serde_json::from_str(good);
    assert!(
        parsed.is_ok(),
        "`SendTurnRequest` no longer accepts `{{ \"message\": \"...\" }}` \
         — wire-type drift. error: {:?}",
        parsed.err()
    );

    // `text` (incorrect / pre-rename) — must fail because `message`
    // has no default.
    let bad = r#"{ "text": "hi" }"#;
    let parsed: Result<SendTurnRequest, _> = serde_json::from_str(bad);
    assert!(
        parsed.is_err(),
        "`SendTurnRequest` is accepting `{{ \"text\": \"...\" }}` — \
         either an alias was added or `message` gained a #[serde(default)]. \
         If alias is intentional, drop this regression guard and update \
         the doc-drift test to allow either spelling."
    );
}
