//! Model selection policy — see remediation plan §6.1 and §A.S2.
//!
//! The imperative `if`s here are now thin wrappers
//! around [`registry::ModelRoutingTable`], which loads rules from
//! `config/model-policy.yaml` (embedded via `include_str!`). Adding a
//! new routing rule is a YAML row, not a Rust method. Behavior parity
//! with the pre-refactor path is asserted by the pin tests in
//! `tests` plus the registry tests.

pub mod registry;

use crate::session::Session;
use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
/// Anthropic model identifier used for routing and pricing.
///
/// Serializes to the snake_case API-name form (`sonnet_4_6`,
/// `opus_4_7`, …) so sidecar `per_model_*` map keys are stable
/// across renames.
pub enum ModelId {
    // Explicit renames so the snake_case serialization is unambiguous
    // around the digit boundary (`sonnet_4_6`, not `sonnet46`). These
    // show up as map keys in SessionMetrics.per_model_* BTreeMaps and
    // in the sidecar per_model field.
    /// Claude Sonnet 4.6 — default model for all turns.
    #[serde(rename = "sonnet_4_6")]
    Sonnet46,
    /// Claude Opus 4.6. Retained so historical sidecars written while
    /// Opus 4.6 was the escalation target still deserialize cleanly and
    /// their spend continues to roll up under `opus_cost_usd`. New
    /// escalations route to `Opus47` (see `ModelPolicy::choose_with_reason`).
    #[serde(rename = "opus_4_6")]
    Opus46,
    /// Claude Opus 4.7 — the current Opus escalation target. Same rate
    /// card as 4.6 ($5 input / $25 output / $6.25 5-min cache write /
    /// $0.50 cache read per MTok) with a newer tokenizer and slightly
    /// different tool-use behavior.
    #[serde(rename = "opus_4_7")]
    Opus47,
    /// Claude Haiku 4.5 — cheaper, faster, smaller-context model. Not
    /// currently reachable via `ModelPolicy::choose`; variant lands now
    /// so the pricing table + metrics pipeline are ready for a future
    /// budget-constrained routing rule.
    #[serde(rename = "haiku_4_5")]
    Haiku45,
}

impl ModelId {
    /// Return the Anthropic API model string (e.g. `"claude-sonnet-4-6"`).
    pub fn api_id(self) -> &'static str {
        match self {
            ModelId::Sonnet46 => "claude-sonnet-4-6",
            ModelId::Opus46 => "claude-opus-4-6",
            ModelId::Opus47 => "claude-opus-4-7",
            ModelId::Haiku45 => "claude-haiku-4-5-20251001",
        }
    }

    /// True for any Opus variant. Used by the metrics snapshot to
    /// aggregate Opus 4.6 + 4.7 into the legacy `opus_turns` /
    /// `opus_cost_usd` UI mirrors so the upgrade isn't a visual cliff
    /// for operators watching those rows.
    pub fn is_opus(self) -> bool {
        matches!(self, ModelId::Opus46 | ModelId::Opus47)
    }

    /// Enumerate every variant. Used by metrics tests to assert every
    /// model has a pricing entry — adding a new variant without adding
    /// pricing would otherwise silently fall through to `Sonnet46` rates.
    pub const ALL: &'static [ModelId] = &[
        ModelId::Sonnet46,
        ModelId::Opus46,
        ModelId::Opus47,
        ModelId::Haiku45,
    ];
}

/// Reason an Opus (or Haiku) routing decision deviated from the
/// default Sonnet path. Attributed in metrics so operators can see
/// which trigger dominates their spend. `None` in
/// `choose_with_reason` means Sonnet (no escalation).
///
/// `SideCall` was added alongside the routing-table
/// refactor — auto-title (Haiku 4.5) and remediation-proposer (Opus
/// 4.7) are both side-call routings now expressed as YAML rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EscalationReason {
    /// Session is in careful mode (`Session::careful_mode == true`).
    CarefulMode,
    /// Session is in `SessionState::Blocked` (first turn of each episode).
    Blocked,
    /// Classifier confidence was below the routing threshold.
    LowConfidence,
    /// Request originated from a side-call (auto-title, remediation, etc.).
    SideCall,
}

/// Thin wrapper around [`registry::ModelRoutingTable`].
///
/// Use `ModelPolicy::choose` / `ModelPolicy::choose_with_reason` rather than
/// calling the registry directly — the policy methods apply the session
/// context and side-call context in the canonical order.
pub struct ModelPolicy;

impl ModelPolicy {
    /// No length-based trigger: long sessions (20+ turns) are common
    /// and escalating the entire tail to Opus at ~1.67× Sonnet cost
    /// (Opus = $5/$25 input/output per MTok vs Sonnet $3/$15) yielded
    /// no matching quality signal. Remaining triggers below are rare
    /// by construction.
    pub fn choose(session: &Session) -> ModelId {
        Self::choose_with_reason(session).0
    }

    /// Same as [`choose`], but also reports WHY a non-default model
    /// was picked. Used by `metrics.rs` to attribute escalations. The
    /// second tuple element is `None` when Sonnet is selected (no
    /// escalation).
    ///
    /// Backed by [`registry::ModelRoutingTable::current`];
    /// `config/model-policy.yaml` declares the rules and ordering.
    pub fn choose_with_reason(session: &Session) -> (ModelId, Option<EscalationReason>) {
        let table = registry::ModelRoutingTable::current();
        let dec = table.resolve(&registry::EvalContext {
            session: Some(session),
            side_call_kind: None,
        });
        (dec.model, dec.reason)
    }

    /// Recommended model for a cheap, deterministic, read-only side call
    /// that is independent of the conversation turn (e.g. a future
    /// subagent that classifies an attachment type, summarizes a single
    /// artifact, or pre-screens an intake prose block before the main
    /// Sonnet turn runs). Haiku 4.5 is ~67% cheaper than Sonnet 4.6 on
    /// both input and output ($1/$5 per MTok vs $3/$15), and it does not
    /// need to participate in the main conversation's prompt cache — a
    /// separate lightweight model keeps the main prefix's cache key
    /// stable (see `shared/prompt-caching.md` → "Don't change models
    /// mid-conversation").
    ///
    /// This helper is a guardrail: the policy pin means the first
    /// subagent that shows up picks Haiku by default. Moving from
    /// Sonnet to Haiku later would require auditing every call-site;
    /// this reverses the default.
    ///
    /// Backed by the routing table (rule
    /// `side_call_kind == auto_title`).
    pub fn for_side_call() -> ModelId {
        let table = registry::ModelRoutingTable::current();
        table
            .resolve(&registry::EvalContext {
                session: None,
                side_call_kind: Some("auto_title"),
            })
            .model
    }

    /// Model used by the remediation proposer (`side_calls::
    /// remediation_proposer::propose_remediations`). Pinned to
    /// **Opus 4.7** — the proposer reasons about specific library /
    /// signal / error-class combinations against a closed taxonomy of
    /// 10 remediations, and the call only fires on a real failure
    /// (rare, and the SME is waiting). The reasoning quality matters
    /// more than the cost of a single ~$0.05 call. Routes through
    /// `record_side_call_usage` for the dedicated cost bucket.
    ///
    /// Backed by the routing table (rule
    /// `side_call_kind == remediation`).
    pub fn for_remediation_proposer() -> ModelId {
        let table = registry::ModelRoutingTable::current();
        table
            .resolve(&registry::EvalContext {
                session: None,
                side_call_kind: Some("remediation"),
            })
            .model
    }
}

/// Return the serde wire name of a `ModelId` (`sonnet_4_6`, `opus_4_7`,
/// `haiku_4_5`). Useful when a caller wants to surface the model id in
/// an HTTP response without pulling in `serde_json`. Falls back to the
/// Debug name when serde fails (unreachable in practice).
pub fn model_serde_name(m: ModelId) -> String {
    serde_json::to_value(m)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| format!("{:?}", m).to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{Session, SessionState};
    use scripps_workflow_core::classify::ClassificationResult;

    #[test]
    fn careful_mode_forces_opus() {
        let s = Session::new(true);
        assert_eq!(ModelPolicy::choose(&s), ModelId::Opus47);
    }

    #[test]
    fn default_is_sonnet() {
        let s = Session::new(false);
        assert_eq!(ModelPolicy::choose(&s), ModelId::Sonnet46);
    }

    #[test]
    fn low_confidence_escalates() {
        let mut s = Session::new(false);
        s.classification = Some(ClassificationResult {
            confidence: 0.1,
            ..Default::default()
        });
        assert_eq!(ModelPolicy::choose(&s), ModelId::Opus47);
    }

    #[test]
    fn high_confidence_stays_sonnet() {
        let mut s = Session::new(false);
        s.classification = Some(ClassificationResult {
            confidence: 0.9,
            ..Default::default()
        });
        assert_eq!(ModelPolicy::choose(&s), ModelId::Sonnet46);
    }

    #[test]
    fn blocked_state_escalates() {
        let mut s = Session::new(false);
        s.state = SessionState::Blocked {
            blockers: vec![],
            reason: "x".into(),
            recovery_hint: "y".into(),
            blocker_kind: None,
            context: None,
        };
        // First turn in the Blocked episode uses Opus.
        assert_eq!(ModelPolicy::choose(&s), ModelId::Opus47);
    }

    #[test]
    fn blocked_second_turn_drops_to_sonnet() {
        // A Blocked episode gets exactly ONE Opus turn. Subsequent turns
        // while the SME is still on the decision card drop back to
        // Sonnet at ~1/5× the cost. The `blocked_opus_escalation_consumed`
        // flag is set in `service::send_turn` after the first escalation.
        let mut s = Session::new(false);
        s.state = SessionState::Blocked {
            blockers: vec![],
            reason: "x".into(),
            recovery_hint: "y".into(),
            blocker_kind: None,
            context: None,
        };
        s.blocked_opus_escalation_consumed = true;
        assert_eq!(ModelPolicy::choose(&s), ModelId::Sonnet46);
    }

    #[test]
    fn reentering_blocked_resets_the_escalation_guard() {
        // A fresh `HostError` or `HarnessTaskBlocked` trigger enters a
        // NEW Blocked episode and must re-escalate to Opus on the first
        // turn. Covered by the transition layer's reset logic.
        use crate::session::StateTrigger;
        let mut s = Session::new(false);
        // Simulate consuming a prior episode.
        s.blocked_opus_escalation_consumed = true;
        s.state = SessionState::Emitted;
        // New block from harness → transitions.rs resets the flag.
        s.try_transition(StateTrigger::HarnessTaskBlocked {
            task_id: "preprocessing".into(),
            detail: "disk full".into(),
            blocker_kind: scripps_workflow_core::blocker::BlockerKind::HostError {
                message: "disk full".into(),
            },
        })
        .unwrap();
        assert!(
            !s.blocked_opus_escalation_consumed,
            "new Blocked episode must reset the escalation guard"
        );
        assert_eq!(ModelPolicy::choose(&s), ModelId::Opus47);
    }

    #[test]
    fn long_conversation_no_longer_escalates() {
        // Regression guard: a 30-turn session with no blockers and
        // decent classification confidence must stay on Sonnet. The
        // length-based trigger was removed — normal long sessions
        // should not pay Opus rates.
        let mut s = Session::new(false);
        for _ in 0..30 {
            std::sync::Arc::make_mut(&mut s.conversation).push(crate::session::Turn::user("x"));
        }
        s.classification = Some(ClassificationResult {
            confidence: 0.9,
            ..Default::default()
        });
        assert_eq!(ModelPolicy::choose(&s), ModelId::Sonnet46);
    }

    #[test]
    fn choose_with_reason_reports_escalation_triggers() {
        // Regression guard: every Opus path must report its trigger so
        // metrics.rs can count them.
        let careful = Session::new(true);
        assert_eq!(
            ModelPolicy::choose_with_reason(&careful),
            (ModelId::Opus47, Some(EscalationReason::CarefulMode))
        );

        let mut blocked = Session::new(false);
        blocked.state = SessionState::Blocked {
            blockers: vec![],
            reason: "x".into(),
            recovery_hint: "y".into(),
            blocker_kind: None,
            context: None,
        };
        assert_eq!(
            ModelPolicy::choose_with_reason(&blocked),
            (ModelId::Opus47, Some(EscalationReason::Blocked))
        );

        let mut low_conf = Session::new(false);
        low_conf.classification = Some(ClassificationResult {
            confidence: 0.1,
            ..Default::default()
        });
        assert_eq!(
            ModelPolicy::choose_with_reason(&low_conf),
            (ModelId::Opus47, Some(EscalationReason::LowConfidence))
        );

        let sonnet = Session::new(false);
        assert_eq!(
            ModelPolicy::choose_with_reason(&sonnet),
            (ModelId::Sonnet46, None)
        );
    }

    #[test]
    fn api_ids_are_correct() {
        assert_eq!(ModelId::Sonnet46.api_id(), "claude-sonnet-4-6");
        assert_eq!(ModelId::Opus46.api_id(), "claude-opus-4-6");
        assert_eq!(ModelId::Opus47.api_id(), "claude-opus-4-7");
        assert_eq!(ModelId::Haiku45.api_id(), "claude-haiku-4-5-20251001");
    }

    #[test]
    fn is_opus_covers_both_variants() {
        assert!(ModelId::Opus46.is_opus());
        assert!(ModelId::Opus47.is_opus());
        assert!(!ModelId::Sonnet46.is_opus());
        assert!(!ModelId::Haiku45.is_opus());
    }

    #[test]
    fn side_call_is_haiku_by_default() {
        // Regression guard for the §3.15 policy pin: a future subagent
        // that asks `ModelPolicy::for_side_call()` for its model must
        // get Haiku 4.5, not Sonnet. If someone "simplifies" this helper
        // by returning `Sonnet46` ("consistent with main path"), they
        // silently triple the per-call cost.
        assert_eq!(ModelPolicy::for_side_call(), ModelId::Haiku45);
    }

    #[test]
    fn all_variants_exhaustive() {
        // If a new variant is added, ModelId::ALL must be extended so the
        // metrics pricing-coverage test can fail loudly. This test pins
        // the count; bump it alongside ALL when adding a variant.
        assert_eq!(ModelId::ALL.len(), 4);
    }
}
