//! Deterministic composer.
//!
//! Wires the AtomRegistry (S4.2) + ArchetypeRegistry (S6.7) into a
//! `CompositionResult` the builder can emit a DAG from. Two code
//! paths converge on the same result type:
//!
//! 1. **Archetype fast-path.** Score-based ranking via
//!    `ArchetypeRegistry::find_match`; 5% tie-surfacing per [DEC Q2.4].
//! 2. **Backward-chain over typed atoms** (S7.2 fallback). When no
//!    archetype matches the goal, walk producer atoms via
//!    `AtomRegistry::find_producers` from the goal's `(edam_data,
//! edam_format)`, recursing on `depends_on`. BETSY-style pruning
//!    prefers shorter chains; tie-break alphabetical by atom id.
//!    Recursion depth cap of 10 prevents pathological cycles.
//!    Uses `crates/core/src/edam.rs::is_subtype_of` for "is-a" lookup.
//!
//! After atom selection, the composer runs **six-item formal
//! validation** (S7.4):
//!
//! 1. Exclusion consistency — atom-level `excludes` against the
//!    composed atom set.
//! 2. Acyclicity — Kahn topological sort over `depends_on`.
//! 3. Goal reachability — ≥ 1 atom output matches the goal
//!    (`edam_data` + `edam_format`, subtype-aware).
//! 4. Input satisfiability — every `depends_on` resolves within the
//!    composition or to an intake-supplied input.
//! 5. Attribute resolution — `method_choice.deferred_to` references
//!    a Discovery atom in the composition.
//! 6. Gate well-formedness — `excludes:` entries reference real
//!    atoms in the registry.
//!
//! Determinism: every collection is BTreeMap-ordered + atoms emit
//! in archetype-declared order (or topologically-sorted-by-id for
//! the backward-chain path). 100× replay test
//! `compose_is_byte_deterministic_across_100_replays` locks the
//! contract.

use crate::archetype::ArchetypeAtomRef;
#[cfg(test)]
use crate::archetype_registry::ArchetypeRegistry;
use crate::atom::{AtomDefinition, ContainerSpec};
#[cfg(test)]
use crate::atom_registry::AtomRegistry;
use crate::goal_spec::GoalSpec;
use crate::ids::{AtomId, StageId};
#[cfg(test)]
use crate::intake_port_mapper::PortMappingRegistry;
use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;

mod backward_chain;
mod dispatch;
mod errors;
mod inheritance;
mod multi_modal;
mod slot_fill;
mod validation;
pub use dispatch::{
    compose, compose_with_version, compose_with_version_and_modalities,
    compose_with_version_and_modalities_full, compose_with_version_and_modality,
};
pub use errors::CompositionError;
pub use inheritance::{
    resolve_inheritance, FlattenedArchetype, InheritanceStep, INHERITANCE_DEPTH_CAP,
};
#[cfg(test)]
use slot_fill::apply_slot_fill;
pub use slot_fill::{
    compose_with_intake, IntakeContext, SlotBinding, SlotSource, LITERATURE_OPT_IN_ATOM_IDS,
};
#[cfg(test)]
use validation::validate_composition;

/// Resolve the effective container for a composed atom
/// using the precedence: atom override > archetype default >
/// `compute-profiles/profiles.yaml` profile default > package-level
/// default > host-mode (None).
///
/// `archetype_default` is currently unused (archetypes don't yet carry
/// `default_container`; that lands in S15.21 follow-up). The
/// `profile_default` slot accepts the per-stage profile container
/// resolved from `compute-profiles/profiles.yaml` per S15.23. When
/// every level returns None the function returns None and the agent
/// wrappers fall back to host mode (legacy path).
///
/// The composer threads the result onto `ComposedAtom::container` so
/// the builder can copy it onto `Task::container` without rerunning
/// the precedence chain.
pub fn resolve_task_container(
    atom: &AtomDefinition,
    archetype_default: Option<&ContainerSpec>,
    profile_default: Option<&ContainerSpec>,
) -> Option<ContainerSpec> {
    if let Some(c) = atom.preferred_container.as_ref() {
        return Some(c.clone());
    }
    if let Some(c) = archetype_default {
        return Some(c.clone());
    }
    if let Some(c) = profile_default {
        return Some(c.clone());
    }
    None
}

pub(crate) fn apply_atom_ref_overrides(
    atom: &AtomDefinition,
    aref: &ArchetypeAtomRef,
) -> AtomDefinition {
    let mut atom = atom.clone();
    if let Some(required_figures) = &aref.required_figures {
        atom.required_figures = required_figures.clone();
    }
    if let Some(plot_stage_id) = &aref.plot_stage_id {
        atom.plot_stage_id = Some(plot_stage_id.clone());
    }
    if let Some(expected_artifacts) = &aref.expected_artifacts {
        atom.expected_artifacts = expected_artifacts.clone();
    }
    if let Some(required_artifacts) = &aref.required_artifacts {
        atom.required_artifacts = required_artifacts.clone();
    }
    atom
}

/// One atom's place in the composed DAG, with the per-call wiring
/// the archetype declared on top of the atom's intrinsic shape.
/// The builder consumes this as the input to its Task emission.
#[derive(Debug, Clone, PartialEq)]
pub struct ComposedAtom {
    /// Stage id in the emitted DAG (atom_id by default; the
    /// archetype's `alias` field overrides).
    pub stage_id: StageId,
    pub atom: AtomDefinition,
    /// Depends on.
    pub depends_on: Vec<String>,
    /// True when the archetype declared this atom required AND its
    /// slot-fill check passed. Optional atoms whose slots couldn't
    /// be filled by an upstream producer or an SME-supplied intake
    /// field are silently dropped from the composition (per plan
    /// §S6.11) — when present in the result they are always
    /// `required: true`. Required atoms with unfilled slots surface
    /// `CompositionError::UnfilledRequiredSlot` instead of being
    /// included with a stub binding.
    pub required: bool,
    /// Slot-fill bindings recorded by the composer.
    /// Each entry names a slot (today modeled as the atom's primary
    /// `edam_data` input plus its `depends_on` ids) and the source
    /// that filled it: an upstream composed atom's output or an SME
    /// intake field via `PortMappingRegistry`.
    ///
    /// Empty when the legacy `compose()` entry point is used (no
    /// intake context supplied). Populated only by the new
    /// `compose_with_intake()` API.
    pub bindings: Vec<SlotBinding>,
    /// Resolved container per atom following the
    /// atom > archetype > profile > package > host precedence
    /// (`resolve_task_container`). The builder copies this onto
    /// `Task::container` at DAG-emit time so `WORKFLOW.json` is the
    /// reproducibility-bearing source of truth. `None` = host-mode.
    pub container: Option<ContainerSpec>,
}

/// Output of `compose()`. The builder consumes this; downstream
/// surfaces (rationale log, RO-Crate provenance) read the rest.
#[derive(Debug, Clone, PartialEq)]
pub struct CompositionResult {
    /// Archetype id the composer matched. Empty when the
    /// (future) backward-chain path produced the composition with
    /// no archetype in play.
    pub matched_archetype: Option<String>,
    /// Score the matched archetype scored against the goal. Used by
    /// the metrics layer to track distribution of close-call matches
    /// over time.
    pub match_score: u32,
    /// One entry per atom in the composition order. The builder
    /// emits Task entries in this exact order (stable, deterministic).
    pub atoms: Vec<ComposedAtom>,
    /// Goal the composition was built against, carried for the
    /// rationale log.
    pub goal: GoalSpec,
    /// Free-text rationale the composer assembled for the SME
    /// confirmation card. Mirrors the archetype's `sme_summary`
    /// today; future backward-chain path will synthesize a richer
    /// explanation.
    pub rationale: String,
    /// Typed per-atom selection rationales. One
    /// entry per `ComposedAtom`, indexed by `stage_id`. The free-text
    /// `rationale` field above is kept for the legacy SME card;
    /// the structured form here drives the new
    /// `AtomSelectionRationale` UI panel which renders the reason
    /// kind + score breakdown + provenance link per atom. Empty
    /// when the legacy `compose()` path (no rationale capture) is
    /// used; populated by the archetype + backward-chain composer
    /// paths.
    #[doc(hidden)]
    pub atom_rationales: BTreeMap<String, AtomSelectionRationale>,
    /// Coarse resource estimate aggregated from each
    /// atom's `resource_profile`. The SME confirmation card renders
    /// this so the cost preview lands before approval, not after.
    /// Sourced from atom-level coarse buckets (small/medium/…); the
    /// pilot subsystem produces a finer-grained projection at
    /// emit time when `SWFC_PILOT_ENABLED=1`.
    pub resource_estimate: ResourceEstimate,
}

/// Recorded policy decision row, persisted to
/// `runtime/policy-decisions.jsonl` at emit time and surfaced on
/// the Composition UI tab. One entry per (active bundle × node ×
/// failed/recorded check). v1/v2/v3 sessions never produce these;
/// v4 sessions populate them via the per-node policy gate.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct PolicyDecisionRecord {
    /// Active policy bundle id (e.g. `clinical_trial`, `phi_strict`).
    pub bundle_id: String,
    /// Policy check kind expressed as the snake_case string the UI
    /// renders directly (`require_pinned_containers`,
    /// `audit_trail_required`, etc.).
    pub kind: String,
    /// Node id the decision applies to. `None` for bundle-wide
    /// recorded decisions (audit-trail / human-signoff).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    /// Human-readable rationale; one short sentence.
    pub statement: String,
    /// `true` for blocking violations (refusal); `false` for
    /// recorded informational decisions.
    pub blocking: bool,
    /// Chain-of-custody for the record when the gated
    /// statement contains suppressed content. `None` for ordinary
    /// non-suppressed policy decisions. Older records without the
    /// field deserialize cleanly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub chain_of_custody: Option<crate::workflow_contracts::chain_of_custody::ChainOfCustody>,
}

/// Wrapper that bundles a `CompositionResult` with the v4-only
/// side-channel data the planner produces. Returned by
/// `compose_with_version_and_modalities_full` so callers can route
/// v4 sessions through the canonical `WorkflowDag → DAG` lowering
/// pass and persist proof-carrying sidecars at emit time. v1/v2/v3
/// sessions leave the v4-only fields empty/None.
#[derive(Debug, Clone)]
pub struct ComposerOutput {
    /// Always populated. The legacy `CompositionResult` shape that
    /// downstream code (rationale log, builder, RO-Crate emitter)
    /// already consumes.
    pub composition: CompositionResult,
    /// Populated only when the v4 planner produced a typed
    /// `WorkflowDag`. Threaded through to `build_dag_from_workflow_dag`
    /// for lowering and to the `runtime/proofs.jsonl` /
    /// `runtime/assumptions.jsonl` sidecars at emit time.
    pub workflow_dag: Option<crate::workflow_contracts::task_node::WorkflowDag>,
    /// Populated only when the v4 planner produced a typed outcome.
    /// `Some(ValidatedExecutableDag)` for clean v4 dispatches;
    /// `Some(DraftDag)` / `Some(PartialDag)` are returned to the
    /// caller through `CompositionError::ComposerV4OutcomeNotExecutable`
    /// today — the dispatch layer surfaces them via this field for
    /// the chat UI.
    pub compose_outcome: Option<crate::workflow_contracts::outcome::ComposeOutcome>,
    /// Top-K ranked alternatives from the v4 planner. Empty for
    /// non-v4 sessions or when only one composition was produced.
    pub ranked_alternatives: Vec<crate::composer_v4::RankedAlternative>,
    /// Per-node policy decisions recorded during v4 composition.
    /// Surfaces in `runtime/policy-decisions.jsonl` and the
    /// Composition UI tab. Empty for non-v4 sessions and v4
    /// sessions with no active policy bundle.
    pub policy_decisions: Vec<PolicyDecisionRecord>,
}

impl ComposerOutput {
    /// Wrap a legacy `CompositionResult` (v1/v2/v3 path) without v4
    /// side-channel data.
    pub fn legacy(composition: CompositionResult) -> Self {
        Self {
            composition,
            workflow_dag: None,
            compose_outcome: None,
            ranked_alternatives: Vec::new(),
            policy_decisions: Vec::new(),
        }
    }
}

/// Structured rationale for one atom's selection in
/// the composition. The free-text `CompositionResult::rationale`
/// remains the SME's primary surface; this typed form drives the
/// per-atom rationale chip in the new `AtomSelectionRationale` UI
/// panel and feeds the WRROC Tier-3 `prov:wasGeneratedBy` chain
/// that S6.14 emits. The 5-variant tagged enum mirrors the
/// `[DEC QS.4]` design decision.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct AtomSelectionRationale {
    /// Stage id of the composed atom (matches `ComposedAtom::stage_id`).
    pub stage_id: StageId,
    /// Atom id of the source atom (matches `AtomDefinition::id`).
    /// Distinct from `stage_id` when an alias was applied.
    pub atom_id: AtomId,
    /// Selection reason kind — one of the five tagged variants.
    pub reason: SelectionReason,
    /// Score the atom earned during slot-fill (0 when not scored;
    /// non-zero for archetype-driven and backward-chain paths).
    #[serde(default)]
    pub score: u32,
    /// Free-text explanation surfaced to the SME alongside the
    /// reason kind. One short sentence — UI truncates beyond a
    /// configured length.
    pub explanation: String,
}

/// Five reason kinds, tagged enum.
/// Externally tagged JSON (`{"kind": "archetype_required",...}`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SelectionReason {
    /// Archetype declared the atom required and the slot-fill check
    /// passed. The default selection path for the archetype fast path.
    ArchetypeRequired,
    /// Archetype declared the atom optional + a slot-fill candidate
    /// resolved. Carries the candidate id that won.
    OptionalSlotFilled { candidate_id: String },
    /// Backward-chain composer pulled the atom in to produce a goal
    /// type (or an upstream input). Carries the target type + the
    /// step number in the chain.
    BackwardChainGoalProducer { target_edam: String, step: u32 },
    /// Atom is a discovery (`role: discovery`) inserted ahead of an
    /// operation atom whose method is `deferred_to: discovery_*`.
    /// Standard self-discovery rule.
    DiscoveryForMethod { for_atom: String },
    /// Atom was kept in the composition because the SME's intake
    /// explicitly named the method (`set_intake_method`). The score
    /// chain is bypassed for SME-named atoms — they always include.
    SmeNamedMethod { method_prose_excerpt: String },
}

/// Aggregated resource estimate for a composition. Coarse
/// numeric form so the SME sees ballpark numbers instead of opaque
/// "moderate/large" buckets. Pilot sizing replaces these projections
/// with measured numbers at execution time; this is the pre-approval
/// preview.
///
/// Bucket → number table (intentionally conservative; tuned to
/// over-estimate slightly so the SME isn't surprised):
/// - `cpu`: light → 2, moderate → 8, heavy → 32, very_heavy → 64 cores
/// - `memory`: small → 4, medium → 16, large → 64, xl → 256 GB
/// - `runtime_class`: seconds → 0.01, minutes → 0.5, hours → 4 hr
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
pub struct ResourceEstimate {
    /// Sum of per-atom `memory` bucket midpoints, in GB.
    pub total_memory_gb: u32,
    /// Largest single-atom `memory` bucket midpoint, in GB. Bounds
    /// the worst-case instance footprint.
    pub peak_memory_gb: u32,
    /// Sum across atoms of `cpu_cores × runtime_hours`. Coarse
    /// rollup; the SME-facing card renders this as "≈ N core-hours".
    pub total_core_hours: f64,
    /// Count of atoms whose `resource_profile.gpu` is true.
    pub gpu_task_count: u32,
    /// Optional dollar projection — left `None` here (pilot's
    /// `estimate_cost_usd` populates it when SWFC_PILOT_ENABLED=1
    /// surfaces the per-atom cost rate).
    #[ts(optional)]
    pub estimated_cost_usd: Option<f64>,
}

/// Bucket → numeric helper. Free function so unit tests
/// don't have to construct a full composition.
pub(crate) fn aggregate_resources(atoms: &[ComposedAtom]) -> ResourceEstimate {
    let mut e = ResourceEstimate::default();
    for ca in atoms {
        let Some(rp) = &ca.atom.resource_profile else {
            continue;
        };
        let mem_gb = match rp.memory.as_deref() {
            Some("small") => 4,
            Some("medium") => 16,
            Some("large") => 64,
            Some("xl") => 256,
            _ => 0,
        };
        e.total_memory_gb = e.total_memory_gb.saturating_add(mem_gb);
        e.peak_memory_gb = e.peak_memory_gb.max(mem_gb);
        let cpu_cores = match rp.cpu.as_deref() {
            Some("light") => 2.0,
            Some("moderate") => 8.0,
            Some("heavy") => 32.0,
            Some("very_heavy") => 64.0,
            _ => 0.0,
        };
        let hours = match rp.runtime_class.as_deref() {
            Some("seconds") => 0.01,
            Some("minutes") => 0.5,
            Some("hours") => 4.0,
            _ => 0.0,
        };
        e.total_core_hours += cpu_cores * hours;
        if rp.gpu {
            e.gpu_task_count = e.gpu_task_count.saturating_add(1);
        }
    }
    e
}

pub(crate) fn merged_depends_on(atom: &AtomDefinition, aref: &ArchetypeAtomRef) -> Vec<String> {
    if aref.depends_on.is_empty() {
        atom.depends_on.clone()
    } else {
        // Archetype-specified depends_on wins; atom's own depends_on
        // is the fallback when the archetype doesn't override.
        aref.depends_on.clone()
    }
}

#[cfg(test)]
mod tests;
