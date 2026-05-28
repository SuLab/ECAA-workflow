//! Proof / assumption / outcome / alternative endpoints.
//!
//! Exposes the typed v4 planner output to the UI:
//!
//! - `GET /api/chat/session/:id/compose-outcome` — the latest
//!   `ComposeOutcome` produced by the planner. Read-only — never
//!   recomposes; just returns the cached outcome from the session
//!   state.
//! - `GET /api/chat/session/:id/compose-alternatives` — the ranked
//!   alternative DAG list (top-K). Empty when v1/v2/v3 sessions
//!   haven't produced alternatives.
//! - `GET /api/chat/session/:id/proofs` — `runtime/proofs.jsonl`
//!   contents as a JSON array (one entry per edge).
//! - `GET /api/chat/session/:id/assumptions` — `AssumptionLedger`
//!   the SME's assumption-ledger card consumes.
//! - `GET /api/chat/session/:id/policy-decisions` — recorded
//!   per-node policy decisions from the policy gate.
//! - `GET /api/chat/session/:id/validation-reports` — per-task
//!   validation reports written by the harness validator hook.
//!   Sourced from `runtime/validation-reports.jsonl`.
//!
//! All endpoints are read-only and return `204 No Content` when the
//! session has no v4 output yet (typical for v1/v2/v3 sessions).

use super::ChatAppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ComposeOutcomeResponse {
    /// Outcome variant: `validated_executable_dag` / `draft_dag` /
    /// `partial_dag` / `novel_node_spec` / `refusal`.
    pub variant: String,
    /// Human-readable summary the UI renders directly.
    pub summary: String,
    /// Number of nodes in the produced DAG (zero when refused).
    pub node_count: u32,
    /// Number of edges in the produced DAG.
    pub edge_count: u32,
    /// Number of unresolved assumptions.
    pub assumption_count: u32,
    /// Per-node accepted list (for the AcceptedNodeList card).
    pub accepted_nodes: Vec<AcceptedNodeRow>,
    /// Refusal report when `variant == "refusal"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refusal: Option<serde_json::Value>,
    /// Novel-node spec when `variant == "novel_node_spec"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub novel_node_spec: Option<serde_json::Value>,
    /// Draft-DAG blockers when `variant == "draft_dag"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<serde_json::Value>,
    /// Partial-DAG unresolved gaps when `variant == "partial_dag"`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_gaps: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AcceptedNodeRow {
    pub id: String,
    pub human_name: String,
    pub lifecycle_state: String,
    pub trust_level: String,
    pub intent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AlternativeSummary {
    /// Stable DAG id (sortable).
    pub dag_id: String,
    /// Compact summary line.
    pub summary: String,
    /// Node count.
    pub node_count: u32,
    /// Edge count.
    pub edge_count: u32,
    /// Total adapters (lossless + lossy + risky).
    pub total_adapters: u32,
    /// Number of risky adapters in the alternative.
    pub risky_adapters: u32,
    /// Number of unresolved assumptions.
    pub unresolved_assumptions: u32,
    /// Reproducibility score (0–10) sourced from the planner's
    /// scoring tuple.
    pub reproducibility_score: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AlternativesResponse {
    pub alternatives: Vec<AlternativeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AssumptionsResponse {
    pub assumptions: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ProofsResponse {
    pub proofs: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PolicyDecisionsResponse {
    pub decisions: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct ValidationReportsResponse {
    pub reports: Vec<serde_json::Value>,
}

pub(super) async fn get_compose_outcome(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    // `tools::rebuild_dag` caches the typed
    // `ComposeOutcome` + `WorkflowDag` on the session for v4
    // dispatches. Render the cached value as a typed
    // `ComposeOutcomeResponse` the UI consumes.
    let outcome = match &session.compose_outcome {
        Some(o) => o,
        None => return StatusCode::NO_CONTENT.into_response(),
    };
    let workflow_dag = session.workflow_dag.as_ref();
    let response = render_compose_outcome(outcome, workflow_dag);
    Json(response).into_response()
}

fn render_compose_outcome(
    outcome: &scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome,
    workflow_dag: Option<&scripps_workflow_core::workflow_contracts::task_node::WorkflowDag>,
) -> ComposeOutcomeResponse {
    use scripps_workflow_core::workflow_contracts::outcome::ComposeOutcome;
    let dag_for_metrics = match outcome {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. }
        | ComposeOutcome::PartialDag { dag, .. } => Some(dag),
        _ => workflow_dag,
    };
    let node_count = dag_for_metrics.map(|d| d.nodes.len() as u32).unwrap_or(0);
    let edge_count = dag_for_metrics.map(|d| d.edges.len() as u32).unwrap_or(0);
    let assumption_count = dag_for_metrics
        .map(|d| {
            d.assumptions
                .entries
                .iter()
                .filter(|a| {
                    matches!(
                        a.resolution,
                        scripps_workflow_core::workflow_contracts::evidence::AssumptionResolution::Unresolved
                    )
                })
                .count() as u32
        })
        .unwrap_or(0);
    let accepted_nodes: Vec<AcceptedNodeRow> = dag_for_metrics
        .map(|d| {
            d.nodes
                .iter()
                .map(|n| AcceptedNodeRow {
                    id: n.id.clone(),
                    human_name: n.human_name.clone(),
                    // Use serde JSON serialization to get the
                    // `snake_case` form the UI's matchers expect
                    // (e.g. `locally_validated`, not `LocallyValidated`).
                    // The enum derives `serde(rename_all = "snake_case")`.
                    lifecycle_state: serde_json::to_value(n.lifecycle_state)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| "unknown".into()),
                    trust_level: serde_json::to_value(n.trust_level)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_else(|| "unknown".into()),
                    intent: n.intent.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    let (variant, summary, refusal, novel_node_spec, blockers, unresolved_gaps) = match outcome {
        ComposeOutcome::ValidatedExecutableDag { dag, .. } => (
            "validated_executable_dag".to_string(),
            format!(
                "Validated executable DAG — {} node(s), {} edge(s).",
                dag.nodes.len(),
                dag.edges.len()
            ),
            None,
            None,
            Vec::new(),
            Vec::new(),
        ),
        ComposeOutcome::DraftDag { dag, blockers, .. } => (
            "draft_dag".to_string(),
            format!(
                "Draft DAG — {} blocker(s), {} unresolved assumption(s) on {} node(s).",
                blockers.len(),
                dag.assumptions
                    .entries
                    .iter()
                    .filter(|a| matches!(
                        a.resolution,
                        scripps_workflow_core::workflow_contracts::evidence::AssumptionResolution::Unresolved
                    ))
                    .count(),
                dag.nodes.len()
            ),
            None,
            None,
            blockers
                .iter()
                .map(|b| serde_json::to_value(b).unwrap_or(serde_json::Value::Null))
                .collect(),
            Vec::new(),
        ),
        ComposeOutcome::PartialDag {
            unresolved_gaps, ..
        } => (
            "partial_dag".to_string(),
            format!(
                "Partial DAG — {} unresolved gap(s) preventing executable composition.",
                unresolved_gaps.len()
            ),
            None,
            None,
            Vec::new(),
            unresolved_gaps
                .iter()
                .map(|g| serde_json::to_value(g).unwrap_or(serde_json::Value::Null))
                .collect(),
        ),
        ComposeOutcome::NovelNodeSpec {
            node,
            required_work,
        } => (
            "novel_node_spec".to_string(),
            format!(
                "Novel node spec — {} requires {} validation obligation(s).",
                node.id,
                required_work.len()
            ),
            None,
            Some(
                serde_json::json!({
                    "node_id": node.id,
                    "intent": node.intent,
                    "proposed_parent_terms": node.outputs.iter().map(|p| match &p.semantic_type {
                        scripps_workflow_core::workflow_contracts::semantic_type::SemanticType::OntologyTerm { iri, .. } => iri.clone(),
                        scripps_workflow_core::workflow_contracts::semantic_type::SemanticType::LocalExtension { id, .. } => id.clone(),
                        scripps_workflow_core::workflow_contracts::semantic_type::SemanticType::Opaque { .. } => "opaque".to_string(),
                        scripps_workflow_core::workflow_contracts::semantic_type::SemanticType::Union { .. } => p.semantic_type.stable_id(),
                    }).collect::<Vec<_>>(),
                    "declared_inputs": node.inputs.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                    "declared_outputs": node.outputs.iter().map(|p| p.name.clone()).collect::<Vec<_>>(),
                    "declared_assumptions": Vec::<String>::new(),
                    "declared_failure_modes": Vec::<String>::new(),
                    "validation_obligations": required_work.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
                }),
            ),
            Vec::new(),
            Vec::new(),
        ),
        ComposeOutcome::Refusal { report } => (
            "refusal".to_string(),
            report.statement.clone(),
            Some(serde_json::to_value(report).unwrap_or(serde_json::Value::Null)),
            None,
            Vec::new(),
            Vec::new(),
        ),
    };

    ComposeOutcomeResponse {
        variant,
        summary,
        node_count,
        edge_count,
        assumption_count,
        accepted_nodes,
        refusal,
        novel_node_spec,
        blockers,
        unresolved_gaps,
    }
}

pub(super) async fn get_compose_alternatives(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    if session.ranked_alternatives.is_empty() {
        return Json(AlternativesResponse {
            alternatives: Vec::new(),
        })
        .into_response();
    }
    let alternatives: Vec<AlternativeSummary> = session
        .ranked_alternatives
        .iter()
        .map(|a| {
            // Count adapter / unresolved-assumption tallies from the
            // alternative's WorkflowDag. Risky/lossy adapter
            // detection looks at the inserted-adapter-node ids
            // emitted by the compatibility engine — every adapter
            // is a TaskNode whose id starts with `adapter_`.
            let total_adapters = a
                .dag
                .nodes
                .iter()
                .filter(|n| n.id.starts_with("adapter_"))
                .count() as u32;
            let risky_adapters = a
                .dag
                .nodes
                .iter()
                .filter(|n| {
                    n.id.starts_with("adapter_")
                        && n.attributes
                            .get("safety")
                            .and_then(|v| v.as_str())
                            .map(|s| s == "scientifically_risky" || s == "policy_restricted")
                            .unwrap_or(false)
                })
                .count() as u32;
            let unresolved_assumptions = a
                .dag
                .assumptions
                .entries
                .iter()
                .filter(|asm| {
                    matches!(
                        asm.resolution,
                        scripps_workflow_core::workflow_contracts::evidence::AssumptionResolution::Unresolved
                    )
                })
                .count() as u32;
            // Reproducibility score: invert the scoring tuple's
            // reproducibility_penalty (lower penalty = higher repro
            // score in 0..=10). Default 5 when unset.
            let reproducibility_score = derive_reproducibility_score(&a.score);
            AlternativeSummary {
                dag_id: a.dag.id.clone(),
                summary: a.summary.clone(),
                node_count: a.dag.nodes.len() as u32,
                edge_count: a.dag.edges.len() as u32,
                total_adapters,
                risky_adapters,
                unresolved_assumptions,
                reproducibility_score,
            }
        })
        .collect();
    Json(AlternativesResponse { alternatives }).into_response()
}

fn derive_reproducibility_score(score: &scripps_workflow_core::composer_v4::ScoringTuple) -> u32 {
    // The scoring tuple's `reproducibility_penalty` is lower-is-better
    // (0 = perfect repro). Map to a 0..=10 score: 0 penalty → 10,
    // saturating downward. Tunable; the UI legend treats >=8 as
    // "good", <8 as "warn".
    let penalty = score.reproducibility_penalty as i64;
    let mapped = 10i64 - penalty.clamp(0, 10);
    mapped.clamp(0, 10) as u32
}

pub(super) async fn get_proofs(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let proofs = read_jsonl_artifact(session.emitted_package_path.as_ref(), "proofs.jsonl").await;
    Json(ProofsResponse { proofs }).into_response()
}

pub(super) async fn get_assumptions(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    // Prefer the cached WorkflowDag (in-memory after rebuild_dag) over
    // the on-disk JSONL; the cached path is fresher when the session
    // has been recomposed since the last emit. Fall through to the
    // sidecar for emitted-but-not-recomposed sessions.
    if let Some(dag) = session.workflow_dag.as_ref() {
        let entries: Vec<serde_json::Value> = dag
            .assumptions
            .entries
            .iter()
            .map(flatten_assumption)
            .collect();
        return Json(AssumptionsResponse {
            assumptions: serde_json::json!({ "entries": entries }),
        })
        .into_response();
    }
    let entries: Vec<serde_json::Value> =
        read_jsonl_artifact(session.emitted_package_path.as_ref(), "assumptions.jsonl")
            .await
            .into_iter()
            .map(flatten_assumption_value)
            .collect();
    Json(AssumptionsResponse {
        assumptions: serde_json::json!({ "entries": entries }),
    })
    .into_response()
}

/// Flatten an `Assumption` for the UI: replace tagged-enum `source`
/// and `resolution` objects with bare snake_case strings the
/// AssumptionLedgerCard's mappers handle directly. Wire format
/// chosen for UI ergonomics; lossless inverse (the original
/// rationale / validator_id payloads carry through as separate
/// `source_detail` / `resolution_detail` fields).
fn flatten_assumption(
    a: &scripps_workflow_core::workflow_contracts::evidence::Assumption,
) -> serde_json::Value {
    let value = serde_json::to_value(a).unwrap_or(serde_json::Value::Null);
    flatten_assumption_value(value)
}

fn flatten_assumption_value(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(obj) = v.as_object_mut() {
        // Flatten `source` tagged enum.
        let source_kind = obj
            .get("source")
            .and_then(|s| s.get("kind"))
            .and_then(|k| k.as_str())
            .map(String::from);
        if let Some(k) = source_kind {
            let detail = obj.get("source").cloned();
            obj.insert("source".into(), serde_json::Value::String(k));
            if let Some(d) = detail {
                obj.insert("source_detail".into(), d);
            }
        }
        // Flatten `resolution` tagged enum. Map enum names to UI
        // expected values: Unresolved → "unresolved", Accepted →
        // "confirmed", Rejected → "rejected", ResolvedByValidator →
        // "confirmed".
        let resolution_kind = obj
            .get("resolution")
            .and_then(|r| r.get("kind"))
            .and_then(|k| k.as_str())
            .map(String::from);
        if let Some(k) = resolution_kind {
            let detail = obj.get("resolution").cloned();
            let ui_value = match k.as_str() {
                "accepted" | "resolved_by_validator" => "confirmed",
                "rejected" => "rejected",
                _ => "unresolved",
            };
            obj.insert(
                "resolution".into(),
                serde_json::Value::String(ui_value.into()),
            );
            if let Some(d) = detail {
                obj.insert("resolution_detail".into(), d);
            }
        }
        // Flatten `risk` if it's an object (defensive — RiskClass
        // is a unit-variant enum with snake_case rename, so it
        // serializes as a bare string already; this path is a
        // no-op for the standard shape).
        let risk_kind = obj
            .get("risk")
            .and_then(|r| r.get("kind"))
            .and_then(|k| k.as_str())
            .map(String::from);
        if let Some(k) = risk_kind {
            obj.insert("risk".into(), serde_json::Value::String(k));
        }
    }
    v
}

pub(super) async fn get_policy_decisions(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    if !session.policy_decisions.is_empty() {
        let decisions: Vec<serde_json::Value> = session
            .policy_decisions
            .iter()
            .map(|d| serde_json::to_value(d).unwrap_or(serde_json::Value::Null))
            .collect();
        return Json(PolicyDecisionsResponse { decisions }).into_response();
    }
    let decisions = read_jsonl_artifact(
        session.emitted_package_path.as_ref(),
        "policy-decisions.jsonl",
    )
    .await;
    Json(PolicyDecisionsResponse { decisions }).into_response()
}

pub(super) async fn get_validation_reports(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let reports = read_jsonl_artifact(
        session.emitted_package_path.as_ref(),
        "validation-reports.jsonl",
    )
    .await;
    Json(ValidationReportsResponse { reports }).into_response()
}

/// Read a JSONL sidecar from the session's emitted package's
/// `runtime/` directory. Returns an empty vec when:
/// - the session has no emitted package yet (typical for sessions
///   that haven't called `emit_package`),
/// - the file is missing on disk (typical for v1/v2 sessions that
///   don't emit semantic sidecars),
/// - the file is unreadable (fast-fails to empty rather than 500
///   so the UI shows the empty-state affordance).
async fn read_jsonl_artifact(
    emitted_package_path: Option<&PathBuf>,
    filename: &str,
) -> Vec<serde_json::Value> {
    let Some(root) = emitted_package_path else {
        return Vec::new();
    };
    let path = root.join("runtime").join(filename);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

/// POST body for resolving an assumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AssumptionResolutionRequest {
    pub assumption_id: String,
    /// `confirmed` (accepted) or `rejected`.
    pub resolution: String,
    /// Optional free-text rationale for the resolution; persisted
    /// onto the DecisionRecord for audit.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `POST /api/chat/session/:id/assumption-resolve` — record an SME
/// resolution against an unresolved assumption. Updates both the
/// in-memory `Session::workflow_dag.assumptions.entries[i].resolution`
/// and appends a `DecisionType::AssumptionResolved` row to
/// `session.decisions`.
pub(super) async fn resolve_assumption(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<AssumptionResolutionRequest>,
) -> impl IntoResponse {
    if !matches!(req.resolution.as_str(), "confirmed" | "rejected") {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown resolution `{}` — must be `confirmed` or `rejected`",
                req.resolution
            ),
        )
            .into_response();
    }
    match app
        .conversation
        .resolve_assumption(
            session_id,
            req.assumption_id.clone(),
            req.resolution.clone(),
            req.rationale,
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response(),
    }
}

/// POST body for recording an adapter
/// confirm/reject decision on a session. The SME's decision becomes
/// a durable `DecisionType::AdapterDecisionRecorded` row in the
/// audit log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct AdapterDecisionRequest {
    pub adapter_id: String,
    /// `confirmed` or `rejected`.
    pub decision: String,
    /// Adapter safety class at decision time (`lossy_declared`,
    /// `scientifically_risky`, `policy_restricted`). Forwarded by
    /// the UI from the AdapterWarningCard's safety chip.
    #[serde(default)]
    pub safety: Option<String>,
}

/// `POST /api/chat/session/:id/adapter-decision`
pub(super) async fn record_adapter_decision(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<AdapterDecisionRequest>,
) -> impl IntoResponse {
    if !matches!(req.decision.as_str(), "confirmed" | "rejected") {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown decision `{}` — must be `confirmed` or `rejected`",
                req.decision
            ),
        )
            .into_response();
    }
    match app
        .conversation
        .record_adapter_decision(
            session_id,
            req.adapter_id.clone(),
            req.decision.clone(),
            req.safety.clone().unwrap_or_else(|| "unknown".into()),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response(),
    }
}

/// POST body for accepting/rejecting a NovelNodeSpec from the v4 planner.
/// Accept enters the proposals registry as a HypothesizedNode;
/// reject removes it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct NovelNodeDecisionRequest {
    pub node_id: String,
    /// `accepted_as_draft` or `rejected`.
    pub decision: String,
}

/// `POST /api/chat/session/:id/novel-node-decision`
pub(super) async fn record_novel_node_decision(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<NovelNodeDecisionRequest>,
) -> impl IntoResponse {
    if !matches!(req.decision.as_str(), "accepted_as_draft" | "rejected") {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown decision `{}` — must be `accepted_as_draft` or `rejected`",
                req.decision
            ),
        )
            .into_response();
    }
    match app
        .conversation
        .record_novel_node_decision(session_id, req.node_id.clone(), req.decision.clone())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response(),
    }
}

/// POST body for acknowledging a Refusal outcome with
/// the SME's chosen recovery affordance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct RefusalAcknowledgmentRequest {
    pub refusal_id: String,
    /// `branch`, `amend_policy`, or `dismiss`.
    pub recovery: String,
}

/// `POST /api/chat/session/:id/refusal-acknowledge`
pub(super) async fn acknowledge_refusal(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<RefusalAcknowledgmentRequest>,
) -> impl IntoResponse {
    if !matches!(req.recovery.as_str(), "branch" | "amend_policy" | "dismiss") {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "unknown recovery `{}` — must be `branch`, `amend_policy`, or `dismiss`",
                req.recovery
            ),
        )
            .into_response();
    }
    match app
        .conversation
        .acknowledge_refusal(session_id, req.refusal_id.clone(), req.recovery.clone())
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response(),
    }
}

/// POST body for activating a policy bundle on a session.
/// Pass `bundle_id: null` (or omit) to clear an active bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PolicyBundleRequest {
    /// Recognized bundle ids: `clinical_trial`, `phi_strict`. Pass
    /// `None` to clear.
    #[serde(default)]
    pub bundle_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct PolicyBundleResponse {
    pub active_bundle: Option<String>,
}

/// `POST /api/chat/session/:id/policy-bundle` — activate or clear the
/// session's active policy bundle. Triggers a full DAG rebuild so the
/// per-node policy gate fires immediately and the Composition tab
/// reflects the new policy decisions on the next state-advanced
/// event. Returns `400` for unknown bundle ids.
pub(super) async fn set_policy_bundle(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    Json(req): Json<PolicyBundleRequest>,
) -> impl IntoResponse {
    if let Some(ref id) = req.bundle_id {
        if !matches!(id.as_str(), "clinical_trial" | "phi_strict") {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "unknown bundle id `{}` — recognized: clinical_trial, phi_strict",
                    id
                ),
            )
                .into_response();
        }
    }
    let updated = match app
        .conversation
        .set_active_policy_bundle(session_id, req.bundle_id.clone())
        .await
    {
        Ok(active) => active,
        Err(e) => {
            return (StatusCode::NOT_FOUND, format!("session not found: {}", e)).into_response();
        }
    };
    Json(PolicyBundleResponse {
        active_bundle: updated,
    })
    .into_response()
}

pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/compose-outcome"),
    ("GET", "/api/chat/session/:id/compose-alternatives"),
    ("GET", "/api/chat/session/:id/proofs"),
    ("GET", "/api/chat/session/:id/assumptions"),
    ("GET", "/api/chat/session/:id/policy-decisions"),
    ("GET", "/api/chat/session/:id/validation-reports"),
    ("POST", "/api/chat/session/:id/policy-bundle"),
    ("POST", "/api/chat/session/:id/assumption-resolve"),
    ("POST", "/api/chat/session/:id/adapter-decision"),
    ("POST", "/api/chat/session/:id/novel-node-decision"),
    ("POST", "/api/chat/session/:id/refusal-acknowledge"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/compose-outcome",
            axum::routing::get(get_compose_outcome),
        )
        .route(
            "/api/chat/session/:id/compose-alternatives",
            axum::routing::get(get_compose_alternatives),
        )
        .route(
            "/api/chat/session/:id/proofs",
            axum::routing::get(get_proofs),
        )
        .route(
            "/api/chat/session/:id/assumptions",
            axum::routing::get(get_assumptions),
        )
        .route(
            "/api/chat/session/:id/policy-decisions",
            axum::routing::get(get_policy_decisions),
        )
        .route(
            "/api/chat/session/:id/validation-reports",
            axum::routing::get(get_validation_reports),
        )
        .route(
            "/api/chat/session/:id/policy-bundle",
            axum::routing::post(set_policy_bundle),
        )
        .route(
            "/api/chat/session/:id/assumption-resolve",
            axum::routing::post(resolve_assumption),
        )
        .route(
            "/api/chat/session/:id/adapter-decision",
            axum::routing::post(record_adapter_decision),
        )
        .route(
            "/api/chat/session/:id/novel-node-decision",
            axum::routing::post(record_novel_node_decision),
        )
        .route(
            "/api/chat/session/:id/refusal-acknowledge",
            axum::routing::post(acknowledge_refusal),
        )
}
