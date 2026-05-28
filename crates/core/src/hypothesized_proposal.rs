//! Hypothesized
//! proposal lifecycle.
//!
//! When the chat tool `propose_hypothesized_node` accepts a proposal,
//! the runtime needs a typed record that tracks the three-gate
//! promotion pipeline (validator → sandbox → SME signoff). This module defines that record plus the
//! helpers that synthesize transient and materialized `TaskNode`s
//! from a proposal.
//!
//!
//! The lifecycle is distinct from [`crate::workflow_contracts::LifecycleState`]:
//! `LifecycleState` describes a node already living in the DAG;
//! `ProposalLifecycle` describes a proposal that has not yet been
//! materialized as a DAG node. The state machine:
//!
//! ```text
//! PendingValidation → PendingSandbox → AwaitingSignoff → Promoted
//! ↓ ↓ ↓
//! Blocked Blocked (none)
//! (any non-terminal stage may transition to Rejected via SME)
//! ```
//!
//! The transient `TaskNode` produced by [`proposal_to_transient_task_node`]
//! is **not** spliced into the workflow DAG; it is used only to feed
//! the existing gate machinery so the gate runner can
//! evaluate the proposal before the SME signs off. Materialization
//! (which *does* splice the node into the DAG) only happens after
//! signoff via [`proposal_to_materialized_task_node`].

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::sandbox_policy::SandboxRefusal;
use crate::workflow_contracts::evidence::ValidatorRef;
use crate::workflow_contracts::implementation::Implementation;
use crate::workflow_contracts::lifecycle::{LifecycleState, PromotionAuthority};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::{Provenance, TaskNode};

/// Stable id for a [`HypothesizedProposal`]. Conventionally formatted
/// as `proposal-<12 hex chars>` but the constructor accepts any
/// non-empty string so test scaffolding + future generators can
/// override.
#[derive(
    Debug,
    Clone,
    Serialize,
    Deserialize,
    TS,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct ProposalId(pub String);

impl ProposalId {
    /// Construct a fresh proposal id (`proposal-<12 hex>`). Mirrors the
    /// `lifecycle_uuid_short` helper used elsewhere in the conversation
    /// crate so ids stay short + URL-safe.
    pub fn generate() -> Self {
        let hex: String = Uuid::new_v4()
            .to_string()
            .chars()
            .filter(|c| c.is_ascii_hexdigit())
            .take(12)
            .collect();
        Self(format!("proposal-{hex}"))
    }

    /// Borrow the inner string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProposalId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for ProposalId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for ProposalId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl AsRef<str> for ProposalId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

/// Promotion-pipeline lifecycle of a proposal. Tagged with
/// `serde(tag = "kind")` so the UI can dispatch on `lifecycle.kind`
/// instead of guessing which variant is present.
///
/// `Promoted` / `Blocked` / `Rejected` are terminal — [`crate::hypothesized_proposal::ProposalLifecycle`]
/// callers (see `crates/conversation/src/proposal_gate.rs`) treat
/// them as no-ops when re-advancing.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProposalLifecycle {
    /// Gate runner has not yet run the validator gate.
    PendingValidation,
    /// Validator gate passed; sandbox gate pending.
    PendingSandbox,
    /// Validator + sandbox gates passed; awaiting SME approve / reject.
    AwaitingSignoff,
    /// SME approved; the proposal materialized as a [`TaskNode`] and
    /// was spliced into the workflow DAG. `task_node_id` is the id of
    /// the materialized node.
    Promoted { task_node_id: String },
    /// A gate failed or materialization threw; the proposal cannot
    /// progress until the SME re-proposes with the failure addressed.
    Blocked { reason: ProposalBlockerReason },
    /// SME rejected the proposal at a non-terminal stage. Carries an
    /// optional rationale captured from the SME at reject time.
    Rejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Rationale.
        rationale: Option<String>,
    },
}

impl ProposalLifecycle {
    /// Stable snake_case key. Mirrors the serde discriminator so
    /// callers (UI, audit log) can use the same string in both wire
    /// formats.
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::PendingValidation => "pending_validation",
            Self::PendingSandbox => "pending_sandbox",
            Self::AwaitingSignoff => "awaiting_signoff",
            Self::Promoted { .. } => "promoted",
            Self::Blocked { .. } => "blocked",
            Self::Rejected { .. } => "rejected",
        }
    }

    /// True when the lifecycle is terminal (no further transitions
    /// possible without re-proposal). Drives idempotency in
    /// `advance_proposal`.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Promoted { .. } | Self::Blocked { .. } | Self::Rejected { .. }
        )
    }

    /// True when the SME still owes a decision on this proposal —
    /// i.e. the proposal is in any state OTHER than `Promoted` or
    /// `Rejected`. Used by `propose_summary_confirmation` and
    /// `emit_package` to refuse advancing the session while
    /// proposals are pending SME action.
    ///
    /// Blocked counts as "pending" because a blocked proposal
    /// either needs explicit Reject or needs the LLM to re-propose
    /// with fixed validation_tests — silently emitting a package
    /// without the blocked capability is exactly the bug this
    /// guard exists to prevent.
    pub fn is_pending_sme(&self) -> bool {
        !matches!(self, Self::Promoted { .. } | Self::Rejected { .. })
    }
}

/// Why a proposal is in the [`ProposalLifecycle::Blocked`] state.
/// Surfaces in the chat-pane card as the SME-facing recovery hint.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProposalBlockerReason {
    /// One or more validation obligations declared in
    /// `validation_tests` failed.
    ValidatorFailed { failures: Vec<String> },
    /// The sandbox check refused the transient TaskNode.
    SandboxRefused { refusals: Vec<SandboxRefusal> },
    /// Post-signoff failure while constructing the materialized
    /// `TaskNode`. Rare; usually a planner gap that can't be
    /// surfaced at gate-run time.
    MaterializationFailed { reason: String },
    /// SME rejected the proposal (terminal, but recorded as
    /// `Rejected` rather than `Blocked`; this variant exists so the
    /// blocker enum is exhaustive on every refusal axis). Carries
    /// the same rationale shape as [`ProposalLifecycle::Rejected`].
    SmeRejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        /// Rationale.
        rationale: Option<String>,
    },
}

/// One per-gate outcome record. Append-only on
/// [`HypothesizedProposal::gate_outcomes`] so the chat-pane card can
/// render a per-gate timeline.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq, schemars::JsonSchema)]
#[ts(export)]
pub struct GateOutcome {
    /// Which gate produced this outcome.
    pub gate: GateName,
    /// True when the gate cleared the proposal; false when it
    /// produced refusals / failures.
    pub passed: bool,
    /// Free-form detail strings (one per refusal kind or obligation
    /// id). Stable formatting; sorted by the caller.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub details: Vec<String>,
    /// Unix-epoch seconds. Set by [`HypothesizedProposal::record_gate`]
    /// at the moment the outcome is recorded.
    pub recorded_at: i64,
}

/// Stable name of a gate in the promotion pipeline.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, TS, PartialEq, Eq, Hash, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum GateName {
    /// `validation_obligations` declared by the proposal
    /// must resolve to passing runs against the transient TaskNode.
    Validator,
    /// `sandbox_policy::check_generated_code_node` must
    /// return zero refusals against the transient TaskNode and the
    /// active sandbox policy.
    Sandbox,
    /// SME pressed Approve in the chat pane and the server
    /// recorded a [`PromotionAuthority`] entry.
    SmeSignoff,
}

/// The typed proposal record. Persisted on
/// `Session::proposals: BTreeMap<ProposalId, HypothesizedProposal>`
/// in the conversation crate.
#[derive(Debug, Clone, Serialize, Deserialize, TS, PartialEq, Eq, schemars::JsonSchema)]
#[ts(export)]
pub struct HypothesizedProposal {
    /// Stable id (`proposal-<12 hex>`).
    pub id: ProposalId,
    /// Snake_case identifier for the proposed node (e.g. `doublet_score`).
    pub node_id: String,
    /// One-sentence SME intent (what this node should do).
    pub intent: String,
    /// Parent ontology terms the LLM proposed for compatibility
    /// subsumption (EDAM or `swfc:<slug>`). Drives the synthesized
    /// output port's semantic type.
    pub parent_terms: Vec<String>,
    /// LLM-summarized assumptions the proposal relies on. Surfaces
    /// in the card; not gate-blocking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assumptions: Vec<String>,
    /// LLM-enumerated failure modes the validators are meant to
    /// catch. Surfaces in the card; not gate-blocking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failure_modes: Vec<String>,
    /// Validation obligation ids the SME / LLM has declared apply to
    /// this node. The validator gate runs these against the transient
    /// TaskNode.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validation_tests: Vec<String>,
    /// Atom-ids the proposed node depends on. When promoted these
    /// become the new node's `depends_on` so the lowered DAG has at
    /// least one upstream edge; without them the promoted node would
    /// render as an orphan with no input data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub upstream_atom_ids: Vec<String>,
    /// LLM's free-text rationale (from prior chat turns).
    pub llm_rationale: String,
    /// Current lifecycle state.
    pub lifecycle: ProposalLifecycle,
    /// Append-only gate-outcome timeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_outcomes: Vec<GateOutcome>,
    /// Unix-epoch seconds — proposal creation timestamp.
    pub created_at: i64,
    /// Unix-epoch seconds — last lifecycle transition or
    /// `record_gate` call.
    pub last_transition_at: i64,
}

impl HypothesizedProposal {
    /// Construct a new proposal seeded by the tool inputs. Generates
    /// a fresh [`ProposalId`] and stamps `created_at` /
    /// `last_transition_at` with the current Unix-epoch second.
    /// Lifecycle starts at [`ProposalLifecycle::PendingValidation`];
    /// the caller drives advancement via the gate runner.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        node_id: impl Into<String>,
        intent: impl Into<String>,
        parent_terms: Vec<String>,
        llm_rationale: impl Into<String>,
        assumptions: Vec<String>,
        failure_modes: Vec<String>,
        validation_tests: Vec<String>,
        upstream_atom_ids: Vec<String>,
    ) -> Self {
        let now = now_ts();
        Self {
            id: ProposalId::generate(),
            node_id: node_id.into(),
            intent: intent.into(),
            parent_terms,
            assumptions,
            failure_modes,
            validation_tests,
            upstream_atom_ids,
            llm_rationale: llm_rationale.into(),
            lifecycle: ProposalLifecycle::PendingValidation,
            gate_outcomes: Vec::new(),
            created_at: now,
            last_transition_at: now,
        }
    }

    /// Append a gate outcome and bump `last_transition_at`. The
    /// outcome's `recorded_at` is preserved as supplied; the caller
    /// should stamp it with [`now_ts`] at the moment of construction.
    pub fn record_gate(&mut self, outcome: GateOutcome) {
        self.last_transition_at = now_ts();
        self.gate_outcomes.push(outcome);
    }
}

/// Canonical port construction shared by
/// [`proposal_to_transient_task_node`],
/// [`proposal_to_materialized_task_node`], and
/// [`promoted_proposal_to_atom_definition`]. One synthesized **output**
/// [`PortContract`] per `parent_terms` entry. Naming convention:
/// single-term proposals produce a port named `output`; multi-term
/// proposals produce `output_1`, `output_2`, …
///
/// Centralized so the AtomRegistry overlay (Plan D) cannot drift from
/// the materialized TaskNode the signoff handler produces — if the
/// planner sees one set of ports during search and the spliced
/// TaskNode declares another, every EdgeContract proof against the
/// promoted node would be inconsistent.
fn proposal_ports(p: &HypothesizedProposal) -> Vec<PortContract> {
    p.parent_terms
        .iter()
        .enumerate()
        .map(|(idx, term)| {
            let name = if p.parent_terms.len() == 1 {
                "output".to_string()
            } else {
                format!("output_{}", idx + 1)
            };
            PortContract::with_semantic_type(name, SemanticType::edam(term, ""))
        })
        .collect()
}

/// Synthesize a transient [`TaskNode`] from a proposal so the
/// validator and sandbox gates can evaluate it before the SME signs off.
///
/// Shape:
/// - `lifecycle_state = LifecycleState::Hypothesized`
/// - `implementation = Implementation::Unimplemented`
/// - `validators` populated from `proposal.validation_tests` (each
///   tests-list entry becomes a `ValidatorRef { id, version: None,
/// parameters: None }`, mirroring the from-atom converter)
/// - Output ports built by [`proposal_ports`] — one per
///   `parent_terms` entry; the gate runners reach for ports when
///   reasoning about compatibility / postconditions, and a proposal
///   without ports would be effectively invisible to them.
///
/// The returned node is **not** inserted into the workflow DAG;
/// only [`proposal_to_materialized_task_node`] produces a node
/// intended for splicing in.
pub fn proposal_to_transient_task_node(p: &HypothesizedProposal) -> TaskNode {
    let outputs: Vec<PortContract> = proposal_ports(p);

    let validators: Vec<ValidatorRef> = p
        .validation_tests
        .iter()
        .map(|id| ValidatorRef {
            id: id.clone(),
            version: None,
            parameters: None,
        })
        .collect();

    let mut node = TaskNode::skeleton(p.node_id.clone(), p.intent.clone());
    node.lifecycle_state = LifecycleState::Hypothesized;
    node.implementation = Implementation::Unimplemented;
    node.validators = validators;
    node.outputs = outputs;
    node.provenance = Provenance {
        source: Some(format!("proposal:{}", p.id)),
        ..Provenance::default()
    };
    node
}

/// Materialize a proposal as a real DAG node after SME signoff.
/// Same shape as [`proposal_to_transient_task_node`] but
/// `lifecycle_state = LifecycleState::Contracted` and the
/// `PromotionAuthority` is appended to `provenance.promotion_history`
/// so the audit trail records who promoted the proposal and when.
///
/// The returned node is intended to be inserted into
/// `Session::workflow_dag.nodes`; the next `rebuild_dag` pass picks
/// it up as a planner candidate (its `Unimplemented` implementation
/// means the harness will refuse dispatch until later phases fill
/// the implementation — but the DAG composition / inspector / UI all
/// gain access to the new node immediately).
pub fn proposal_to_materialized_task_node(
    p: &HypothesizedProposal,
    authority: PromotionAuthority,
) -> TaskNode {
    let mut node = proposal_to_transient_task_node(p);
    node.lifecycle_state = LifecycleState::Contracted;
    node.provenance.promotion_history.push(authority);
    node
}

/// Synthesize an [`crate::atom::AtomDefinition`]
/// overlay row from a Promoted proposal so the v4 composer's
/// [`crate::atom_registry::AtomRegistry`] overlay surfaces the proposed
/// capability as a planner candidate.
///
/// Without this overlay, the planner only sees the YAML-loaded
/// registry atoms during forward/backward search; promoted-proposal
/// nodes spliced onto `session.workflow_dag` would be evicted on the
/// next `rebuild_dag` because the composer rebuilds the WorkflowDag
/// from scratch. With the overlay registered, the planner picks up the
/// synthesized atom as a legitimate candidate, places its TaskNode in
/// `workflow_dag.nodes`, AND edges it in via the `parent_terms`-derived
/// output ports — landing the promoted node in `session.dag.tasks`
/// during the canonical `WorkflowDag → DAG` lowering pass.
///
/// Returns `None` when the proposal is not in
/// [`ProposalLifecycle::Promoted`] lifecycle. Pre-promotion (validator
/// pending, sandbox pending, awaiting signoff) the proposal MUST NOT
/// influence planning — surfacing it as an atom before the SME signed
/// off would let the planner thread a downstream dependency through
/// an un-gated node. Rejected / Blocked proposals are similarly
/// excluded.
///
/// Shape parity with [`proposal_to_materialized_task_node`]:
/// - `id` equals `proposal.node_id` (NOT `proposal.id`); the planner
///   keys atoms by id, and the materialized TaskNode also uses
///   `node_id`. Lifecycle alignment requires both records to share
///   one stable handle.
/// - `outputs` produced by the shared [`proposal_ports`] helper; the
///   planner's [`crate::compatibility::engine::CompatibilityEngine::prove`]
///   call would reject the EdgeContract proof if the materialized
///   node's ports diverged from the overlay-atom's.
/// - `role = Computation`-equivalent — user-defined analysis steps,
///   not validators/discoveries. The closest [`crate::atom::AtomRole`]
///   variant is [`crate::atom::AtomRole::Operation`].
/// - `edam_operation` taken from the first `parent_terms` entry
///   that parses as `operation:NNNN`; falls back to a synthetic
///   `swfc:proposal_<node_id>` slug when no operation IRI is supplied
///   (the AtomDefinition field is non-optional).
/// - `attributes["_proposal_overlay"] = true` marker key so downstream
///   code can distinguish overlay atoms from registry atoms.
///
/// Every other field defaults to its empty / None / default form;
/// production atoms hand-author the full struct via the YAML
/// registry. The overlay only needs enough shape for the planner to
/// reason over ports + role.
pub fn promoted_proposal_to_atom_definition(
    proposal: &HypothesizedProposal,
) -> Option<crate::atom::AtomDefinition> {
    if !matches!(proposal.lifecycle, ProposalLifecycle::Promoted { .. }) {
        return None;
    }
    let outputs = proposal_ports(proposal);
    // Pick the first `parent_terms` entry that looks like an EDAM
    // operation IRI (`operation:NNNN`). If none qualifies, mint a
    // `swfc:` slug rooted on the proposal node id so the field stays
    // populated (the schema validator the YAML loader runs at registry
    // load is bypassed for overlays — they go in via
    // `with_promoted_overlay`, not `load_from_dir` — but downstream
    // consumers still read this field unconditionally).
    let edam_operation = proposal
        .parent_terms
        .iter()
        .find(|t| t.starts_with("operation:"))
        .cloned()
        .unwrap_or_else(|| format!("swfc:proposal_{}", proposal.node_id));
    let mut attributes: std::collections::BTreeMap<String, serde_json::Value> =
        std::collections::BTreeMap::new();
    attributes.insert("_proposal_overlay".to_string(), serde_json::json!(true));
    Some(crate::atom::AtomDefinition {
        id: proposal.node_id.clone(),
        version: "0.0.0".to_string(),
        role: crate::atom::AtomRole::Operation,
        discovery_kind: None,
        description: proposal.intent.clone(),
        edam_operation,
        edam_data: None,
        edam_format: None,
        assignee: crate::atom::AtomAssignee::Agent,
        depends_on: proposal.upstream_atom_ids.clone(),
        excludes: Vec::new(),
        attributes,
        joint_with: Vec::new(),
        inputs: Vec::new(),
        outputs,
        method_choice: None,
        resource_profile: None,
        preferred_container: None,
        claim_boundary: None,
        iterate: None,
        condition: None,
        required_figures: Vec::new(),
        plot_stage_id: None,
        figure_exempt: None,
        expected_artifacts: Vec::new(),
        required_artifacts: Vec::new(),
        validators: proposal.validation_tests.clone(),
        runtime_packages: crate::runtime_prereqs::RuntimePrereqs::default(),
        safety: crate::atom::SafetyPolicy::default(),
    })
}

/// Unix-epoch seconds. Centralized so `created_at`,
/// `last_transition_at`, and `GateOutcome::recorded_at` share one
/// clock source. `chrono::Utc::now().timestamp()` returns `i64` and
/// is already a workspace dep; using `chrono` keeps determinism /
/// log formatting consistent with the rest of the crate.
pub fn now_ts() -> i64 {
    chrono::Utc::now().timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proposal_id_round_trips() {
        let id = ProposalId::from("proposal-abc123");
        let s: String = id.0.clone();
        assert_eq!(id.as_str(), s.as_str());
        assert_eq!(format!("{id}"), s);
        assert_eq!(id.as_ref() as &str, s.as_str());
        let id2: ProposalId = "proposal-xyz".into();
        assert_eq!(id2.as_str(), "proposal-xyz");
    }

    #[test]
    fn proposal_id_generate_has_expected_shape() {
        let id = ProposalId::generate();
        assert!(id.as_str().starts_with("proposal-"));
        assert_eq!(id.as_str().len(), "proposal-".len() + 12);
        assert!(id
            .as_str()
            .trim_start_matches("proposal-")
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn lifecycle_kind_strs_are_stable() {
        assert_eq!(
            ProposalLifecycle::PendingValidation.kind_str(),
            "pending_validation"
        );
        assert_eq!(
            ProposalLifecycle::PendingSandbox.kind_str(),
            "pending_sandbox"
        );
        assert_eq!(
            ProposalLifecycle::AwaitingSignoff.kind_str(),
            "awaiting_signoff"
        );
        assert_eq!(
            ProposalLifecycle::Promoted {
                task_node_id: "x".into()
            }
            .kind_str(),
            "promoted"
        );
        assert_eq!(
            ProposalLifecycle::Rejected { rationale: None }.kind_str(),
            "rejected"
        );
        let blocked = ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::ValidatorFailed { failures: vec![] },
        };
        assert_eq!(blocked.kind_str(), "blocked");
    }

    #[test]
    fn lifecycle_is_terminal_matches_terminal_states() {
        assert!(!ProposalLifecycle::PendingValidation.is_terminal());
        assert!(!ProposalLifecycle::PendingSandbox.is_terminal());
        assert!(!ProposalLifecycle::AwaitingSignoff.is_terminal());
        assert!(ProposalLifecycle::Promoted {
            task_node_id: "x".into()
        }
        .is_terminal());
        assert!(ProposalLifecycle::Rejected { rationale: None }.is_terminal());
        assert!(ProposalLifecycle::Blocked {
            reason: ProposalBlockerReason::ValidatorFailed { failures: vec![] }
        }
        .is_terminal());
    }

    #[test]
    fn lifecycle_round_trips_through_serde() {
        let cases: Vec<ProposalLifecycle> = vec![
            ProposalLifecycle::PendingValidation,
            ProposalLifecycle::PendingSandbox,
            ProposalLifecycle::AwaitingSignoff,
            ProposalLifecycle::Promoted {
                task_node_id: "n1".into(),
            },
            ProposalLifecycle::Rejected {
                rationale: Some("wrong approach".into()),
            },
            ProposalLifecycle::Blocked {
                reason: ProposalBlockerReason::ValidatorFailed {
                    failures: vec!["p_value_in_unit_interval".into()],
                },
            },
        ];
        for lc in cases {
            let json = serde_json::to_string(&lc).unwrap();
            assert!(json.contains("\"kind\""), "missing kind tag: {json}");
            let back: ProposalLifecycle = serde_json::from_str(&json).unwrap();
            assert_eq!(lc, back);
        }
    }

    #[test]
    fn proposal_record_gate_bumps_last_transition_at() {
        let mut p = HypothesizedProposal::new(
            "doublet_score",
            "Score doublets",
            vec!["data:2603".into()],
            "rationale",
            vec![],
            vec![],
            vec!["p_value_in_unit_interval".into()],
            vec![],
        );
        let before = p.last_transition_at;
        // Force the clock forward so the timestamp comparison is
        // robust to sub-second test execution.
        p.last_transition_at = before - 10;
        p.record_gate(GateOutcome {
            gate: GateName::Validator,
            passed: true,
            details: vec![],
            recorded_at: before,
        });
        assert!(
            p.last_transition_at >= before,
            "expected last_transition_at to advance"
        );
        assert_eq!(p.gate_outcomes.len(), 1);
        assert!(matches!(p.gate_outcomes[0].gate, GateName::Validator));
    }
}
