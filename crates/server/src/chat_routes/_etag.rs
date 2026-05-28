//! Optimistic-concurrency ETag helpers for high-impact mutation
//! endpoints (`confirm`, `reject`, `unblock`, `branch_session`).
//!
//! Problem: the UI and the LLM tool loop both observe session state
//! through `GET /api/chat/session/:id/state`. Two near-simultaneous
//! state mutations against the same session (SME clicks Confirm, LLM
//! runs `emit_package` from a now-stale view of `user_confirmed`,
//! amend lands while the SME's reject is in-flight, etc.) silently
//! win-last-write because neither caller declared what version of the
//! session it was acting on.
//!
//! Solution: derive a content-hash ETag from the session's
//! `state` discriminant + the transcript turn count, surface it on
//! GET responses, and require `If-Match: <etag>` on the four
//! high-impact mutation POSTs. A mismatch returns
//! `412 Precondition Failed` with the current ETag in the response
//! so the UI can refresh state and retry. Absence of `If-Match` is
//! tolerated on the server side (back-compat) but logged at
//! `info!(target: "etag_drift")` so we can track stragglers.
//!
//! ETag shape: `"<state_kind>.<turn_count>.<sha8>"` — `state_kind` is
//! the variant name; `turn_count` is `session.conversation.len()`;
//! `sha8` is the first 8 hex chars of `sha256(state_kind || \"|\" ||
//! turn_count)` so a downstream client treating the value as opaque
//! still sees something visually distinct between revisions and
//! cannot synthesize one without observing the session.
//!
//! The hash is deliberately shallow (not a full session JSON hash):
//! the four guarded endpoints care only about the state-machine
//! position + the conversation log length. A deeper hash would force
//! a recompute on every harness progress event (which mutates
//! `harness_events` but not the SME-decision-relevant fields) and
//! would push concurrent UI polls into spurious 412s.

use axum::http::{header::ETAG, HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use scripps_workflow_conversation::Session;
use sha2::Digest;

/// Compute the session ETag from its state and transcript length.
/// See module docs for the shape rationale.
pub fn etag_for_session(session: &Session) -> String {
    let state_kind = state_kind(&session.state);
    let turn_count = session.conversation.len();
    let mut h = sha2::Sha256::new();
    h.update(state_kind.as_bytes());
    h.update(b"|");
    h.update(turn_count.to_le_bytes());
    let digest = h.finalize();
    let short = hex::encode(&digest[..4]);
    format!("\"{state_kind}.{turn_count}.{short}\"")
}

/// Insert the session's current ETag into an outbound HeaderMap.
/// Used by GET handlers that surface session state so the UI can
/// later send the value back as `If-Match` on a mutation.
pub fn insert_etag(headers: &mut HeaderMap, session: &Session) {
    if let Ok(value) = HeaderValue::from_str(&etag_for_session(session)) {
        headers.insert(ETAG, value);
    }
}

/// Outcome of an `If-Match` precondition check on an inbound
/// mutation request. `Match` proceeds; `Mismatch` is returned to the
/// handler so it can emit `412 Precondition Failed`; `Absent` lets
/// back-compat clients (UI ones that haven't been updated yet) keep
/// working but logs a drift warning.
pub enum IfMatchOutcome {
    /// The client's ETag matched; proceed.
    Match,
    /// ETags differ; handler should return 412.
    Mismatch {
        /// Server's current ETag.
        server: String,
        /// ETag sent by the client.
        client: String,
    },
    /// No `If-Match` header present; permitted for back-compat.
    Absent,
}

/// Inspect `If-Match` on an inbound request and compare against the
/// session's current ETag. The endpoint name is included only for
/// the drift-log so a single dashboard can split-by-endpoint.
pub fn check_if_match(
    headers: &HeaderMap,
    session: &Session,
    endpoint: &'static str,
) -> IfMatchOutcome {
    let header = match headers.get(axum::http::header::IF_MATCH) {
        Some(v) => v,
        None => {
            tracing::info!(
                target: "etag_drift",
                endpoint,
                session_id = %session.id,
                "If-Match absent on high-impact mutation; permitting for back-compat"
            );
            return IfMatchOutcome::Absent;
        }
    };
    let client = match header.to_str() {
        Ok(s) => s.to_string(),
        Err(_) => {
            tracing::warn!(
                target: "etag_drift",
                endpoint,
                session_id = %session.id,
                "If-Match header is not valid UTF-8; treating as absent"
            );
            return IfMatchOutcome::Absent;
        }
    };
    let server = etag_for_session(session);
    // RFC 9110 §13.1.1: "*" matches any current representation.
    if client.trim() == "*" || client.trim() == server.trim() {
        IfMatchOutcome::Match
    } else {
        IfMatchOutcome::Mismatch { server, client }
    }
}

/// Render an `IfMatchOutcome::Mismatch` as the standard 412 response.
/// Body carries the server's current ETag so the UI can refresh and
/// retry without an extra GET round-trip.
pub fn precondition_failed_response(server: &str, client: &str) -> axum::response::Response {
    let body = serde_json::json!({
        "error": "precondition_failed",
        "code": "stale_state_version",
        "detail": "session state has advanced since If-Match was issued; refresh and retry",
        "server_etag": server,
        "client_etag": client,
    });
    let mut response = (StatusCode::PRECONDITION_FAILED, axum::Json(body)).into_response();
    if let Ok(value) = HeaderValue::from_str(server) {
        response.headers_mut().insert(ETAG, value);
    }
    response
}

/// Variant-name slice for [`scripps_workflow_conversation::SessionState`].
/// Stable across `Debug` impl changes — we don't want a derive tweak
/// to silently invalidate every outstanding ETag.
fn state_kind(state: &scripps_workflow_conversation::SessionState) -> &'static str {
    use scripps_workflow_conversation::SessionState as S;
    match state {
        S::Greeting => "Greeting",
        S::Intake => "Intake",
        S::IntakeFollowup => "IntakeFollowup",
        S::PendingConfirmation { .. } => "PendingConfirmation",
        S::ReadyToEmit => "ReadyToEmit",
        S::Emitting => "Emitting",
        S::Emitted => "Emitted",
        S::Amending { .. } => "Amending",
        S::Blocked { .. } => "Blocked",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderName;

    fn fake_session() -> Session {
        Session::new(false)
    }

    #[test]
    fn etag_is_deterministic_for_same_state_and_turn_count() {
        let s = fake_session();
        let a = etag_for_session(&s);
        let b = etag_for_session(&s);
        assert_eq!(a, b, "same session must produce same etag");
    }

    #[test]
    fn etag_changes_when_state_changes() {
        let mut s = fake_session();
        let before = etag_for_session(&s);
        s.state = scripps_workflow_conversation::SessionState::Emitted;
        let after = etag_for_session(&s);
        assert_ne!(
            before, after,
            "etag must change when session state advances"
        );
    }

    #[test]
    fn etag_changes_when_turn_count_changes() {
        let mut s = fake_session();
        let before = etag_for_session(&s);
        let conv = std::sync::Arc::make_mut(&mut s.conversation);
        conv.push(scripps_workflow_conversation::Turn::user("hi"));
        let after = etag_for_session(&s);
        assert_ne!(before, after, "etag must change when transcript grows");
    }

    #[test]
    fn wildcard_if_match_passes() {
        let s = fake_session();
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("if-match"),
            HeaderValue::from_static("*"),
        );
        match check_if_match(&h, &s, "test") {
            IfMatchOutcome::Match => {}
            _ => panic!("wildcard must match any current representation"),
        }
    }

    #[test]
    fn matching_if_match_passes() {
        let s = fake_session();
        let etag = etag_for_session(&s);
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("if-match"),
            HeaderValue::from_str(&etag).unwrap(),
        );
        match check_if_match(&h, &s, "test") {
            IfMatchOutcome::Match => {}
            _ => panic!("exact match must pass"),
        }
    }

    #[test]
    fn mismatching_if_match_fails() {
        let s = fake_session();
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("if-match"),
            HeaderValue::from_static("\"stale.0.deadbeef\""),
        );
        match check_if_match(&h, &s, "test") {
            IfMatchOutcome::Mismatch { .. } => {}
            _ => panic!("stale etag must produce Mismatch"),
        }
    }

    #[test]
    fn absent_if_match_is_tolerated() {
        let s = fake_session();
        let h = HeaderMap::new();
        match check_if_match(&h, &s, "test") {
            IfMatchOutcome::Absent => {}
            _ => panic!("absent header must surface as Absent (back-compat)"),
        }
    }
}
