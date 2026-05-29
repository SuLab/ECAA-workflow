//! Hypothesized
//! proposal lifecycle REST routes.
//!
//! Four routes wrap the session-scoped proposals registry that
//! `propose_hypothesized_node` writes to:
//!
//! - `GET /api/chat/session/:id/proposals` — list all proposals
//!   sorted by `created_at` ascending.
//! - `GET /api/chat/session/:id/proposal/:proposal_id` — fetch one
//!   full `HypothesizedProposal`.
//! - `POST /api/chat/session/:id/proposal/:proposal_id/signoff` —
//!   the SME approve gate. Refuses when the proposal isn't in
//!   `AwaitingSignoff`; on success materializes a `TaskNode`, splices
//!   it into `session.workflow_dag.nodes`, marks the proposal
//!   `Promoted { task_node_id }`, fires the git-commit hook, and
//!   broadcasts `ProposalPromoted` + `StateAdvanced` SSE events.
//! - `POST /api/chat/session/:id/proposal/:proposal_id/reject` —
//!   terminal-state guard plus a `DecisionType::ProposalRejected`
//!   audit row and a `ProposalRejected` SSE event.
//!
//! §7.3 and §7.4 define the canonical accept/reject contract.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};

use super::BoundedJson;
use ecaa_workflow_core::hypothesized_proposal::{
    proposal_to_materialized_task_node, HypothesizedProposal, ProposalId, ProposalLifecycle,
};
use ecaa_workflow_core::workflow_contracts::lifecycle::PromotionAuthority;
use serde::Deserialize;
use uuid::Uuid;

use super::{ChatAppState, DropNotifier, SsePayload};
use std::sync::Arc;

/// `POST.../signoff` body. Both fields are optional; the handler
/// substitutes `"sme"` when `sme_initials` is absent so the
/// `PromotionAuthority` row never carries an empty id.
#[derive(Debug, Default, Deserialize)]
pub(super) struct SignoffRequest {
    /// SME initials / username. Becomes `PromotionAuthority.id`. Falls
    /// back to `"sme"` when absent so the audit trail always has an
    /// actor string.
    #[serde(default)]
    pub sme_initials: Option<String>,
}

/// `POST.../reject` body.
#[derive(Debug, Default, Deserialize)]
pub(super) struct RejectRequest {
    /// Free-form SME rationale; propagates into
    /// `ProposalLifecycle::Rejected { rationale }` and the
    /// `DecisionType::ProposalRejected` audit record.
    #[serde(default)]
    pub rationale: Option<String>,
}

/// `GET /api/chat/session/:id/proposals` — return every proposal on
/// the session, sorted by `created_at` ascending. Returns `404` when
/// the session id is unknown.
pub(super) async fn list_proposals(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    // BTreeMap iter is in `ProposalId` order; the SME-facing card
    // wants chronological order so the timeline reads top-to-bottom.
    // Stable sort by `(created_at, id)` so ties break deterministically.
    let mut proposals: Vec<HypothesizedProposal> = session.proposals.values().cloned().collect();
    proposals.sort_by(|a, b| {
        a.created_at
            .cmp(&b.created_at)
            .then_with(|| a.id.0.cmp(&b.id.0))
    });
    Json(proposals).into_response()
}

/// `GET /api/chat/session/:id/proposal/:proposal_id` — fetch one
/// proposal by id. Returns `404` when the session or proposal is
/// unknown.
pub(super) async fn get_proposal(
    State(app): State<ChatAppState>,
    Path((session_id, proposal_id)): Path<(Uuid, String)>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };
    let pid = ProposalId(proposal_id);
    match session.proposals.get(&pid) {
        Some(p) => Json(p.clone()).into_response(),
        None => (StatusCode::NOT_FOUND, "proposal not found").into_response(),
    }
}

/// `POST /api/chat/session/:id/proposal/:proposal_id/signoff` — SME
/// approve gate. Refuses with `409` when the proposal isn't in
/// `AwaitingSignoff`; on success materializes a `TaskNode`, splices
/// it into `session.workflow_dag.nodes`, marks the proposal
/// `Promoted`, fires the git-commit hook (fire-and-forget), and
/// broadcasts `ProposalPromoted` + `StateAdvanced` SSE events.
pub(super) async fn signoff_proposal(
    State(app): State<ChatAppState>,
    Path((session_id, proposal_id)): Path<(Uuid, String)>,
    body: Option<BoundedJson<SignoffRequest>>,
) -> impl IntoResponse {
    let pid = ProposalId(proposal_id);
    let sme_initials = body
        .and_then(|BoundedJson(b)| b.sme_initials)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "sme".to_string());

    // Build the PromotionAuthority once so the closure captures the
    // pre-baked struct rather than re-computing inside the lock.
    let authority = PromotionAuthority {
        kind: "human".into(),
        id: sme_initials,
        at: ecaa_workflow_core::time_helpers::now_rfc3339(),
    };

    // outcome_cell carries the materialized node + final lifecycle out
    // of the update closure so we can broadcast / fire the git hook
    // after the store lock releases. The closure is `FnOnce` so we only
    // ever write once; std::sync::OnceLock matches that contract without
    // the unwrap_or_else(poison) ceremony a Mutex requires. `Arc` makes
    // it cloneable into the closure, and `set` is `&self` so no
    // interior-mutability ceremony at the call sites either.
    let outcome_cell: std::sync::Arc<std::sync::OnceLock<SignoffOutcome>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let outcome_writer = outcome_cell.clone();
    let pid_for_update = pid.clone();

    let store_result = app
        .conversation
        .store_handle()
        .update(session_id, move |session| {
            // 1. Locate the proposal.
            let Some(proposal) = session.proposals.get(&pid_for_update).cloned() else {
                let _ = outcome_writer.set(SignoffOutcome::NotFound);
                return Ok(());
            };
            // 2. Refuse non-AwaitingSignoff lifecycles with 409.
            if !matches!(proposal.lifecycle, ProposalLifecycle::AwaitingSignoff) {
                let _ = outcome_writer.set(SignoffOutcome::WrongState {
                    current_kind: proposal.lifecycle.kind_str().to_string(),
                });
                return Ok(());
            }
            // 3. Materialize the proposal as a TaskNode.
            let task_node = proposal_to_materialized_task_node(&proposal, authority.clone());
            let task_node_id: String = task_node.id.clone();
            // 4. Splice into session.workflow_dag.nodes if present.
            // When workflow_dag is None we log a warning and let the
            // next rebuild_dag pick up the node; the proposal still
            // transitions to Promoted (per spec §7.3 step 3).
            //
            // Route the mutation through the `workflow_dag_mut()` RAII
            // guard so the derived `session.dag` cache is invalidated
            // on drop. The previous bare `session.workflow_dag.as_mut()`
            // left the cache stale, so reads through `current_dag()`
            // after signoff returned pre-splice structure.
            if let Some(dag) = session.workflow_dag_mut().as_mut() {
                // Idempotency: skip the splice when an identical id is
                // already present (concurrent signoff race).
                if !dag.nodes.iter().any(|n| n.id == task_node_id) {
                    dag.nodes.push(task_node.clone());
                }
                // Wire edges. Prior to this block the promoted node
                // entered the DAG as an isolated TaskNode — the
                // workflow_json lowering computes Task.depends_on
                // strictly from `dag.edges`, so a node without edges
                // emits as a fully-stranded entry in WORKFLOW.json
                // (no upstream, no downstream). The audit run across
                // 15 recent packages found this happening on every
                // Tier-B + novel-method scenario (survival_analysis,
                // cryoem_*, snmc_*, codex_*, scatac_qc_tfidf,
                // two_sample_mr, tad_calling…).
                //
                // Wire each `proposal.upstream_atom_ids` entry that
                // exists in the DAG as a from→promoted edge. The
                // CompatibilityProof is synthetic ("SME-declared
                // dependency") because there's no real port-type
                // subsumption to prove — the SME authored the
                // dependency via propose_hypothesized_node.
                use ecaa_workflow_core::workflow_contracts::edge::{
                    CompatibilityProof, EdgeContract,
                };
                for upstream in &proposal.upstream_atom_ids {
                    if !dag.nodes.iter().any(|n| n.id.as_str() == upstream.as_str()) {
                        continue;
                    }
                    let already_wired = dag.edges.iter().any(|e| {
                        e.from_node == *upstream && e.to_node == task_node_id
                    });
                    if already_wired {
                        continue;
                    }
                    dag.edges.push(EdgeContract {
                        from_node: upstream.clone(),
                        from_port: "default".to_string(),
                        to_node: task_node_id.clone(),
                        to_port: "default".to_string(),
                        proof: CompatibilityProof {
                            producer_type: format!("ecaax:promoted_upstream_{upstream}"),
                            consumer_type: format!(
                                "ecaax:promoted_consumer_{task_node_id}"
                            ),
                            rationale: Some(format!(
                                "SME-declared dependency from propose_hypothesized_node \
                                 upstream_atom_ids: {upstream} → {task_node_id}"
                            )),
                            ..CompatibilityProof::default()
                        },
                        chain_of_custody: None,
                    });
                }
                // Wire the promoted node downstream to the terminal
                // reporting atom (if present) so the lowered Task
                // reaches `final_reporting` instead of dangling.
                // Prefer `reporting` (the standard archetype's report
                // atom that feeds final_reporting); fall back to
                // `final_reporting` directly when the archetype was
                // pruned down. For Tier-B minimum-DAGs that have
                // neither, leave the node as a terminal sink — the
                // SME's `propose_hypothesized_node` IS the deliverable
                // in that case.
                let downstream = if dag.nodes.iter().any(|n| n.id == "reporting") {
                    Some("reporting")
                } else if dag.nodes.iter().any(|n| n.id == "final_reporting") {
                    Some("final_reporting")
                } else {
                    None
                };
                if let Some(downstream_id) = downstream {
                    let already = dag.edges.iter().any(|e| {
                        e.from_node == task_node_id && e.to_node == downstream_id
                    });
                    if !already {
                        dag.edges.push(EdgeContract {
                            from_node: task_node_id.clone(),
                            from_port: "default".to_string(),
                            to_node: downstream_id.to_string(),
                            to_port: "default".to_string(),
                            proof: CompatibilityProof {
                                producer_type: format!(
                                    "ecaax:promoted_producer_{task_node_id}"
                                ),
                                consumer_type: format!(
                                    "ecaax:reporting_consumer_{downstream_id}"
                                ),
                                rationale: Some(format!(
                                    "Default downstream wiring: promoted \
                                     hypothesized atom {task_node_id} feeds \
                                     {downstream_id}"
                                )),
                                ..CompatibilityProof::default()
                            },
                            chain_of_custody: None,
                        });
                    }
                }
                // Synthesize a `validate_<id>` wrapper task so the
                // lowered Task DAG carries the same validate companion
                // that registry atoms get via `builder::emit_stage`.
                // Without this, custom-DAG packages emit the promoted
                // node but no `validate_<node_id>` task — the audit
                // across 15 recent packages found `validate_doublet_score`
                // etc. missing while registry atoms got their
                // validators. The validator node mirrors the parent's
                // `validators` list and an edge from parent → wrapper
                // gives the workflow_json lowering the `depends_on:
                // [<id>]` shape it expects.
                use ecaa_workflow_core::workflow_contracts::implementation::Implementation;
                use ecaa_workflow_core::workflow_contracts::lifecycle::LifecycleState;
                use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;
                use ecaa_workflow_core::workflow_contracts::evidence::ValidatorRef;
                let validate_id = format!("validate_{task_node_id}");
                if !dag.nodes.iter().any(|n| n.id == validate_id) {
                    let mut validator_node = TaskNode::skeleton(
                        validate_id.clone(),
                        format!("Validate outputs of: {}", proposal.intent),
                    );
                    validator_node.lifecycle_state = LifecycleState::Contracted;
                    validator_node.implementation = Implementation::Unimplemented;
                    validator_node.validators = proposal
                        .validation_tests
                        .iter()
                        .map(|id| ValidatorRef {
                            id: id.clone(),
                            version: None,
                            parameters: None,
                        })
                        .collect();
                    dag.nodes.push(validator_node);
                }
                let validate_edge_wired = dag.edges.iter().any(|e| {
                    e.from_node == task_node_id && e.to_node == validate_id
                });
                if !validate_edge_wired {
                    dag.edges.push(EdgeContract {
                        from_node: task_node_id.clone(),
                        from_port: "default".to_string(),
                        to_node: validate_id.clone(),
                        to_port: "default".to_string(),
                        proof: CompatibilityProof {
                            producer_type: format!(
                                "ecaax:promoted_producer_{task_node_id}"
                            ),
                            consumer_type: format!(
                                "ecaax:validator_consumer_{validate_id}"
                            ),
                            rationale: Some(
                                "validator wrapper for promoted hypothesized atom"
                                    .to_string(),
                            ),
                            ..CompatibilityProof::default()
                        },
                        chain_of_custody: None,
                    });
                }
            } else {
                tracing::warn!(
                    session_id = %session_id,
                    proposal_id = %pid_for_update,
                    task_node_id = %task_node_id,
                    "signoff_proposal: workflow_dag is None; node will be picked up by next rebuild_dag",
                );
            }
            // 5. Mark the proposal Promoted.
            if let Some(p_mut) = session.proposals.get_mut(&pid_for_update) {
                p_mut.lifecycle = ProposalLifecycle::Promoted {
                    task_node_id: task_node_id.clone(),
                };
                p_mut.last_transition_at =
                    ecaa_workflow_core::hypothesized_proposal::now_ts();
            }
            let _ = outcome_writer.set(SignoffOutcome::Promoted(Box::new(PromotedPayload {
                task_node_id,
                package_path: session.emitted_package_path.clone(),
                new_state: session.state.clone(),
                title: session.title.clone(),
            })));
            Ok(())
        })
        .await;

    if let Err(e) = store_result {
        let msg = e.to_string();
        // Typed `ApiError` envelope; UI branches on
        // `code` rather than substring-matching the body.
        if msg.contains("no session") || msg.contains("not found") {
            return crate::error::ApiError::NotFound(msg).into_response();
        }
        return crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response();
    }

    // `Arc::try_unwrap` succeeds iff the closure has dropped its
    // Arc<OnceLock> clone (the FnOnce ran to completion). The
    // `OnceLock::into_inner` then yields the captured outcome.
    let outcome = std::sync::Arc::try_unwrap(outcome_cell)
        .ok()
        .and_then(|cell| cell.into_inner());

    match outcome {
        Some(SignoffOutcome::NotFound) => {
            (StatusCode::NOT_FOUND, "proposal not found").into_response()
        }
        Some(SignoffOutcome::WrongState { current_kind }) => (
            StatusCode::CONFLICT,
            format!(
                "proposal lifecycle is `{}`; only `awaiting_signoff` may be signed off",
                current_kind
            ),
        )
            .into_response(),
        Some(SignoffOutcome::Promoted(payload)) => {
            let PromotedPayload {
                task_node_id,
                package_path,
                new_state,
                title,
            } = *payload;
            // Fire rebuild_dag so the freshly-spliced promoted node gains
            // proof-carrying edges immediately (before the next user turn).
            // Soft-fail: rebuild errors are logged but never roll back the
            // signoff. The promoted-node re-injection pass inside
            // `rebuild_dag` ensures the node survives any subsequent
            // rebuild even if this eager rebuild races or fails.
            let _ = app.conversation.rebuild_dag_after_signoff(session_id).await;
            // Fire-and-forget git commit hook. Mirrors the confirm
            // hook in `turns.rs` and the branch hook in
            // `branches.rs` — runs in spawn_blocking so the request
            // path isn't tied to git I/O; a hook failure logs but
            // never rolls back the signoff.
            if let Some(pkg) = package_path {
                let cfg = app.git_config().read().clone();
                let sid_str = session_id.to_string();
                let subject = title.unwrap_or_else(|| {
                    format!("proposal {} promoted to {}", pid.as_str(), task_node_id)
                });
                let app_for_drop = app.clone();
                let drop_notifier: DropNotifier = Arc::new(move |trigger: &str, reason: &str| {
                    app_for_drop.spawn_fanout(
                        session_id,
                        SsePayload::ProvenanceCommitDropped {
                            trigger: trigger.to_string(),
                            reason: reason.to_string(),
                        },
                    );
                });
                app.git_hook_pool.spawn_with_sink(
                    "amend",
                    move || {
                        crate::git_routes::service::hook_commit(
                            &cfg, &pkg, "amend", &subject, &sid_str,
                        );
                        Ok(())
                    },
                    Some(drop_notifier),
                );
            }
            // SSE: ProposalPromoted + StateAdvanced (mirrors the
            // confirm endpoint's "broadcast state on transition"
            // pattern from turns.rs).
            app.broadcast(
                session_id,
                SsePayload::ProposalPromoted {
                    proposal_id: pid.clone(),
                    task_node_id,
                },
            )
            .await;
            app.broadcast(session_id, SsePayload::StateAdvanced { new_state })
                .await;
            StatusCode::NO_CONTENT.into_response()
        }
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "signoff handler produced no outcome".to_string(),
        )
            .into_response(),
    }
}

/// `POST /api/chat/session/:id/proposal/:proposal_id/reject` — SME
/// reject. Refuses with `409` when the proposal lifecycle is already
/// terminal (`Promoted` / `Rejected`); on success marks the proposal
/// `Rejected`, appends a `DecisionType::ProposalRejected` audit row,
/// and broadcasts a `ProposalRejected` SSE event.
pub(super) async fn reject_proposal(
    State(app): State<ChatAppState>,
    Path((session_id, proposal_id)): Path<(Uuid, String)>,
    body: Option<BoundedJson<RejectRequest>>,
) -> impl IntoResponse {
    let pid = ProposalId(proposal_id);
    let rationale = body
        .and_then(|BoundedJson(b)| b.rationale)
        .filter(|s| !s.trim().is_empty());

    // See `signoff_proposal` for the OnceLock rationale (closure is
    // FnOnce — single write — and OnceLock avoids the Mutex-poison
    // ceremony without sacrificing Send + Sync).
    let outcome_cell: std::sync::Arc<std::sync::OnceLock<RejectOutcome>> =
        std::sync::Arc::new(std::sync::OnceLock::new());
    let outcome_writer = outcome_cell.clone();
    let pid_for_update = pid.clone();
    let rationale_for_update = rationale.clone();

    let store_result = app
        .conversation
        .store_handle()
        .update(session_id, move |session| {
            let Some(proposal) = session.proposals.get(&pid_for_update).cloned() else {
                let _ = outcome_writer.set(RejectOutcome::NotFound);
                return Ok(());
            };
            // Terminal-state guard: Promoted + Rejected are terminal;
            // Blocked is terminal-ish but the spec only mentions
            // Promoted/Rejected so we follow the spec literally.
            let already_terminal = matches!(
                proposal.lifecycle,
                ProposalLifecycle::Promoted { .. } | ProposalLifecycle::Rejected { .. }
            );
            if already_terminal {
                let _ = outcome_writer.set(RejectOutcome::WrongState {
                    current_kind: proposal.lifecycle.kind_str().to_string(),
                });
                return Ok(());
            }
            // Mark Rejected.
            if let Some(p_mut) = session.proposals.get_mut(&pid_for_update) {
                p_mut.lifecycle = ProposalLifecycle::Rejected {
                    rationale: rationale_for_update.clone(),
                };
                p_mut.last_transition_at = ecaa_workflow_core::hypothesized_proposal::now_ts();
            }
            // Audit record — push a `ProposalRejected` decision log entry.
            session.decisions.push(DecisionRecord::new(
                session_id.to_string(),
                DecisionType::ProposalRejected {
                    proposal_id: pid_for_update.0.clone(),
                    rationale: rationale_for_update.clone(),
                },
                DecisionActor::Sme,
                rationale_for_update.clone(),
            ));
            let _ = outcome_writer.set(RejectOutcome::Rejected);
            Ok(())
        })
        .await;

    if let Err(e) = store_result {
        let msg = e.to_string();
        // Typed `ApiError` envelope; UI branches on
        // `code` rather than substring-matching the body.
        if msg.contains("no session") || msg.contains("not found") {
            return crate::error::ApiError::NotFound(msg).into_response();
        }
        return crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response();
    }

    let outcome = std::sync::Arc::try_unwrap(outcome_cell)
        .ok()
        .and_then(|cell| cell.into_inner());

    match outcome {
        Some(RejectOutcome::NotFound) => {
            (StatusCode::NOT_FOUND, "proposal not found").into_response()
        }
        Some(RejectOutcome::WrongState { current_kind }) => (
            StatusCode::CONFLICT,
            format!(
                "proposal lifecycle is `{}`; only non-terminal proposals may be rejected",
                current_kind
            ),
        )
            .into_response(),
        Some(RejectOutcome::Rejected) => {
            app.broadcast(
                session_id,
                SsePayload::ProposalRejected {
                    proposal_id: pid.clone(),
                    rationale,
                },
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        None => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "reject handler produced no outcome".to_string(),
        )
            .into_response(),
    }
}

/// Outcome carried out of the signoff update closure so the
/// post-lock broadcast / git-hook fan-out runs with stable copies of
/// every value it needs. The `Promoted` payload is boxed so the enum
/// has uniform-ish variant sizes (the SessionState clone is large).
enum SignoffOutcome {
    NotFound,
    WrongState { current_kind: String },
    Promoted(Box<PromotedPayload>),
}

struct PromotedPayload {
    task_node_id: String,
    package_path: Option<std::path::PathBuf>,
    new_state: ecaa_workflow_conversation::SessionState,
    title: Option<String>,
}

/// Outcome carried out of the reject update closure. Mirrors
/// `SignoffOutcome` but Rejected carries no post-lock payload —
/// the SSE event payload is already available in scope.
enum RejectOutcome {
    NotFound,
    WrongState { current_kind: String },
    Rejected,
}

/// Route inventory for the doc-as-contract gate +
/// per-submodule `routes()` builder. `mod.rs::router()` merges every
/// submodule's builder into the single chat surface.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/chat/session/:id/proposals"),
    ("GET", "/api/chat/session/:id/proposal/:proposal_id"),
    (
        "POST",
        "/api/chat/session/:id/proposal/:proposal_id/signoff",
    ),
    ("POST", "/api/chat/session/:id/proposal/:proposal_id/reject"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/proposals",
            axum::routing::get(list_proposals),
        )
        .route(
            "/api/chat/session/:id/proposal/:proposal_id",
            axum::routing::get(get_proposal),
        )
        .route(
            "/api/chat/session/:id/proposal/:proposal_id/signoff",
            axum::routing::post(signoff_proposal),
        )
        .route(
            "/api/chat/session/:id/proposal/:proposal_id/reject",
            axum::routing::post(reject_proposal),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat_routes::test_support::{body_json, make_router};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use ecaa_workflow_core::hypothesized_proposal::HypothesizedProposal;
    use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;
    use tower::util::ServiceExt;

    /// Seed a session with a proposal in the requested lifecycle. The
    /// helper accepts a closure so tests can construct any
    /// `ProposalLifecycle` variant they need.
    async fn seed_session_with_proposal(
        app: &super::ChatAppState,
        node_id: &str,
        lifecycle: ProposalLifecycle,
    ) -> (Uuid, ProposalId) {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let mut proposal = HypothesizedProposal::new(
            node_id.to_string(),
            format!("Score {node_id}"),
            vec!["data:2603".into()],
            "SME asked for a custom score",
            vec![],
            vec![],
            vec!["p_value_in_unit_interval".into()],
            vec![],
        );
        proposal.lifecycle = lifecycle;
        let proposal_id = proposal.id.clone();
        let store = app.conversation.store_handle();
        let proposal_for_closure = proposal.clone();
        store
            .update(id, move |s| {
                s.proposals
                    .insert(proposal_for_closure.id.clone(), proposal_for_closure);
                // Provide a workflow_dag so the splice path is
                // exercised on signoff.
                s.workflow_dag = Some(WorkflowDag {
                    id: "wf-test".into(),
                    nodes: vec![],
                    edges: vec![],
                    assumptions: Default::default(),
                    source_template: None,
                });
                Ok(())
            })
            .await
            .unwrap();
        (id, proposal_id)
    }

    #[tokio::test]
    async fn signoff_from_awaiting_signoff_returns_204_and_materializes_node() {
        let (router, app) = make_router(vec![]).await;
        let (sid, pid) =
            seed_session_with_proposal(&app, "doublet_score", ProposalLifecycle::AwaitingSignoff)
                .await;

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/proposal/{}/signoff",
                sid, pid
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"sme_initials":"alan"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Inspect the session: proposal must be Promoted; workflow_dag
        // must contain a node whose id matches the promoted task_node_id.
        let session = app.conversation.get_session(sid).await.unwrap();
        let proposal = session.proposals.get(&pid).expect("proposal preserved");
        match &proposal.lifecycle {
            ProposalLifecycle::Promoted { task_node_id } => {
                assert!(!task_node_id.is_empty(), "task_node_id must be set");
                let dag = session.workflow_dag.as_ref().expect("dag present");
                assert!(
                    dag.nodes.iter().any(|n| &n.id == task_node_id),
                    "materialized node must be spliced into workflow_dag.nodes; \
                     dag node ids: {:?}",
                    dag.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
                );
            }
            other => panic!("expected Promoted lifecycle, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn signoff_from_pending_validation_returns_409() {
        let (router, app) = make_router(vec![]).await;
        let (sid, pid) =
            seed_session_with_proposal(&app, "doublet_score", ProposalLifecycle::PendingValidation)
                .await;

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/proposal/{}/signoff",
                sid, pid
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn reject_transitions_to_rejected_and_appends_audit() {
        let (router, app) = make_router(vec![]).await;
        let (sid, pid) =
            seed_session_with_proposal(&app, "doublet_score", ProposalLifecycle::PendingSandbox)
                .await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/proposal/{}/reject", sid, pid))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"rationale":"wrong direction"}"#))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        let session = app.conversation.get_session(sid).await.unwrap();
        let proposal = session.proposals.get(&pid).expect("proposal preserved");
        match &proposal.lifecycle {
            ProposalLifecycle::Rejected { rationale } => {
                assert_eq!(rationale.as_deref(), Some("wrong direction"));
            }
            other => panic!("expected Rejected lifecycle, got {:?}", other),
        }
        // The decisions Vec must contain a ProposalRejected row for
        // this proposal id.
        let has_audit = session.decisions.iter().any(|d| {
            matches!(
                &d.decision,
                DecisionType::ProposalRejected { proposal_id, .. } if proposal_id == pid.as_str()
            )
        });
        assert!(
            has_audit,
            "decision log must contain a ProposalRejected entry; got: {:?}",
            session
                .decisions
                .iter()
                .map(|d| &d.decision)
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn reject_from_promoted_returns_409() {
        let (router, app) = make_router(vec![]).await;
        let (sid, pid) = seed_session_with_proposal(
            &app,
            "doublet_score",
            ProposalLifecycle::Promoted {
                task_node_id: "doublet_score".into(),
            },
        )
        .await;

        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/proposal/{}/reject", sid, pid))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn get_proposals_returns_sorted_list() {
        let (router, app) = make_router(vec![]).await;
        let (sid, _) = app.conversation.start_session(false).await.unwrap();

        // Build three proposals with distinct created_at values out of
        // order so the sort-by-created_at guarantee is meaningful.
        let mut p1 = HypothesizedProposal::new(
            "node_a",
            "first",
            vec!["data:0001".into()],
            "r1",
            vec![],
            vec![],
            vec![],
            vec![],
        );
        p1.created_at = 100;
        let mut p2 = HypothesizedProposal::new(
            "node_b",
            "second",
            vec!["data:0002".into()],
            "r2",
            vec![],
            vec![],
            vec![],
            vec![],
        );
        p2.created_at = 50;
        let mut p3 = HypothesizedProposal::new(
            "node_c",
            "third",
            vec!["data:0003".into()],
            "r3",
            vec![],
            vec![],
            vec![],
            vec![],
        );
        p3.created_at = 200;

        let store = app.conversation.store_handle();
        store
            .update(sid, move |s| {
                s.proposals.insert(p1.id.clone(), p1);
                s.proposals.insert(p2.id.clone(), p2);
                s.proposals.insert(p3.id.clone(), p3);
                Ok(())
            })
            .await
            .unwrap();

        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/proposals", sid))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        let arr = body.as_array().expect("array body");
        assert_eq!(arr.len(), 3);
        let created_ats: Vec<i64> = arr
            .iter()
            .map(|p| p["created_at"].as_i64().unwrap())
            .collect();
        assert_eq!(
            created_ats,
            vec![50, 100, 200],
            "proposals must be sorted by created_at ascending"
        );
    }

    #[tokio::test]
    async fn signoff_emits_proposal_promoted_sse() {
        let (router, app) = make_router(vec![]).await;
        let (sid, pid) =
            seed_session_with_proposal(&app, "doublet_score", ProposalLifecycle::AwaitingSignoff)
                .await;

        // Subscribe BEFORE firing the signoff request so the
        // broadcast lands on a live receiver.
        let mut rx = app.broadcaster(sid).await.subscribe();

        let req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/proposal/{}/signoff",
                sid, pid
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);

        // Collect events until we see ProposalPromoted; cap iterations
        // so a bug can't hang the test.
        let mut saw_promoted = false;
        for _ in 0..8 {
            match tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await {
                Ok(Ok(envelope)) => match envelope.payload {
                    SsePayload::ProposalPromoted {
                        proposal_id: got_pid,
                        task_node_id,
                    } => {
                        assert_eq!(got_pid, pid);
                        assert!(!task_node_id.is_empty());
                        saw_promoted = true;
                        break;
                    }
                    _ => continue, // skip non-target events
                },
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }
        assert!(
            saw_promoted,
            "ProposalPromoted SSE event must fire on signoff"
        );
    }

    #[tokio::test]
    async fn get_proposals_returns_404_for_unknown_session() {
        let (router, _) = make_router(vec![]).await;
        let bogus = Uuid::new_v4();
        let req = Request::builder()
            .method("GET")
            .uri(format!("/api/chat/session/{}/proposals", bogus))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_proposal_returns_404_for_unknown_proposal_id() {
        let (router, app) = make_router(vec![]).await;
        let (sid, _) = app.conversation.start_session(false).await.unwrap();
        let req = Request::builder()
            .method("GET")
            .uri(format!(
                "/api/chat/session/{}/proposal/proposal-deadbeef0000",
                sid
            ))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    /// Signoff invokes rebuild_dag so the workflow_dag contains the
    /// promoted node immediately after the call (not just after the
    /// next user turn).
    ///
    /// Design context: `rebuild_dag` (via `try_build_via_composer`)
    /// replaces `session.workflow_dag` entirely from the composer's
    /// Output. Without the re-injection pass added by this fix, any
    /// rebuild triggered after signoff would silently evict the
    /// promoted node.
    ///
    /// This test seeds a session whose `workflow_dag` has one
    /// existing node, signs off a proposal, and then calls
    /// `rebuild_dag_after_signoff` directly to simulate the eager
    /// Rebuild the signoff route now fires. Because the test session
    /// has no taxonomy / classification, the composer fast-path
    /// returns early and the DAG is unchanged — so the assertion
    /// focuses on the invariant that holds in ALL cases: the promoted
    /// node is present in `workflow_dag.nodes` after the signoff +
    /// rebuild sequence completes.
    ///
    /// The *survivability* of the promoted node through a REAL
    /// rebuild (one where the composer actually runs) is tested by
    /// the re-injection pass in `tools::rebuild_dag`, which scans
    /// `session.proposals` for `Promoted` entries and splices their
    /// Materialized TaskNodes back in before returning. That path is
    /// exercised by `rebuild_dag` itself whenever classification and
    /// taxonomy are present; the test here focuses on the server-
    /// route / service integration.
    #[tokio::test]
    async fn signoff_invokes_rebuild_dag_so_workflow_dag_has_node_present_post_call() {
        let (_router, app) = make_router(vec![]).await;

        // Seed a session with one pre-existing node in the dag plus a
        // proposal in AwaitingSignoff.
        let (sid, _) = app.conversation.start_session(false).await.unwrap();
        let mut proposal = HypothesizedProposal::new(
            "doublet_score_rebuild",
            "Score doublet probability — rebuild test",
            vec!["data:2603".into()],
            "Reviewer follow-on: verify rebuild_dag re-injects promoted node",
            vec![],
            vec![],
            vec!["p_value_in_unit_interval".into()],
            vec![],
        );
        proposal.lifecycle = ProposalLifecycle::AwaitingSignoff;
        let proposal_id = proposal.id.clone();

        let store = app.conversation.store_handle();
        let proposal_for_closure = proposal.clone();
        store
            .update(sid, move |s| {
                s.proposals
                    .insert(proposal_for_closure.id.clone(), proposal_for_closure);
                // Seed the dag with one pre-existing node so we can
                // assert both the original AND the new node survive.
                use ecaa_workflow_core::hypothesized_proposal::proposal_to_transient_task_node;
                use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;
                // Build a sentinel node whose id we can check later.
                let sentinel = {
                    let mut p2 = HypothesizedProposal::new(
                        "existing_node",
                        "pre-existing node",
                        vec![],
                        "seed",
                        vec![],
                        vec![],
                        vec![],
                        vec![],
                    );
                    p2.lifecycle = ProposalLifecycle::AwaitingSignoff;
                    proposal_to_transient_task_node(&p2)
                };
                s.workflow_dag = Some(WorkflowDag {
                    id: "wf-rebuild-test".into(),
                    nodes: vec![sentinel],
                    edges: vec![],
                    assumptions: Default::default(),
                    source_template: None,
                });
                Ok(())
            })
            .await
            .unwrap();

        // --- Step 1: POST signoff. This splices the new node into
        // workflow_dag and calls rebuild_dag_after_signoff.
        // In test sessions without taxonomy/classification,
        // the rebuild is a no-op (soft-fail), but the node
        // from the splice is still present.
        let signoff_req = Request::builder()
            .method("POST")
            .uri(format!(
                "/api/chat/session/{}/proposal/{}/signoff",
                sid, proposal_id
            ))
            .header("content-type", "application/json")
            .body(Body::from(r#"{"sme_initials":"reviewer-test"}"#))
            .unwrap();
        // Use the router wired to `app` so the signoff sees the
        // session we seeded above.
        let fresh_router = crate::chat_routes::router(app.clone());
        let resp = tower::ServiceExt::oneshot(fresh_router, signoff_req)
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::NO_CONTENT,
            "signoff must return 204"
        );

        // --- Step 2: Assert the promoted node is in workflow_dag.
        let session = app.conversation.get_session(sid).await.unwrap();
        let proposal_after = session
            .proposals
            .get(&proposal_id)
            .expect("proposal must still be present");
        let task_node_id = match &proposal_after.lifecycle {
            ProposalLifecycle::Promoted { task_node_id } => task_node_id.clone(),
            other => panic!("expected Promoted lifecycle after signoff, got {:?}", other),
        };
        let dag = session
            .workflow_dag
            .as_ref()
            .expect("workflow_dag must be present");
        assert!(
            dag.nodes.iter().any(|n| n.id == task_node_id),
            "promoted node '{}' must appear in workflow_dag.nodes after signoff; \
             present ids: {:?}",
            task_node_id,
            dag.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
        assert!(
            dag.nodes.iter().any(|n| n.id == "existing_node"),
            "pre-existing 'existing_node' must still be in workflow_dag.nodes; \
             present ids: {:?}",
            dag.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
        );

        // --- Step 3: Call rebuild_dag_after_signoff again to simulate
        // a second rebuild (idempotency). In this test
        // session without taxonomy/classification the
        // rebuild is a soft-fail no-op, but the promoted
        // node must still be present afterwards.
        app.conversation
            .rebuild_dag_after_signoff(sid)
            .await
            .unwrap();
        let session2 = app.conversation.get_session(sid).await.unwrap();
        let dag2 = session2
            .workflow_dag
            .as_ref()
            .expect("workflow_dag must still be present after second rebuild");
        assert!(
            dag2.nodes.iter().any(|n| n.id == task_node_id),
            "promoted node '{}' must survive a second rebuild_dag_after_signoff call; \
             present ids: {:?}",
            task_node_id,
            dag2.nodes.iter().map(|n| &n.id).collect::<Vec<_>>()
        );
    }

    /// Documents the bug.
    ///
    /// Before the fix: `proposal.rs:163` mutated
    /// `session.workflow_dag` directly via `as_mut()` without calling
    /// `Session::invalidate_dag()`. The derived `session.dag` cache
    /// stayed populated with pre-splice structure → readers through
    /// `current_dag()` saw a stale DAG.
    ///
    /// This test reproduces the bare-splice pattern as it existed
    /// pre-fix and asserts the cache is left in the stale state.
    /// It's an *invariant of the bare primitive*: mutating
    /// `workflow_dag` via `.as_mut()` does NOT auto-invalidate. The
    /// production fix moves the splice site to
    /// `workflow_dag_mut()` (the RAII guard) — see the companion
    /// test below.
    #[tokio::test]
    async fn signoff_bare_splice_leaves_cache_stale_documents_rc18_bug() {
        use ecaa_workflow_conversation::session::Session;
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let mut s = Session::new(false);
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-rc18".into(),
            nodes: vec![TaskNode::skeleton("existing", "seed node")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });
        let _ = s.ensure_dag_cached();
        assert!(s.dag_cache_is_valid(), "setup: cache populated");

        // Bare-splice pattern (still accessible because `workflow_dag`
        // is a public field — a follow-up could privatize it and gate
        // mutations behind the guard).
        if let Some(dag) = s.workflow_dag.as_mut() {
            dag.nodes.push(TaskNode::skeleton("promoted", "spliced"));
        }

        assert!(
            s.dag_cache_is_valid(),
            "bare-splice path does NOT auto-invalidate — this is the foot-gun \
             RC-18 closes by routing production splices through workflow_dag_mut()"
        );
    }

    /// The fix.
    ///
    /// The `workflow_dag_mut()` RAII guard MUST invalidate the
    /// derived cache on drop, so a contributor who routes a
    /// `workflow_dag` mutation through the guard cannot accidentally
    /// leave the cache stale. This test fails the build if a future
    /// refactor accidentally removes the `Drop` impl or stops it
    /// calling `invalidate_dag`.
    ///
    /// Without the guard this test would not even compile —
    /// `workflow_dag_mut` would not exist.
    #[tokio::test]
    async fn signoff_guarded_splice_invalidates_derived_dag_cache() {
        use ecaa_workflow_conversation::session::Session;
        use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let mut s = Session::new(false);
        s.workflow_dag = Some(WorkflowDag {
            id: "wf-rc18".into(),
            nodes: vec![TaskNode::skeleton("existing", "seed node")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        });
        let _ = s.ensure_dag_cached();
        assert!(s.dag_cache_is_valid(), "setup: cache populated");

        // Guarded splice pattern — this is what proposal.rs does in
        // production.
        {
            if let Some(dag) = s.workflow_dag_mut().as_mut() {
                dag.nodes.push(TaskNode::skeleton("promoted", "spliced"));
            }
        } // guard drops here → invalidates cache

        assert!(
            !s.dag_cache_is_valid(),
            "RC-18 invariant: workflow_dag_mut() guard must invalidate \
             session.dag on drop"
        );
    }
}
