//! Proof-carrying planner — production entry point. Call
//! `composer_v4::planner::plan` directly or route through
//! `composer.rs::compose_v4_dispatch_full`.

pub mod atom_eligibility;
pub mod backward_search;
pub mod companion_synthesis;
pub mod dag_mutation;
pub mod discover_companion_synthesis;
pub mod forward_search;
pub mod meet_in_middle;
pub mod planner;
pub mod policy_gate;
pub mod reporting_consumer_synthesis;
pub mod scoring;

pub use backward_search::{
    backward_search, search_backward, BackwardRequirement, BackwardSearchInput, ResolvedAtom,
};
pub use forward_search::{forward_search, ForwardFrontier, ForwardFrontierEntry};
pub use meet_in_middle::{meet_in_the_middle, MeetResult};
pub use planner::{
    lift_to_workflow_dag, lower_dag_to_composition_result, plan, planning_context_for_goal,
    planning_context_for_goal_with_intake, planning_context_for_goal_with_modalities, rescore_dag,
};
pub use scoring::{ScoringTuple, ScoringValue};

use crate::workflow_contracts::data_product::DataProductContract;
use crate::workflow_contracts::outcome::{ComposeOutcome, ValidationReport};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};
use crate::workflow_contracts::workflow_intent::WorkflowIntent;

use crate::compatibility::engine::{AdapterPolicy, RiskMode};
use crate::sandbox_policy::SandboxPolicy;

/// Search and policy context the v4 planner consumes. The registry
/// snapshot + atom search wires through this struct.
#[derive(Debug, Clone, Default)]
pub struct PlanningContext {
    /// Intent.
    pub intent: WorkflowIntent,
    /// Available data.
    pub available_data: Vec<DataProductContract>,
    /// Registry snapshot id (atom registry version pin).
    pub atom_snapshot_id: Option<String>,
    /// Ontology snapshot id.
    pub ontology_snapshot_id: Option<String>,
    /// Adapter policy.
    pub adapter_policy: AdapterPolicy,
    /// Risk mode.
    pub risk_mode: RiskMode,
    /// Search limits — bounded so the planner is replayable in
    /// determinism tests.
    pub max_branches: u32,
    /// Max depth.
    pub max_depth: u32,
    /// Bound on top-K alternatives returned. `0` means "single
    /// alternative" (no ranking).
    pub max_alternatives: u32,
    /// Sandbox policy for per-node GeneratedCode refusal at compose
    /// time. When `None`, `SandboxPolicy::default_strict()`
    /// is applied — conservative default means any GeneratedCode node
    /// that hasn't passed human review is refused before the SME sees
    /// a confirmation prompt.
    pub sandbox_policy: Option<SandboxPolicy>,
    /// Additional (non-primary) modalities the SME
    /// requested. `intent.modality` carries the primary modality;
    /// `additional_modalities` carries the rest. When non-empty, the
    /// planner attempts a cross-omics archetype match (set-equality on
    /// `cross_omics_modalities`) before falling back to the
    /// single-modality matcher. Multi-modality scenarios (e.g.
    /// `cross_omics_rnaseq_proteomics`) require the namespaced
    /// parallel-pipeline scaffold the cross-omics archetypes encode.
    pub additional_modalities: Vec<String>,
    /// V3 assumption-policy table consulted by
    /// `classify_outcome_with_policy` to drive blocking decisions on
    /// unresolved assumptions. `None` falls back to the legacy
    /// "any unresolved blocks Production" behaviour. Loaded by
    /// `planner::planning_context_with_assumption_policy` from
    /// `config/assumption-policy.yaml`.
    ///
    /// PlanningContext is not Serialize/Deserialize today; the field
    /// is `Option<Arc<...>>` so it defaults to `None` for callers
    /// that haven't migrated to the policy-aware constructor. Wrapped
    /// in `Arc` so the planner can clone the context cheaply per
    /// composition attempt.
    pub assumption_policy: Option<std::sync::Arc<crate::assumption_policy::AssumptionPolicyTable>>,
    /// V4 modality-ontology coverage matrix consulted at
    /// nearest-term lookup, registry-import quarantine, and graduation-
    /// candidate parent-term selection. Annotation-only enforcement
    /// today (logs forbidden hits + downgrades trust on import);
    /// v4 P2 wires `decision_substrate::record(OntologyScopeChecked)`
    /// so the matrix becomes hard refusal. Same `Option<Arc<...>>`
    /// shape as `assumption_policy` for symmetry — defaults to `None`
    /// so existing call sites compile unchanged. Loaded by
    /// `planner::planning_context_with_ontology_scope` from
    /// `config/modality-ontology-coverage.yaml`.
    pub ontology_scope: Option<std::sync::Arc<crate::ontology_scope::OntologyScopeMatrix>>,
    /// V4 alignment validation × lifecycle
    /// promotion grid loaded from `config/promotion-gate-policy.yaml`.
    /// Consulted by `composer_v4::policy_gate::consult_promotion_gate`
    /// at every promotion attempt; F19 forbids ad-hoc promotion in
    /// code. `None` short-circuits the F11 refusal block in
    /// `classify_outcome_with_policy` (back-compat for historical
    /// sessions); same `Option<Arc<...>>` shape as `assumption_policy`.
    ///
    /// `PlanningContext` doesn't derive serde today, so a
    /// `#[serde(default, skip)]` attribute would not parse here —
    /// omitted to keep `cargo check` clean. If the context graduates
    /// to a serializable form, add the attribute at that point.
    pub promotion_gate: Option<std::sync::Arc<crate::promotion_gate_policy::PromotionGatePolicy>>,
    /// Directory containing per-archetype
    /// population-coverage YAML files (one file per workflow id at
    /// `<dir>/<workflow_id>.yaml`). When set, the planner's
    /// `classify_outcome_with_policy` consults the matching file for
    /// every source archetype in the composed DAG and emits a
    /// `RefusalKind::PopulationOutOfCoverage` outcome when the
    /// `WorkflowIntent.sample_cohort` falls outside the validated set
    /// and no in-scope `PopulationWaiver` covers the gap. `None`
    /// short-circuits the gate to "no check" — the planner's existing
    /// scoring/policy paths run unchanged.
    ///
    /// `PlanningContext` doesn't derive `Serialize`/`Deserialize`
    /// today, so the v3 P9 spec's `#[serde(default, skip)]` is
    /// intentionally omitted here — it would not parse as an
    /// attribute. If the context graduates to a serializable form,
    /// add `#[serde(default, skip)]` at that point.
    pub population_coverage_dir: Option<std::path::PathBuf>,
    /// v4 P5 (D5 / F20) — repair-strategy registry consulted by the
    /// planner's repair wiring when `meet_in_the_middle` returns
    /// `Disconnected` / `PartiallyConnected` with structured gaps.
    /// Defaults to `None` (the planner's repair pass is a no-op when
    /// unset); production server paths wire via
    /// `RepairRegistry::with_builtin()`. Wrapped in `Arc` so
    /// PlanningContext stays cheaply `Clone`-able per composition
    /// attempt.
    pub repair_registry: Option<std::sync::Arc<crate::repair::RepairRegistry>>,
    /// v4 P5 (D5 / F20) — risk-class ceiling for auto-application of
    /// repair proposals. Proposals whose
    /// `risk_class <= auto_attempt_risk_threshold` MAY auto-apply;
    /// everything above MUST emit substrate but MUST NOT mutate the
    /// DAG (the F20 invariant). Defaults to `LowAutoAttempt` so a
    /// forgotten configuration never silently auto-applies a
    /// medium/high-risk repair.
    pub auto_attempt_risk_threshold: crate::repair::RepairRiskClass,
    /// v3 §4 / v4 §4 Round-2 closure (R1/R2) — cross-session opaque
    /// observation sink forwarded into the engine `PlanningContext` at
    /// per-atom `prove()` call sites. Threaded from
    /// `compose_with_version_and_modalities_full`'s optional 8th param.
    /// `None` at bare-composer call sites (CLI `intake`, eval-baselines,
    /// tests); the conversation crate wires the concrete
    /// `OpaqueObservationSinkImpl` from `<sessions_dir>/<id>/_opaque_registry.jsonl`.
    /// `PlanningContext` doesn't derive serde today so the field has no
    /// `#[serde(default, skip)]` — same shape as `assumption_policy`.
    pub opaque_observation_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    /// v3 §4 / v4 §4 Round-2 closure (R1/R2) — session id paired with
    /// `opaque_observation_sink` so the sink can attribute cross-session
    /// aggregation correctly. Same `None`-default semantics as the sink.
    pub opaque_session_id: Option<String>,
}

impl PlanningContext {
    /// New.
    pub fn new(intent: WorkflowIntent) -> Self {
        Self {
            intent,
            max_branches: 64,
            max_depth: 32,
            max_alternatives: 3,
            ..Default::default()
        }
    }
}

/// One alternative composition produced by the planner. The
/// `score` is what the ranking algorithm produces; UI surfaces
/// `summary` directly.
///
/// `Eq` not derived because `WorkflowDag` carries `f64`
/// (`IterationDeclaration.threshold`); use stable JSON snapshots
/// for determinism comparisons.
///
/// `source` records which planner stage
/// produced the alternative. Today's values:
///
/// - `"archetype"` — `ArchetypeRegistry::find_match` produced a
///   matching archetype scaffold and the planner lifted it into a
///   typed DAG.
/// - `"search"` — forward/backward/meet-in-the-middle search
///   produced a connected (or partially-connected) DAG.
///
/// New stages may add new strings without breaking serde
/// compatibility (the field defaults to empty string for
/// historical-session compatibility). The UI's
/// `AlternativeDagComparisonCard` is decoupled from this field via
/// its own `AlternativeSummary` projection.
#[derive(
    Debug,
    Clone,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct RankedAlternative {
    /// Dag.
    pub dag: WorkflowDag,
    /// Score.
    pub score: ScoringTuple,
    /// Compact human-readable summary ("3-stage pipeline,
    /// 2 lossless adapters, GRCh38 throughout").
    pub summary: String,
    /// Which planner stage produced this alternative
    /// (`"archetype"` / `"search"` / future stages).
    #[serde(default)]
    pub source: String,
}

/// Planner output: a typed `ComposeOutcome` plus the ranked
/// alternatives for the SME's "compare alternatives" card.
///
/// Adds `Serialize` / `Deserialize` so the
/// determinism replay test can hash a JSON snapshot. `Eq` is not
/// derivable because `WorkflowDag` carries `f64`
/// (`IterationDeclaration.threshold`); compare via JSON snapshot
/// when needed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlannerResult {
    /// Primary.
    pub primary: ComposeOutcome,
    /// Alternatives.
    pub alternatives: Vec<RankedAlternative>,
}

/// Deterministic ranking of a set of `RankedAlternative`s by
/// scoring tuple, then by stable DAG id as the final tie-breaker.
/// Stable across calls — required for determinism.
pub fn rank_alternatives(alts: &mut [RankedAlternative]) {
    alts.sort_by(|a, b| a.score.cmp(&b.score).then_with(|| a.dag.id.cmp(&b.dag.id)));
}

/// Build a single-alternative `PlannerResult` from a manually
/// constructed `WorkflowDag`. Used by adapters that bridge the
/// v3 (backward-chain) composer into the v4 return shape.
pub fn single_alternative(
    dag: WorkflowDag,
    score: ScoringTuple,
    summary: impl Into<String>,
    report: ValidationReport,
) -> PlannerResult {
    let alt = RankedAlternative {
        dag: dag.clone(),
        score,
        summary: summary.into(),
        source: "manual".into(),
    };
    PlannerResult {
        primary: ComposeOutcome::ValidatedExecutableDag { dag, report },
        alternatives: vec![alt],
    }
}

/// Convenience: project a `TaskNode` slice into the
/// "production trust count" bucket for `ScoringTuple`. Used by the
/// per-alternative scorer.
pub fn count_production_nodes(nodes: &[TaskNode]) -> u32 {
    nodes
        .iter()
        .filter(|n| {
            matches!(
                n.lifecycle_state,
                crate::workflow_contracts::lifecycle::LifecycleState::Production
            )
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rank_alternatives_is_deterministic() {
        let mut alts = vec![
            RankedAlternative {
                dag: WorkflowDag {
                    id: "alt_b".into(),
                    ..Default::default()
                },
                score: ScoringTuple::default(),
                summary: "b".into(),
                source: "test".into(),
            },
            RankedAlternative {
                dag: WorkflowDag {
                    id: "alt_a".into(),
                    ..Default::default()
                },
                score: ScoringTuple::default(),
                summary: "a".into(),
                source: "test".into(),
            },
        ];
        rank_alternatives(&mut alts);
        // Tied scores → lexical tie-break by id.
        assert_eq!(alts[0].dag.id, "alt_a");
        assert_eq!(alts[1].dag.id, "alt_b");
    }

    #[test]
    fn single_alternative_wraps_dag_into_validated_outcome() {
        let dag = WorkflowDag {
            id: "x".into(),
            ..Default::default()
        };
        let result = single_alternative(
            dag,
            ScoringTuple::default(),
            "single",
            ValidationReport::default(),
        );
        assert!(matches!(
            result.primary,
            ComposeOutcome::ValidatedExecutableDag { .. }
        ));
        assert_eq!(result.alternatives.len(), 1);
    }
}
