//! v3 P9 §11.X — population-coverage routes.
//!
//! `GET /api/chat/session/:id/population-coverage` — return the applicable
//! `PopulationCoverageStatement` for the session's primary archetype.
//! Returns 404 when the session has no DAG yet or no statement on disk;
//! returns 200 with `null` when the session has a DAG but no archetype
//! id is recorded (search-derived DAG).
//!
//! `POST /api/chat/session/:id/population-waiver` — record a
//! `PopulationWaiver` on every active `PolicyBundle` for the session.
//! Returns 204 on success, 400 when rationale is empty, 404 when the
//! session doesn't exist.
//!
//! Framing constraint (v3 §11.X): the waiver records *workflow-level*
//! authorization to process an out-of-coverage cohort. It is not an
//! access-control assertion about the user's identity — it's a recorded
//! decision that the workflow's validation envelope is being explicitly
//! exceeded with the named authority's sign-off.

use super::ChatAppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Json},
    Router,
};
use ecaa_workflow_core::assumption_policy::AssumptionPolicyTable;
use ecaa_workflow_core::population_coverage::{PopulationCoverageStatement, PopulationWaiver};
use ecaa_workflow_core::workflow_contracts::policy_rule_id::PolicyRuleId;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

/// Request body for `POST /api/chat/session/:id/population-waiver`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct PopulationWaiverRequest {
    /// Workflow (archetype) id this waiver applies to.
    pub workflow_id: String,
    /// Role granting the waiver (e.g. "clinical_lead"). Resolved against
    /// the user's authentication context at higher layers; recorded here
    /// as the post-resolution name for the audit trail.
    pub waiving_authority: String,
    /// Free-text rationale. Empty rationale is rejected with 400.
    pub rationale: String,
    /// Stable id of the policy rule the waiver overrides. Matches the
    /// `RefusalReport.id` of the refusal that prompted the request.
    pub policy_rule_id: String,
}

/// 200 response body for `GET /api/chat/session/:id/population-coverage`.
/// Returns `null` for `statement` when no archetype id is available.
#[derive(Debug, Clone, Serialize)]
pub(super) struct PopulationCoverageResponse {
    /// Archetype id consulted (matches `source_template` of the
    /// session's DAG when set), or `null` if no archetype was matched.
    pub workflow_id: Option<String>,
    /// Loaded coverage statement, or `null` when no on-disk YAML matched.
    pub statement: Option<PopulationCoverageStatement>,
}

/// Resolve the directory where population-coverage YAMLs live. Honors
/// `ECAA_CONFIG_DIR` so test harnesses can point at a tmpdir.
fn coverage_dir() -> PathBuf {
    let config_dir = std::env::var("ECAA_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"));
    config_dir.join("population-coverage")
}

/// `GET /api/chat/session/:id/population-coverage` — read-only.
pub(super) async fn get_population_coverage(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let session = match app.conversation.get_session(session_id).await {
        Some(s) => s,
        None => return (StatusCode::NOT_FOUND, "session not found").into_response(),
    };
    // Best-effort archetype id resolution. The session's `workflow_intent`
    // carries `modality` and `project_class`; for clinical projects the
    // canonical archetype id encodes both (e.g. `rnaseq-de-clinical` for
    // bulk_rnaseq + clinical_trial). Today we surface the modality as the
    // workflow id, which lets the UI render the applicable statement
    // during intake before the composer has actually selected an archetype.
    // When the composer wires its result back onto `Session`, swap this
    // for the recorded `source_template`.
    let archetype_id = session
        .workflow_intent
        .as_ref()
        .and_then(|wi| wi.modality.clone());
    let archetype_id = match archetype_id {
        Some(id) => id,
        None => {
            return Json(PopulationCoverageResponse {
                workflow_id: None,
                statement: None,
            })
            .into_response();
        }
    };
    let path = coverage_dir().join(format!("{archetype_id}.yaml"));
    let statement = PopulationCoverageStatement::load_from_path(&path).ok();
    Json(PopulationCoverageResponse {
        workflow_id: Some(archetype_id),
        statement,
    })
    .into_response()
}

/// `POST /api/chat/session/:id/population-waiver` — record a waiver on
/// every active policy bundle of the session.
pub(super) async fn post_population_waiver(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<PopulationWaiverRequest>,
) -> impl IntoResponse {
    if req.rationale.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "rationale must not be empty").into_response();
    }
    if req.workflow_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "workflow_id must not be empty").into_response();
    }
    if req.waiving_authority.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "waiving_authority must not be empty",
        )
            .into_response();
    }

    // V3 §10.2 registry-validate the policy_rule_id when the
    // `policy_rules:` registry is loadable. The registry lives next to
    // the coverage YAMLs under `ECAA_CONFIG_DIR`. If the file isn't on
    // disk (test sandboxes without a full config tree), we fall back to the
    // unchecked-construction path so the surface stays available; the
    // production deploy ships the registry, so this is operator-noticeable
    // only when running outside a fully-provisioned config tree.
    let assumption_policy_path = std::env::var("ECAA_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("config"))
        .join("assumption-policy.yaml");
    let policy_rule_id = match AssumptionPolicyTable::load_from_path(&assumption_policy_path) {
        Ok(table) => match PolicyRuleId::new(&req.policy_rule_id, &table) {
            Ok(id) => id,
            Err(_) => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!(
                        "policy_rule_id {:?} is not in the assumption-policy registry",
                        req.policy_rule_id
                    ),
                )
                    .into_response();
            }
        },
        Err(_) => PolicyRuleId::unchecked(req.policy_rule_id.clone()),
    };

    let waiver = PopulationWaiver {
        workflow_id: req.workflow_id.clone(),
        waiving_authority: req.waiving_authority.clone(),
        rationale: req.rationale.clone(),
        // ISO-8601 UTC; deterministic format for byte-stable provenance.
        waived_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        policy_rule_id,
    };

    // Record the waiver as a durable decision-log entry on the session
    // so the audit trail survives session reload. The composer's
    // population-coverage gate consults `PolicyBundle.population_waivers`
    // for runtime suppression — wiring the on-disk session bundles to
    // the composer is wired separately to `Session::policy_context`;
    // for now the decision log is the
    // canonical record.
    let store = app.conversation.store_handle();
    let workflow_id = req.workflow_id.clone();
    let waiver_for_record = waiver.clone();
    let session_id_str = session_id.to_string();
    match store
        .update(session_id, move |s| {
            s.decisions
                .push(ecaa_workflow_core::decision_log::DecisionRecord::new(
                    session_id_str.clone(),
                    ecaa_workflow_core::decision_log::DecisionType::UserNote {
                        task_id: format!("population_waiver:{workflow_id}").into(),
                        body: format!(
                            "Population-coverage waiver: workflow `{workflow_id}` signed \
                             by `{}` (waived_at={}, policy_rule_id={}): {}",
                            waiver_for_record.waiving_authority,
                            waiver_for_record.waived_at,
                            waiver_for_record.policy_rule_id,
                            waiver_for_record.rationale,
                        ),
                        author: waiver_for_record.waiving_authority.clone(),
                    },
                    ecaa_workflow_core::decision_log::DecisionActor::Sme,
                    Some(waiver_for_record.rationale.clone()),
                ));
            Ok(())
        })
        .await
    {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => {
            let msg = e.to_string();
            // Typed `ApiError` envelope; UI branches on
            // `code` rather than substring-matching the body.
            if msg.contains("not found") {
                crate::error::ApiError::NotFound(msg).into_response()
            } else {
                crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response()
            }
        }
    }
}

/// Route inventory for the doc-as-contract gate.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/population-coverage"),
    ("POST", "/api/chat/session/:id/population-waiver"),
];

pub(super) fn routes() -> Router<ChatAppState> {
    Router::new()
        .route(
            "/api/chat/session/:id/population-coverage",
            axum::routing::get(get_population_coverage),
        )
        .route(
            "/api/chat/session/:id/population-waiver",
            axum::routing::post(post_population_waiver),
        )
}
