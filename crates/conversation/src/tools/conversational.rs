//! conversational-control tools.
//!
//! `propose_summary_confirmation` (gates the confirmation card) and
//! `propose_quick_replies` (renders the inline reply row). Neither
//! mutates `Session.dag` or `Session.intake_methods`; both are pure
//! UX-shaping surfaces.

use crate::errors::{ToolError, ToolResult};
use crate::session::Session;
use scripps_workflow_core::hash_utils::sha256_hex;
use std::path::Path;
use uuid::Uuid;

/// Hex SHA-256 digest of the LLM's summary
/// markdown. The same function is invoked from `service::tool_loop`
/// when the `ConfirmationCard` is populated and from `service::transitions`
/// when the SME's `/confirm` attaches the fingerprint to the durable
/// `DecisionType::Confirm` audit record. A single source means the two
/// fingerprints are always byte-identical for identical input.
///
/// Hash is over the raw UTF-8 bytes with no normalization — any change
/// to the displayed text (whitespace, capitalization, ordering) yields
/// a different fingerprint.
pub(crate) fn summary_hash_of(summary_markdown: &str) -> String {
    sha256_hex(summary_markdown.as_bytes())
}

pub(super) fn propose_summary_confirmation(
    session: &mut Session,
    summary_markdown: &str,
) -> ToolResult {
    if summary_markdown.trim().is_empty() {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: "summary_markdown is empty".into(),
            valid_alternatives: vec![],
            hint: "Provide a non-empty plain-language summary.".into(),
        });
    }
    // Refuse to put up the emit-confirmation card while
    // any hypothesized proposal is still pending SME action. Without
    // this guard the LLM races ahead to "ready to emit" while
    // proposal cards are sitting in `awaiting_signoff`; the SME
    // clicks Confirm on the summary, the package emits, and the
    // pending proposals never make it into the DAG. See the live-
    // session RCA.
    let pending: Vec<(&str, &str)> = session
        .proposals
        .values()
        .filter(|p| p.lifecycle.is_pending_sme())
        .map(|p| (p.node_id.as_str(), p.lifecycle.kind_str()))
        .collect();
    if !pending.is_empty() {
        let summary = pending
            .iter()
            .map(|(node, kind)| format!("{node} ({kind})"))
            .collect::<Vec<_>>()
            .join(", ");
        return ToolResult::err(ToolError::PreconditionFailure {
            reason: format!(
                "cannot raise emit-confirmation card while {} proposal(s) await SME action: {summary}",
                pending.len()
            ),
            hint: "Tell the SME to approve or reject each pending proposal card first. \
                   Do not call propose_summary_confirmation again until every proposal \
                   in session.proposals is in `Promoted` or `Rejected` lifecycle."
                .into(),
        });
    }
    // `StateTrigger::ProposeSummaryConfirmation`
    // (Intake → PendingConfirmation) fires from the dispatcher's
    // post-handler hook. The handler no longer calls `try_transition`
    // directly — see `tools/mod.rs::propose_summary_confirmation_post_ok`.
    // The hook is fire-and-forget; from terminal states it's a no-op.
    //
    // Deterministically derive `pending_emission_id` from the canonical
    // `current_summary_hash()`. The grant §G2 identifiability invariant
    // says "identical SME-confirmed plan bytes ⇒ identical emission id";
    // computing the id here, at propose-time, from the same hash that
    // `ConfirmationToken::authorizes` binds against guarantees the gate
    // cannot be re-armed behind the SME's back by re-rendering the same
    // summary, and that two independent sessions reaching the same plan
    // shape carry identical emission identities. UUIDv5 over the OID
    // namespace with the hex summary hash as input is the canonical
    // content-addressed UUID; idempotent across propose calls when the
    // plan shape is unchanged. Skip if the id is already set
    // (intentional pre-seed, or a re-propose mid-PendingConfirmation
    // after an upstream gate didn't clear it) so we don't churn the
    // identity on an unchanged plan.
    if session.pending_emission_id.is_none() {
        let summary_hash = session.current_summary_hash();
        let derived = Uuid::new_v5(&Uuid::NAMESPACE_OID, summary_hash.as_bytes());
        session.pending_emission_id = Some(derived);
    }
    ToolResult::ok(serde_json::json!({
        "rendered": "confirmation_card",
        "state": session.state,
        "summary_markdown": summary_markdown,
    }))
}

pub(super) fn propose_quick_replies(
    session: &Session,
    question: &str,
    options: &[String],
    config_dir: &Path,
) -> ToolResult {
    // Calibrated learning-to-defer override (Proposal E): when the
    // classifier has parked a tie window onto `pending_disambiguation`,
    // surface the registry's calibrated prompt + chips INSTEAD of the
    // LLM's natural-language follow-up. The LLM's `question` /
    // `options` arguments are discarded in this branch — by design,
    // the registry's wording is the source of truth for tie windows
    // we've calibrated against the corpus.
    if let Some(pair_id) = &session.pending_disambiguation {
        let disambig_path = config_dir.join("classifier-disambiguation.yaml");
        if disambig_path.exists() {
            match scripps_workflow_core::disambiguation::DisambiguationRegistry::load(
                &disambig_path,
            ) {
                Ok(reg) => {
                    if let Some(pair) = reg.pairs.iter().find(|p| p.id == *pair_id) {
                        let chips: Vec<serde_json::Value> = pair
                            .quick_replies
                            .iter()
                            .map(|q| {
                                serde_json::json!({
                                    "id": q.id,
                                    "label": q.label,
                                })
                            })
                            .collect();
                        return ToolResult::ok(serde_json::json!({
                            "rendered": "quick_reply_row",
                            "source": "disambiguation",
                            "pair_id": pair.id,
                            "question": pair.sme_prompt,
                            "options": chips,
                        }));
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        session_id = %session.id,
                        err = %e,
                        "propose_quick_replies: disambiguation registry load failed; \
                         falling back to LLM-supplied options"
                    );
                }
            }
        }
    }

    if question.trim().is_empty() {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: "question is empty".into(),
            valid_alternatives: vec![],
            hint: "Provide a non-empty clarification question.".into(),
        });
    }
    if options.is_empty() {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: "options list is empty".into(),
            valid_alternatives: vec![],
            hint: "Provide at least one quick-reply option.".into(),
        });
    }
    ToolResult::ok(serde_json::json!({
        "rendered": "quick_reply_row",
        "question": question,
        "options": options,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use scripps_workflow_core::hypothesized_proposal::{HypothesizedProposal, ProposalLifecycle};

    fn fresh_session() -> Session {
        Session::new(false)
    }

    fn add_proposal(session: &mut Session, node_id: &str, lifecycle: ProposalLifecycle) {
        let mut p = HypothesizedProposal::new(
            node_id,
            "intent",
            vec!["data:2603".into()],
            "rationale",
            vec![],
            vec![],
            vec![],
            vec![],
        );
        p.lifecycle = lifecycle;
        session.proposals.insert(p.id.clone(), p);
    }

    #[test]
    fn confirmation_refused_when_proposal_awaiting_signoff() {
        let mut s = fresh_session();
        add_proposal(
            &mut s,
            "guide_assignment",
            ProposalLifecycle::AwaitingSignoff,
        );
        let r = propose_summary_confirmation(&mut s, "summary");
        assert!(r.is_error, "must refuse while awaiting_signoff exists");
        let body = serde_json::to_string(&r.content).unwrap();
        assert!(
            body.contains("await SME action"),
            "hint must surface: {body}"
        );
        assert!(
            body.contains("guide_assignment"),
            "must name the pending node: {body}"
        );
    }

    #[test]
    fn confirmation_refused_when_proposal_blocked() {
        let mut s = fresh_session();
        add_proposal(
            &mut s,
            "grn_inference",
            ProposalLifecycle::Blocked {
                reason: scripps_workflow_core::hypothesized_proposal::ProposalBlockerReason::ValidatorFailed {
                    failures: vec!["unknown_check".into()],
                },
            },
        );
        let r = propose_summary_confirmation(&mut s, "summary");
        assert!(r.is_error, "Blocked must also block emit-confirm");
    }

    #[test]
    fn confirmation_passes_when_all_proposals_decided() {
        let mut s = fresh_session();
        add_proposal(
            &mut s,
            "fate_prediction",
            ProposalLifecycle::Promoted {
                task_node_id: "fate_prediction".into(),
            },
        );
        add_proposal(
            &mut s,
            "rejected_one",
            ProposalLifecycle::Rejected {
                rationale: Some("wrong shape".into()),
            },
        );
        let r = propose_summary_confirmation(&mut s, "summary");
        assert!(
            !r.is_error,
            "Promoted + Rejected must allow advance, got {:?}",
            r
        );
    }

    #[test]
    fn confirmation_passes_with_no_proposals() {
        let mut s = fresh_session();
        let r = propose_summary_confirmation(&mut s, "summary");
        assert!(!r.is_error);
    }

    #[test]
    fn derives_pending_emission_id_from_summary_hash() {
        // Identical fresh sessions ⇒ identical `current_summary_hash()`
        // ⇒ identical derived `pending_emission_id`. §G2 invariant.
        let mut a = fresh_session();
        let mut b = fresh_session();
        let _ = propose_summary_confirmation(&mut a, "summary");
        let _ = propose_summary_confirmation(&mut b, "summary");
        assert!(a.pending_emission_id.is_some());
        assert_eq!(a.pending_emission_id, b.pending_emission_id);
        // The derivation is the canonical UUIDv5 over NAMESPACE_OID of
        // the hex summary hash.
        let expected = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_OID,
            a.current_summary_hash().as_bytes(),
        );
        assert_eq!(a.pending_emission_id, Some(expected));
    }

    #[test]
    fn idempotent_when_pending_emission_id_already_set() {
        // Re-propose on the same session must not churn the emission id.
        let mut s = fresh_session();
        let _ = propose_summary_confirmation(&mut s, "summary");
        let first = s.pending_emission_id;
        let _ = propose_summary_confirmation(&mut s, "summary");
        assert_eq!(s.pending_emission_id, first);
    }

    #[test]
    fn early_return_paths_do_not_mint_emission_id() {
        // PreconditionFailure (pending proposal) returns before the
        // derivation runs; the id stays None.
        let mut s = fresh_session();
        add_proposal(
            &mut s,
            "guide_assignment",
            ProposalLifecycle::AwaitingSignoff,
        );
        let r = propose_summary_confirmation(&mut s, "summary");
        assert!(r.is_error);
        assert!(s.pending_emission_id.is_none());
    }
}
