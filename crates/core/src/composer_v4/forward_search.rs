//! Forward reachability — given a `WorkflowIntent`'s available
//! `DataProductContract`s, walk the `AtomRegistry` until every atom
//! whose input ports unify with a reachable producer is enumerated.
//!
//! Bounded by `max_depth` (BFS layers) and `max_branches` (per-depth
//! emission cap, capping the per-layer fan-out so the search is
//! replayable in determinism tests).
//!
//! Deterministic — the `frontier` and `next` vectors are sorted at every
//! step (so the iteration order doesn't depend on insertion order), and
//! the output is finally sorted by `(depth, atom_id, port_index)` so the
//! whole search is byte-stable across runs given the same inputs.
//!
//! # API choice for unification
//!
//! The compatibility engine's public surface is
//! `CompatibilityEngine::prove(producer: &PortContract, consumer:
//! &PortContract, ctx: &PlanningContext)`. There is no public
//! `prove_consumes(dp, port)` helper today.
//!
//! Forward search needs to ask "does this `DataProductContract`
//! produced by some upstream node satisfy this atom's input port?"
//! Rather than extend the engine's surface, we **project a
//! `DataProductContract` into a synthesized `PortContract` and call the
//! existing `prove`**. This keeps the engine API small and reuses the
//! well-tested compatibility path.
//! The synthesized port mirrors the data product's semantic type +
//! biological facets; physical format / cardinality are coarser and
//! left at `None` / default since `prove` treats absent producer
//! facets as "Compatible w/ warning" per the engine's facet-unification
//! contract.

use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext as CompatibilityContext,
};
use crate::workflow_contracts::data_product::DataProductContract;
use crate::workflow_contracts::port::{PortContract, PortPrivacyClass};
use crate::workflow_contracts::workflow_intent::WorkflowIntent;
use std::collections::BTreeMap;

use super::atom_eligibility::is_v4_eligible;

/// One reachable producer port discovered by `forward_search`. The
/// `produced` field carries the `DataProductContract` derived from the
/// atom's output port via [`DataProductContract::from_port`] — that
/// contract is what feeds the next BFS layer's input-port unification.
///
/// `Ord` is hand-rolled (not derived) because `DataProductContract`
/// doesn't implement `Ord`. The sort key is `(depth, atom_id,
/// port_index)` — those three fields uniquely identify an entry within
/// one search call.
///
/// `Eq` is also hand-rolled (not derived) because `DataProductContract`
/// no longer implements `Eq` after v4 P6's `LocalExtensionMaturity`
/// graduation field (carries `f32 success_rate` inside
/// `GraduationCandidate`). Equality keys on the same `(depth, atom_id,
/// port_index)` triple the `Ord` impl uses — that's the load-bearing
/// identity here; `produced` is fully determined by the triple given a
/// stable atom registry.
#[derive(Clone, Debug)]
pub struct ForwardFrontierEntry {
    /// BFS depth at which the entry was first reached. Sorted-first.
    pub depth: u32,
    /// Atom id (from the registry).
    pub atom_id: crate::ids::AtomId,
    /// Index of the producing output port within the atom's
    /// `outputs` vec.
    pub port_index: u32,
    /// Synthesized data product the consumer would receive from this
    /// port if the atom executed.
    pub produced: DataProductContract,
}

impl PartialEq for ForwardFrontierEntry {
    fn eq(&self, other: &Self) -> bool {
        self.depth == other.depth
            && self.atom_id == other.atom_id
            && self.port_index == other.port_index
    }
}

impl Eq for ForwardFrontierEntry {}

impl Ord for ForwardFrontierEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.depth
            .cmp(&other.depth)
            .then_with(|| self.atom_id.cmp(&other.atom_id))
            .then_with(|| self.port_index.cmp(&other.port_index))
    }
}

impl PartialOrd for ForwardFrontierEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Sorted vector of [`ForwardFrontierEntry`]. Sorted by
/// `(depth, atom_id, port_index)` — the natural derived `Ord` of
/// `ForwardFrontierEntry`.
pub type ForwardFrontier = Vec<ForwardFrontierEntry>;

/// Walk forward from `intent.available_data` through every atom in
/// `atom_reg` whose input ports unify (per
/// `CompatibilityEngine::prove`) with some reachable producer.
///
/// `max_depth` bounds the number of BFS layers; layer 0 considers
/// atoms consuming `intent.available_data` directly. Each layer's
/// emissions are capped at `max_branches` to prevent combinatorial
/// blow-up on registries with broad applicability across atoms.
///
/// Returns a deterministic [`ForwardFrontier`] sorted by
/// `(depth, atom_id, port_index)`.
pub fn forward_search(
    intent: &WorkflowIntent,
    atom_reg: &AtomRegistry,
    max_depth: u32,
    max_branches: u32,
) -> ForwardFrontier {
    forward_search_with_opaque_observation(intent, atom_reg, max_depth, max_branches, None, None)
}

/// R1/R2 closure (closure-residuals plan Task 1.4) — same as
/// [`forward_search`] but threads an optional `OpaqueObservationSink` +
/// session id into the engine `CompatibilityContext` so Opaque-type
/// observations attribute to the right session and node. The node id is
/// mutated per-iteration before every `engine.prove()` call so a
/// downstream sink receives the correct attribution.
///
/// Bare callers (tests, CLI) keep the 4-arg form; the conversation
/// crate's `try_build_via_composer` reaches this entry point via
/// `compose_with_version_and_modalities_full` → `composer_v4::plan`.
pub fn forward_search_with_opaque_observation(
    intent: &WorkflowIntent,
    atom_reg: &AtomRegistry,
    max_depth: u32,
    max_branches: u32,
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
) -> ForwardFrontier {
    let engine = DeterministicCompatibilityEngine::new();
    let mut cctx = CompatibilityContext {
        opaque_observation_sink: opaque_sink,
        opaque_session_id: opaque_session_id.map(String::from),
        ..Default::default()
    };

    // Reachable: keyed by (atom_id, port_index) so a given output port
    // is recorded exactly once. The first time we reach a port wins
    // (we skip on `contains_key`), which is the lowest depth in
    // practice because we walk depth in monotonic order.
    let mut reachable: BTreeMap<(String, u32), ForwardFrontierEntry> = BTreeMap::new();

    // Frontier — every DataProductContract observed so far. Each layer
    // tries to consume from anything in this list. Sorted at every
    // step so iteration order is deterministic.
    let mut frontier: Vec<DataProductContract> = intent.available_data.clone();
    sort_dps(&mut frontier);

    for depth in 0..max_depth {
        let mut next: Vec<DataProductContract> = Vec::new();
        let mut emitted_this_depth: u32 = 0;

        // Project each frontier DP into a synthesized producer port
        // once per layer; reused across atom inputs.
        let producer_ports: Vec<PortContract> = frontier
            .iter()
            .map(synthesize_port_from_data_product)
            .collect();

        // BTreeMap iteration order = sorted-by-id, so atoms are visited
        // deterministically.
        'atoms: for (atom_id, atom) in atom_reg.iter() {
            // Skip atoms with unresolvable
            // `method_choice` (they would surface
            // `MethodChoiceUnresolved` in `validate_composition`) and
            // skip wide-spectrum agentic / benchmark atoms when the
            // intent declares a modality. See
            // `composer_v4::atom_eligibility::is_v4_eligible` for the
            // full criteria.
            if !is_v4_eligible(atom, atom_reg, intent) {
                continue;
            }
            // R1/R2 closure — set the per-atom node id so any
            // `engine.prove()` call below that observes a `SemanticType::Opaque`
            // attributes the observation to this atom.
            cctx.opaque_node_id = Some(atom_id.clone());
            for input_port in atom.inputs.iter() {
                // Does any reachable DP satisfy this input port?
                let satisfied = producer_ports.iter().any(|prod| {
                    matches!(
                        engine.prove(prod, input_port, &cctx),
                        CompatibilityResult::Compatible(_)
                            | CompatibilityResult::CompatibleWithAdapters { .. }
                    )
                });
                if !satisfied {
                    continue;
                }
                // Emit each output port of this atom that we haven't
                // recorded yet. Stop early if we hit the per-depth cap.
                for (out_index, output_port) in atom.outputs.iter().enumerate() {
                    let key = (atom_id.clone(), out_index as u32);
                    if reachable.contains_key(&key) {
                        continue;
                    }
                    let produced = DataProductContract::from_port(output_port, atom_id, depth);
                    reachable.insert(
                        key.clone(),
                        ForwardFrontierEntry {
                            depth,
                            atom_id: atom_id.as_str().into(),
                            port_index: out_index as u32,
                            produced: produced.clone(),
                        },
                    );
                    next.push(produced);
                    emitted_this_depth += 1;
                    if emitted_this_depth >= max_branches {
                        break 'atoms;
                    }
                }
                // Forward search's reachability semantics are relaxed:
                // once any one of an atom's input ports unifies with a
                // reachable producer,
                // we emit that atom's outputs as new reachable. The
                // backward search (Task 3.2) will tighten this with the
                // strict "all-inputs-satisfiable" check. Break out of
                // the per-atom input loop so a multi-input atom emits
                // its outputs once, not once per matching input.
                break;
            }
        }

        if next.is_empty() {
            break;
        }
        sort_dps(&mut next);
        frontier.extend(next);
        sort_dps(&mut frontier);
        // Dedupe by id (the from_port id is stable per
        // `<atom_id>:<port_name>@<depth>`).
        frontier.dedup_by(|a, b| a.id == b.id);
    }

    let mut out: Vec<ForwardFrontierEntry> = reachable.into_values().collect();
    out.sort();
    out
}

/// Synthesize a `PortContract` from a `DataProductContract`. Used to
/// feed `CompatibilityEngine::prove(producer, consumer, ctx)` without
/// extending the engine's public surface — see the module-level docs
/// for the API choice rationale. The same projection works for both
/// directions of the search:
///
/// - **Forward search** uses it as the *producer* side (Task 3.1):
///   "does this DataProduct produced by intake satisfy the consumer's
///   input port?" → `engine.prove(synthesized, atom_input, ctx)`.
/// - **Backward search** uses it as the *consumer* side (Task 3.2):
///   "does this atom's output port produce something the goal
///   consumes?" → `engine.prove(atom_output, synthesized, ctx)`.
///
/// `pub(crate)` so the backward-search module can reuse without
/// extending the engine's public API surface.
pub(crate) fn synthesize_port_from_data_product(dp: &DataProductContract) -> PortContract {
    PortContract {
        name: dp.id.clone(),
        semantic_type: dp.semantic_type.clone(),
        physical_format: None,
        structural_schema: dp.structural_schema.clone(),
        ontology_terms: Vec::new(),
        modality: None,
        organism: dp.biological.organism.clone(),
        genome_build: dp.biological.genome_build.clone(),
        annotation_version: dp.biological.annotation_version.clone(),
        coordinate_system: dp.biological.coordinate_system.clone(),
        units: None,
        normalization_state: dp
            .statistical
            .as_ref()
            .and_then(|s| s.normalization.clone()),
        statistical_state: None,
        privacy_class: match dp.privacy {
            crate::workflow_contracts::data_product::PrivacyClass::Public => {
                PortPrivacyClass::Public
            }
            crate::workflow_contracts::data_product::PrivacyClass::Internal => {
                PortPrivacyClass::Internal
            }
            crate::workflow_contracts::data_product::PrivacyClass::Sensitive => {
                PortPrivacyClass::Sensitive
            }
            crate::workflow_contracts::data_product::PrivacyClass::Phi => PortPrivacyClass::Phi,
            crate::workflow_contracts::data_product::PrivacyClass::Restricted => {
                PortPrivacyClass::Restricted
            }
        },
        cardinality: dp.cardinality.clone(),
        validators: Vec::new(),
        constraints: Vec::new(),
        facets: std::collections::BTreeMap::new(),
    }
}

/// Sort a `Vec<DataProductContract>` deterministically. Sort key is
/// `(id, semantic_type.stable_id())`. `DataProductContract` doesn't
/// derive `Ord` (no `Ord` on its nested option-of-float QC fields), so
/// the function picks the two id-bearing string fields that uniquely
/// identify the contracts produced by `forward_search` — both are
/// deterministic functions of `from_port`'s inputs, so the sort is
/// stable across runs.
///
/// `pub(crate)` so the backward-search module can reuse without
/// duplicating the sort key.
pub(crate) fn sort_dps(v: &mut [DataProductContract]) {
    v.sort_by(|a, b| {
        a.id.cmp(&b.id).then_with(|| {
            a.semantic_type
                .stable_id()
                .cmp(&b.semantic_type.stable_id())
        })
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::semantic_type::SemanticType;

    #[test]
    fn empty_intent_yields_empty_frontier() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent::default();
        let frontier = forward_search(&intent, &atom_reg, 4, 64);
        assert!(frontier.is_empty());
    }

    #[test]
    fn empty_registry_yields_empty_frontier() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent {
            available_data: vec![DataProductContract::sample_paired_fastq()],
            ..Default::default()
        };
        let frontier = forward_search(&intent, &atom_reg, 4, 64);
        assert!(frontier.is_empty());
    }

    #[test]
    fn synthesized_port_carries_facets() {
        let dp = DataProductContract::skeleton(
            "x",
            SemanticType::edam("data:0863", "Sequence alignment"),
        );
        let p = synthesize_port_from_data_product(&dp);
        assert_eq!(p.semantic_type.stable_id(), "data:0863");
    }
}
