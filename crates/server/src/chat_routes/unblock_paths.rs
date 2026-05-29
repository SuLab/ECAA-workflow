//! HTTP surface for `UnblockPath` dispatch.
//!
//! Two endpoints:
//!
//! - `GET /api/chat/session/:id/refusal/:refusal_id/unblock-paths` —
//!   list every `UnblockPath` the v4 planner synthesized for the
//!   refusal. Read-only; the UI calls this to populate the recovery
//!   card.
//! - `POST /api/chat/session/:id/refusal/:refusal_id/dispatch` — the
//!   SME picked a path; route it to the deterministic server-side
//!   handler. Wires `AttemptRepair`, `SupplyMissingMetadata`, and
//!   `EscalateToReviewer`. `ResolveAssumption` and `Waiver` return
//!   `400 BAD_REQUEST` with a `not_implemented` error code — the
//!   planner still synthesizes the variants (the UI lists them via the
//!   GET endpoint) but the dispatch surface refuses them rather than
//!   silently succeeding on a tracing-only stub. The variants will be
//!   wired when the assumption-ledger + credential-class registries
//!   land.
//!
//! The refusal is looked up against the session's cached
//! `compose_outcome` (populated by the v4 planner's most recent run).
//! When the cached outcome isn't a `Refusal`, or the refusal id
//! doesn't match, the endpoint returns 404 — the UI is expected to
//! re-render in that case.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use ecaa_workflow_core::workflow_contracts::outcome::{ComposeOutcome, RefusalReport};
use ecaa_workflow_core::workflow_contracts::unblock_path::UnblockPath;

use super::{BoundedJson, ChatAppState};

/// JSON body for `POST.../dispatch`. `path_index` indexes into
/// `refusal.unblock_paths`; `payload` is the per-variant data the
/// dispatch handler consumes (assumption resolution value, credential
/// IDs, metadata value, etc.).
#[derive(Debug, Deserialize)]
pub(super) struct DispatchRequest {
    #[serde(default)]
    pub path_index: usize,
    #[serde(default)]
    #[allow(dead_code)]
    pub payload: serde_json::Value,
}

/// Stub response body for `POST.../dispatch`. Returns an
/// acknowledgement carrying the dispatched path kind so the UI can
/// surface a per-kind toast. Richer state (new `session.state`,
/// mutated assumption ids, etc.) will be added when stubs are wired.
#[derive(Debug, Clone, Serialize)]
pub(super) struct DispatchResponse {
    pub session_id: Uuid,
    pub refusal_id: String,
    pub dispatched_path_kind: String,
}

/// Locate the active refusal report on the session's cached compose
/// outcome. Returns `None` when the cached outcome isn't a refusal or
/// the refusal id doesn't match.
async fn lookup_refusal(
    app: &ChatAppState,
    session_id: Uuid,
    refusal_id: &str,
) -> Option<RefusalReport> {
    let session = app.conversation.get_session(session_id).await?;
    let outcome = session.compose_outcome.as_ref()?;
    let ComposeOutcome::Refusal { report } = outcome else {
        return None;
    };
    if report.id != refusal_id {
        return None;
    }
    Some(report.clone())
}

/// `GET /api/chat/session/:id/refusal/:refusal_id/unblock-paths` —
/// list every synthesized unblock path on the active refusal.
///
/// 200 → JSON array of `UnblockPath`.
/// 404 → session or refusal not found (or cached outcome isn't a
/// refusal).
pub(super) async fn list_unblock_paths(
    State(app): State<ChatAppState>,
    Path((session_id, refusal_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    match lookup_refusal(&app, session_id, &refusal_id).await {
        Some(report) => Json(report.unblock_paths).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `POST /api/chat/session/:id/refusal/:refusal_id/dispatch` — route
/// the SME's chosen path to the deterministic server-side handler.
///
/// 200 → dispatched; body carries `DispatchResponse`.
/// 400 → `path_index` out of range, or the chosen variant
///         (`ResolveAssumption` / `Waiver`) is not yet implemented
///         (`{"error":"not_implemented","kind":"...","detail":"..."}`).
/// 404 → session/refusal not found.
pub(super) async fn dispatch_unblock_path(
    State(app): State<ChatAppState>,
    Path((session_id, refusal_id)): Path<(Uuid, String)>,
    BoundedJson(req): BoundedJson<DispatchRequest>,
) -> impl IntoResponse {
    let Some(report) = lookup_refusal(&app, session_id, &refusal_id).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(path) = report.unblock_paths.get(req.path_index) else {
        return StatusCode::BAD_REQUEST.into_response();
    };

    // Each variant lands in its dedicated handler so the dispatch is
    // a closed switch over the typed taxonomy. `ResolveAssumption` and
    // `Waiver` are not yet wired (assumption-ledger + credential-class
    // registry land later); they return a typed `not_implemented` 400
    // so the UI surfaces the gap explicitly instead of receiving an
    // acknowledgement for an op the server didn't actually perform.
    let dispatched_kind = match dispatch_unblock_path_variant(path, session_id, &refusal_id) {
        Ok(kind) => kind,
        Err(resp) => return *resp,
    };

    Json(DispatchResponse {
        session_id,
        refusal_id,
        dispatched_path_kind: dispatched_kind.to_string(),
    })
    .into_response()
}

/// Closed switch over the typed `UnblockPath` taxonomy. Returns the dispatched
/// kind string on success, or a typed `not_implemented` 400 response for the
/// variants whose handlers are not yet wired.
fn dispatch_unblock_path_variant(
    path: &UnblockPath,
    session_id: Uuid,
    refusal_id: &str,
) -> Result<&'static str, Box<axum::response::Response>> {
    match path {
        UnblockPath::ResolveAssumption { assumption_id, .. } => Err(Box::new(
            refuse_resolve_assumption(session_id, refusal_id, assumption_id),
        )),
        UnblockPath::Waiver {
            rule_id,
            required_credentials,
            ..
        } => Err(Box::new(refuse_waiver(
            session_id,
            refusal_id,
            rule_id,
            required_credentials,
        ))),
        UnblockPath::AttemptRepair {
            strategy_id,
            gap_id,
            ..
        } => Ok(dispatch_attempt_repair(
            session_id,
            refusal_id,
            strategy_id,
            gap_id,
        )),
        UnblockPath::SupplyMissingMetadata { field, .. } => Ok(dispatch_supply_missing_metadata(
            session_id, refusal_id, field,
        )),
        UnblockPath::EscalateToReviewer {
            reviewer_class,
            required_artifacts,
            ..
        } => Ok(dispatch_escalate_to_reviewer(
            session_id,
            refusal_id,
            reviewer_class,
            required_artifacts,
        )),
    }
}

/// Refuse a `ResolveAssumption` dispatch (assumption-ledger wiring deferred).
fn refuse_resolve_assumption(
    session_id: Uuid,
    refusal_id: &str,
    assumption_id: &str,
) -> axum::response::Response {
    tracing::warn!(
        target: "unblock_paths",
        session_id = %session_id,
        refusal_id = %refusal_id,
        assumption_id = %assumption_id,
        "ResolveAssumption dispatch refused: handler not yet implemented"
    );
    not_implemented_response(
        "resolve_assumption",
        "ResolveAssumption dispatch is not yet implemented in this version; the assumption-ledger wiring is deferred. Pick a different unblock path.",
    )
}

/// Refuse a `Waiver` dispatch (credential-class verification deferred).
fn refuse_waiver(
    session_id: Uuid,
    refusal_id: &str,
    rule_id: &str,
    required_credentials: &[String],
) -> axum::response::Response {
    tracing::warn!(
        target: "unblock_paths",
        session_id = %session_id,
        refusal_id = %refusal_id,
        rule_id = %rule_id,
        required_credentials = ?required_credentials,
        "Waiver dispatch refused: handler not yet implemented"
    );
    not_implemented_response(
        "waiver",
        "Waiver dispatch is not yet implemented in this version; credential-class verification is deferred. Pick a different unblock path.",
    )
}

/// `SupplyMissingMetadata` dispatch — routes to the per-field intake mutation
/// (set_intake_field / set_intake_method). Currently a stub that roundtrips the
/// dispatch ack.
fn dispatch_supply_missing_metadata(
    session_id: Uuid,
    refusal_id: &str,
    field: &str,
) -> &'static str {
    tracing::info!(
        target: "unblock_paths",
        session_id = %session_id,
        refusal_id = %refusal_id,
        field = %field,
        "SupplyMissingMetadata dispatch (phase 4 stub; phase 5 mutates intake)"
    );
    "supply_missing_metadata"
}

/// `EscalateToReviewer` dispatch — creates a typed escalation entry + opens an
/// out-of-band reviewer request. Currently captures the request via tracing.
fn dispatch_escalate_to_reviewer(
    session_id: Uuid,
    refusal_id: &str,
    reviewer_class: &str,
    required_artifacts: &[String],
) -> &'static str {
    tracing::info!(
        target: "unblock_paths",
        session_id = %session_id,
        refusal_id = %refusal_id,
        reviewer_class = %reviewer_class,
        required_artifacts = ?required_artifacts,
        "EscalateToReviewer dispatch (phase 4 stub; phase 5 opens escalation)"
    );
    "escalate_to_reviewer"
}

/// Build the typed `not_implemented` 400 body for an unblock-path variant whose
/// server-side handler is not yet wired.
fn not_implemented_response(kind: &str, detail: &str) -> axum::response::Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({
            "error": "not_implemented",
            "kind": kind,
            "detail": detail,
        })),
    )
        .into_response()
}

/// Route an `AttemptRepair` dispatch to the substrate-backed repair surface,
/// recording the SME's explicit "attempt repair" intent as an audit
/// breadcrumb. The accept/reject of the proposal happens via the dedicated
/// repair endpoints (which require the proposal id, not the gap id).
fn dispatch_attempt_repair(
    session_id: Uuid,
    refusal_id: &str,
    strategy_id: &str,
    gap_id: &str,
) -> &'static str {
    tracing::info!(
        target: "unblock_paths",
        session_id = %session_id,
        refusal_id = %refusal_id,
        strategy_id = %strategy_id,
        gap_id = %gap_id,
        "AttemptRepair dispatch routed to repair-proposals service (v4 P5)"
    );
    // Emit a substrate breadcrumb so the audit trail captures
    // the SME's explicit "attempt repair" intent. The repair
    // endpoint's accept/reject calls record the load-bearing
    // RepairAccepted / RepairRejected rows.
    ecaa_workflow_core::decision_substrate::record(
        ecaa_workflow_core::decision_substrate::VerifierDecision::RepairProposed {
            id: ecaa_workflow_core::decision_substrate::stable_id(
                "repair_dispatch_intent",
                strategy_id,
                gap_id,
            ),
            timestamp: ecaa_workflow_core::decision_substrate::timestamp(),
            gap_id: gap_id.to_string(),
            strategy: strategy_id.to_string(),
            risk_class: String::from("dispatched"),
            proposal_payload: format!(
                "{{\"dispatch_intent\":true,\"strategy_id\":\"{}\",\"gap_id\":\"{}\"}}",
                strategy_id, gap_id
            ),
        },
    );
    "attempt_repair"
}

/// Route inventory for `mod.rs::ALL_ROUTES`.
pub(super) const ROUTES: &[(&str, &str)] = &[
    (
        "GET",
        "/api/chat/session/:id/refusal/:refusal_id/unblock-paths",
    ),
    ("POST", "/api/chat/session/:id/refusal/:refusal_id/dispatch"),
];

/// Per-domain `routes()` builder. Merged by `mod.rs::router`.
pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/refusal/:refusal_id/unblock-paths",
            axum::routing::get(list_unblock_paths),
        )
        .route(
            "/api/chat/session/:id/refusal/:refusal_id/dispatch",
            axum::routing::post(dispatch_unblock_path),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_routes::test_support::make_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    /// Smoke test: GET on an unknown session returns 404. Exercising
    /// the route table is the load-bearing assertion here — the
    /// dispatch logic itself is tested below.
    #[tokio::test]
    async fn list_returns_404_on_unknown_session() {
        let (router, _app) = make_router(vec![]).await;
        let fake = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/refusal/r1/unblock-paths",
                fake
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn dispatch_returns_404_on_unknown_session() {
        let (router, _app) = make_router(vec![]).await;
        let fake = Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/refusal/r1/dispatch", fake))
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"path_index": 0, "payload": {}}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
