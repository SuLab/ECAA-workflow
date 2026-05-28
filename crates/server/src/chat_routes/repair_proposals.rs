//! v4 P5 (D5 / F20) — HTTP surface for the repair-strategy registry.
//!
//! Three endpoints:
//!
//! - `GET /api/chat/session/:id/repair/pending` — list every
//!   `RepairProposal` the planner emitted for the session that has
//!   not yet been accepted or rejected. The pending set is computed
//!   from the verifier-decision substrate (read-only); a proposal
//!   appears here when a `RepairProposed` row exists without a
//!   matching `RepairAccepted` / `RepairRejected`.
//! - `POST /api/chat/session/:id/repair/:proposal_id/accept` — record
//!   acceptance against the substrate. The endpoint records
//!   `VerifierDecision::RepairAccepted`; the F20 invariant is that
//!   accepting `MediumUserGated` or `HighCredentialedReview` does not
//!   itself mutate the DAG — re-running the planner with the proposal
//!   applied is the next phase's work.
//! - `POST /api/chat/session/:id/repair/:proposal_id/reject` — record
//!   rejection. Substrate-only.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use ecaa_workflow_core::decision_substrate;
use ecaa_workflow_core::repair::proposal::RepairProposal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

use super::{BoundedJson, ChatAppState};

/// `POST.../accept` body.
#[derive(Debug, Deserialize)]
pub(super) struct AcceptRequest {
    /// Credential class chain the SME presents — validated against
    /// `proposal.required_credentials`. Free-text strings today; a
    /// future iteration cross-checks against
    /// `config/credential-classes.yaml`.
    #[serde(default)]
    pub credentials: Vec<String>,
    /// Optional free-text rationale appended to the substrate row.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `POST.../reject` body.
#[derive(Debug, Deserialize)]
pub(super) struct RejectRequest {
    /// SME-supplied reason. Required so the substrate carries an
    /// audit-friendly trail.
    pub reason: String,
}

/// Reply shape for `accept` + `reject`. Keeps the wire surface
/// small so the UI's optimistic-update path doesn't drift if we add
/// new substrate columns later.
#[derive(Debug, Serialize)]
pub(super) struct ProposalAckResponse {
    pub session_id: Uuid,
    pub proposal_id: String,
    pub dispatched_action: String,
}

/// Drain the substrate, filter the proposals down to the pending set,
/// and return them sorted by `(risk_class, strategy_id, id)`. The
/// substrate is process-wide — we re-record everything we drained so
/// other readers see the same events.
fn collect_pending_proposals() -> Vec<RepairProposal> {
    let events = decision_substrate::drain();

    // Re-record everything so other readers don't lose state — the
    // substrate is process-wide and `drain()` empties it.
    let snapshot = events.clone();

    let mut proposals: Vec<RepairProposal> = Vec::new();
    let mut accepted: BTreeSet<String> = BTreeSet::new();
    let mut rejected: BTreeSet<String> = BTreeSet::new();
    for ev in &events {
        match ev {
            decision_substrate::VerifierDecision::RepairProposed {
                proposal_payload, ..
            } => {
                if let Ok(p) = serde_json::from_str::<RepairProposal>(proposal_payload) {
                    proposals.push(p);
                }
            }
            decision_substrate::VerifierDecision::RepairAccepted { proposal_id, .. } => {
                accepted.insert(proposal_id.clone());
            }
            decision_substrate::VerifierDecision::RepairRejected { proposal_id, .. } => {
                rejected.insert(proposal_id.clone());
            }
            _ => {}
        }
    }
    // Restore — preserve insertion order so substrate readers see a
    // consistent view.
    for ev in snapshot {
        decision_substrate::record(ev);
    }

    proposals.retain(|p| !accepted.contains(&p.id) && !rejected.contains(&p.id));
    proposals.sort_by(|a, b| {
        (a.risk_class as u8)
            .cmp(&(b.risk_class as u8))
            .then(a.strategy_id.cmp(&b.strategy_id))
            .then(a.id.cmp(&b.id))
    });
    proposals
}

/// `GET.../repair/pending` — list pending repair proposals on the
/// session. 200 with an empty array when none is pending.
pub(super) async fn list_pending(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    if app.conversation.get_session(session_id).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    Json(collect_pending_proposals()).into_response()
}

/// `POST.../repair/:proposal_id/accept` — record acceptance.
/// 204 on success, 404 on unknown session/proposal, 403 when the
/// SME's credentials don't satisfy
/// `proposal.required_credentials`.
pub(super) async fn accept(
    State(app): State<ChatAppState>,
    Path((session_id, proposal_id)): Path<(Uuid, String)>,
    BoundedJson(req): BoundedJson<AcceptRequest>,
) -> impl IntoResponse {
    if app.conversation.get_session(session_id).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Find the proposal (so we can verify credentials).
    let Some(proposal) = collect_pending_proposals()
        .into_iter()
        .find(|p| p.id == proposal_id)
    else {
        return StatusCode::NOT_FOUND.into_response();
    };

    // Credentialed-review gate: required_credentials must be a subset
    // of the SME-supplied chain. Empty required_credentials passes
    // unconditionally.
    if !proposal.required_credentials.is_empty() {
        let presented: BTreeSet<&str> = req.credentials.iter().map(|s| s.as_str()).collect();
        let missing: Vec<&String> = proposal
            .required_credentials
            .iter()
            .filter(|c| !presented.contains(c.as_str()))
            .collect();
        if !missing.is_empty() {
            tracing::warn!(
                target: "repair_proposals",
                session_id = %session_id,
                proposal_id = %proposal_id,
                missing = ?missing,
                "Accept refused — required credentials not presented"
            );
            return StatusCode::FORBIDDEN.into_response();
        }
    }

    let acceptor = req.rationale.clone().unwrap_or_else(|| "sme".into());
    decision_substrate::record(decision_substrate::VerifierDecision::RepairAccepted {
        id: decision_substrate::stable_id("repair_accepted", &proposal_id, &acceptor),
        timestamp: decision_substrate::timestamp(),
        proposal_id: proposal_id.clone(),
        acceptor,
        credentials: req.credentials,
        // V3+v4 residuals SME-initiated accept; the planner
        // re-run applies the modification, so we don't record one here.
        attempt_kind: decision_substrate::AttemptKind::Manual,
        applied_modification: None,
    });

    Json(ProposalAckResponse {
        session_id,
        proposal_id,
        dispatched_action: "accept".into(),
    })
    .into_response()
}

/// `POST.../repair/:proposal_id/reject` — record rejection. 204 on
/// success, 404 on unknown session, 400 when the reason is empty.
pub(super) async fn reject(
    State(app): State<ChatAppState>,
    Path((session_id, proposal_id)): Path<(Uuid, String)>,
    BoundedJson(req): BoundedJson<RejectRequest>,
) -> impl IntoResponse {
    if app.conversation.get_session(session_id).await.is_none() {
        return StatusCode::NOT_FOUND.into_response();
    }
    if req.reason.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "reason must be non-empty").into_response();
    }

    decision_substrate::record(decision_substrate::VerifierDecision::RepairRejected {
        id: decision_substrate::stable_id("repair_rejected", &proposal_id, &req.reason),
        timestamp: decision_substrate::timestamp(),
        proposal_id: proposal_id.clone(),
        reason: req.reason,
    });

    Json(ProposalAckResponse {
        session_id,
        proposal_id,
        dispatched_action: "reject".into(),
    })
    .into_response()
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/repair/pending"),
    ("POST", "/api/chat/session/:id/repair/:proposal_id/accept"),
    ("POST", "/api/chat/session/:id/repair/:proposal_id/reject"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/repair/pending",
            axum::routing::get(list_pending),
        )
        .route(
            "/api/chat/session/:id/repair/:proposal_id/accept",
            axum::routing::post(accept),
        )
        .route(
            "/api/chat/session/:id/repair/:proposal_id/reject",
            axum::routing::post(reject),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::make_router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;
    use uuid::Uuid;

    #[tokio::test]
    async fn list_pending_returns_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/repair/pending", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn reject_requires_non_empty_reason() {
        let (router, app) = make_router(vec![]).await;
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/repair/p1/reject", id))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"reason":""}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
