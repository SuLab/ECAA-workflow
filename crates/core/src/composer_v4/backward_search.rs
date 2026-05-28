//! Backward goal-decomposition — given a `WorkflowIntent`'s
//! desired outputs, walk the `AtomRegistry` to find every
//! `(atom_id, port_index)` whose output port produces something
//! satisfying a goal `DataProductContract`. Each visited atom's input
//! ports become new subgoals enqueued at `depth + 1`, until
//! `max_depth` is exhausted or the queue empties. Bounded by
//! `max_branches` (per-layer emission cap). Output is sorted by
//! `(atom_id, port_index, depth)` for byte-stable replay.
//!
//! # API choice for unification
//!
//! Symmetric to [`super::forward_search`] (Task 3.1): we project the
//! goal `DataProductContract` into a synthesized `PortContract` via
//! [`super::forward_search::synthesize_port_from_data_product`] and
//! call the existing `engine.prove(producer_port, consumer_port, ctx)`.
//! Producer = the atom's real output port; consumer = the synthesized
//! goal port. `Compatible` / `CompatibleWithAdapters` → record the
//! requirement and enqueue the atom's input ports as new sub-goals
//! for the next BFS layer.
//!
//! # Determinism
//!
//! - Goals are sorted by `sort_dps` before enqueue.
//! - Atom registry iteration is `BTreeMap`-keyed (sorted by atom id).
//! - Per-layer emission cap (`max_branches`) is applied
//!   deterministically — first-N matches in sorted iteration order.
//! - The output `Vec<BackwardRequirement>` is sorted by the derived
//!   `Ord` (`(atom_id, port_index, depth)`) before return.
//!
//! # Cycle / depth bounds
//!
//! The search is BFS-bounded: an atom's outputs are recorded at most
//! once across the whole search (keyed by `(atom_id, port_index)`),
//! so revisits short-circuit on the dedupe check. Re-encountering the
//! same goal contract at a higher depth doesn't re-emit a requirement
//! either (because the dedupe is on atom output, not on goal id).
//! Layer count is capped by `max_depth`; per-layer fan-out is capped
//! by `max_branches`.

use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext as CompatibilityContext,
};
use crate::workflow_contracts::data_product::DataProductContract;
use crate::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};
use std::collections::{BTreeMap, VecDeque};

use super::atom_eligibility::is_v4_eligible;
use super::forward_search::{sort_dps, synthesize_port_from_data_product};

/// One required producer-port discovered by [`backward_search`]. The
/// requirement says "to satisfy the goal, some atom in the registry
/// (this one, at this output-port index) needs to run". The depth
/// records the BFS layer at which the requirement was first
/// discovered — depth-0 = direct producer of a top-level goal.
///
/// `Ord` is derived on `(atom_id, port_index, depth)` so `Vec`
/// outputs are byte-stable across runs given the same inputs.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct BackwardRequirement {
    /// Atom id (from the registry).
    pub atom_id: crate::ids::AtomId,
    /// Index of the producing output port within the atom's
    /// `outputs` vec.
    pub port_index: u32,
    /// BFS depth at which the requirement was first discovered.
    pub depth: u32,
}

/// Walk backward from `intent.desired_outputs` (mapped to typed
/// `DataProductContract`s via `lift_desired_output`) through every
/// atom in `atom_reg` whose output port unifies (per
/// `CompatibilityEngine::prove`) with a desired output. Each visited
/// atom's input ports become new subgoals enqueued at `depth + 1`,
/// until `max_depth` is exhausted or the queue empties.
///
/// `max_branches` is the per-layer emission cap; the search emits at
/// most `max_branches` requirements per BFS layer to keep the search
/// replayable in determinism tests and to bound combinatorial
/// blow-up on registries where many atoms produce a common
/// `data:*` type.
///
/// Returns a deterministic `Vec<BackwardRequirement>` sorted by
/// `(atom_id, port_index, depth)`.
pub fn backward_search(
    intent: &WorkflowIntent,
    atom_reg: &AtomRegistry,
    max_depth: u32,
    max_branches: u32,
) -> Vec<BackwardRequirement> {
    backward_search_with_opaque_observation(intent, atom_reg, max_depth, max_branches, None, None)
}

/// R1/R2 closure (closure-residuals plan Task 1.4) — sibling of
/// [`backward_search`] that threads an optional `OpaqueObservationSink`
/// plus session id into the engine `CompatibilityContext` so Opaque-type
/// observations attribute to the right session and per-atom node. The
/// node id is mutated immediately before each `engine.prove()` call so
/// a downstream sink receives the correct attribution.
pub fn backward_search_with_opaque_observation(
    intent: &WorkflowIntent,
    atom_reg: &AtomRegistry,
    max_depth: u32,
    max_branches: u32,
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
) -> Vec<BackwardRequirement> {
    let engine = DeterministicCompatibilityEngine::new();
    let mut cctx = CompatibilityContext {
        opaque_observation_sink: opaque_sink,
        opaque_session_id: opaque_session_id.map(String::from),
        ..Default::default()
    };

    // Required: keyed by (atom_id, port_index) so a given output port
    // is recorded exactly once across the whole search. The first
    // depth at which we reach it wins (BFS guarantees lowest-depth
    // first).
    let mut required: BTreeMap<(String, u32), BackwardRequirement> = BTreeMap::new();

    // BFS queue of (goal data product, depth). Initial goals come from
    // `intent.desired_outputs`; each `DesiredOutput` is lifted into a
    // typed `DataProductContract` via `lift_desired_output`.
    let mut queue: VecDeque<(DataProductContract, u32)> = VecDeque::new();
    let mut goals: Vec<DataProductContract> = intent
        .desired_outputs
        .iter()
        .filter_map(lift_desired_output)
        .collect();
    sort_dps(&mut goals);
    for goal in goals {
        queue.push_back((goal, 0));
    }

    // Visited goal ids: prevents the same DataProductContract id from
    // being re-expanded multiple times when several atoms enqueue the
    // same input contract. Without this, large registries would do
    // redundant proving on identical sub-goals.
    let mut visited_goals: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

    // Track per-layer emission count so the per-layer fan-out cap
    // (`max_branches`) applies symmetrically with `forward_search`.
    // We process the queue layer-by-layer (depth ascending) so we can
    // reset the counter at each layer boundary.
    let mut current_depth: u32 = 0;
    let mut emitted_this_layer: u32 = 0;

    while let Some((goal, depth)) = queue.pop_front() {
        if depth >= max_depth {
            continue;
        }
        if depth != current_depth {
            current_depth = depth;
            emitted_this_layer = 0;
        }
        if !visited_goals.insert(goal.id.clone()) {
            continue;
        }

        // Project the goal into a consumer-shaped port we can hand to
        // engine.prove(producer_atom_output, consumer_synthesized_goal).
        let goal_port = synthesize_port_from_data_product(&goal);

        // Walk the atom registry in id-sorted order (BTreeMap iter)
        // for determinism.
        'atoms: for (atom_id, atom) in atom_reg.iter() {
            // Skip atoms with unresolvable
            // `method_choice` and (when modality is set) wide-spectrum
            // agentic / benchmark atoms. See
            // `composer_v4::atom_eligibility::is_v4_eligible` for the
            // full criteria. Filtering here mirrors the forward-search
            // filter so the meet-in-middle assembly only considers
            // eligible atoms regardless of which side discovered them.
            if !is_v4_eligible(atom, atom_reg, intent) {
                continue;
            }
            // R1/R2 closure — attribute any Opaque observation emitted
            // by `engine.prove()` below to the producer atom we're
            // currently examining.
            cctx.opaque_node_id = Some(atom_id.clone());
            for (out_index, output_port) in atom.outputs.iter().enumerate() {
                let key = (atom_id.clone(), out_index as u32);
                if required.contains_key(&key) {
                    continue;
                }
                let result = engine.prove(output_port, &goal_port, &cctx);
                if !matches!(
                    result,
                    CompatibilityResult::Compatible(_)
                        | CompatibilityResult::CompatibleWithAdapters { .. }
                ) {
                    continue;
                }
                required.insert(
                    key.clone(),
                    BackwardRequirement {
                        atom_id: atom_id.as_str().into(),
                        port_index: out_index as u32,
                        depth,
                    },
                );
                emitted_this_layer += 1;
                // Enqueue this atom's input ports as new sub-goals
                // for the next BFS layer. Each input becomes a typed
                // `DataProductContract` via `from_port` so the next
                // layer's matching reuses the same engine path.
                for input_port in &atom.inputs {
                    queue.push_back((
                        DataProductContract::from_port(input_port, atom_id, depth + 1),
                        depth + 1,
                    ));
                }
                if emitted_this_layer >= max_branches {
                    break 'atoms;
                }
                // Stop on first matching output for this atom — we
                // don't need to enumerate every output port of the
                // same atom for the same goal layer; the goal is
                // satisfied by the first match. (Other output ports
                // may match different goals at later layers — they
                // get a fresh chance via the queue.)
                break;
            }
        }
    }

    let mut out: Vec<BackwardRequirement> = required.into_values().collect();
    out.sort();
    out
}

/// Project a `DesiredOutput` (today's `WorkflowIntent.desired_outputs`
/// shape) into a typed `DataProductContract` the search can match
/// against atom output ports. Uses `edam_data` as the semantic type
/// IRI; an empty / missing `edam_data` filters the entry out (the
/// search has nothing to match).
///
/// **Privacy class.** The goal `DataProductContract` is set to
/// `PrivacyClass::Restricted` (the most permissive *consumer* class)
/// so the engine's privacy-widening rule
/// (`producer.privacy_class > consumer.privacy_class → Incompatible`)
/// doesn't filter out atom producers whose outputs carry a stricter
/// privacy tag than the SME's stated intent. Backward search's
/// purpose is *discovery* of producer chains, not enforcement of
/// delivery policy — privacy delivery is enforced at planner time
/// once the actual delivery edge is known.
///
/// This helper keeps `backward_search`'s signature stable while the
/// upstream intake migration to typed contracts is in progress; it
/// lifts a `DesiredOutput` into a `DataProductContract`.
fn lift_desired_output(d: &DesiredOutput) -> Option<DataProductContract> {
    let iri = d.edam_data.as_ref()?;
    if iri.is_empty() {
        return None;
    }
    let mut dp = DataProductContract::skeleton(
        format!("desired_{}", d.label.replace(' ', "_")),
        crate::workflow_contracts::semantic_type::SemanticType::edam(iri.clone(), &d.label),
    );
    dp.privacy = crate::workflow_contracts::data_product::PrivacyClass::Restricted;
    Some(dp)
}

// ---------------------------------------------------------------------------
// Backward type-directed A* search over the atom catalog (Proposal C).
//
// Given a desired output `semantic_type` IRI + optional format and a set
// of available input `semantic_type` IRIs (typically FASTQ for sequencing
// intake), search backward through the atom catalog producing a chain of
// atoms whose outputs reach the goal and whose inputs reduce to the
// available set. A* uses (chain_length + frontier_size) as cost; tiebreak
// on lex order of the atom-id chain so the result is byte-reproducible.
//
// Distinct from the BFS-based `backward_search` above (which produces a
// flat `Vec<BackwardRequirement>` for meet-in-the-middle assembly): this
// is a chain-producing A* used by the planner as a fallback when neither
// archetype seeding nor meet-in-the-middle search yields a composition.
// Mirrors BETSY's backward-chaining approach (Chen & Chang,
// Bioinformatics 2017) and APE's SLTL-based synthesis (Kasalica &
// Lamprecht, ICCS 2020).
// ---------------------------------------------------------------------------

use crate::atom::AtomDefinition;
use crate::workflow_contracts::semantic_type::SemanticType;
use std::collections::{BTreeSet, BinaryHeap};

/// Inputs to [`search_backward`]. The `available_inputs` set names the
/// semantic-type IRIs the search may treat as "already on hand"
/// (typically FASTQ — `data:2044` — for sequencing intake; future
/// callers will derive it from `IntakeFacts.input_kinds`). The
/// `goal_format` is carried for downstream context only; the search
/// matches on `goal_data` (semantic-type) IRI.
pub struct BackwardSearchInput<'a> {
    /// Target semantic-type IRI we want some atom in the chain to
    /// produce (e.g. `data:1255` for peaks, `data:0951` for DE table).
    pub goal_data: String,
    /// Target format IRI; carried for caller bookkeeping.
    pub goal_format: Option<String>,
    /// Semantic-type IRIs the search treats as already-satisfied
    /// (search terminates when the frontier reduces to this set).
    pub available_inputs: Vec<String>,
    /// Atom catalog the search walks. Iteration order is the registry's
    /// natural (BTreeMap-keyed by atom id) so the same registry
    /// produces the same result on every call.
    pub atom_registry: &'a crate::atom_registry::AtomRegistry,
    /// Bound on chain length. Search prunes any node whose
    /// `chain.len() > max_depth`.
    pub max_depth: usize,
}

/// One atom in a chain produced by [`search_backward`]. Carries a
/// reference to the resolved `AtomDefinition` so the caller can read
/// inputs/outputs without re-looking-up the registry. The chain
/// returned by `search_backward` is ordered intake-first (the producer
/// of the goal is last).
pub struct ResolvedAtom<'a> {
    /// Atom.
    pub atom: &'a AtomDefinition,
}

/// One A* frontier node. The priority cost = chain length + frontier
/// size; ties break on lex order of the chain Vec so the result is
/// byte-reproducible across runs.
#[derive(Clone, Eq, PartialEq)]
struct SearchNode {
    /// Semantic-type IRIs still unsatisfied (chain inputs not yet
    /// produced by an atom in the chain and not in `available_inputs`).
    frontier: BTreeSet<String>,
    /// Atom ids appended in reverse order (goal-first). The chain
    /// returned to the caller is `chain.iter().rev()` so the producer
    /// of the goal lands last.
    chain: Vec<String>,
    /// Priority cost: `chain.len() + frontier.len()`. Smaller is better.
    cost: usize,
}

impl Ord for SearchNode {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Min-heap by `cost`; tiebreak on shorter chain first; final
        // tiebreak on lex order of the chain Vec so the result is
        // byte-reproducible across runs.
        other
            .cost
            .cmp(&self.cost)
            .then_with(|| other.chain.len().cmp(&self.chain.len()))
            .then_with(|| self.chain.cmp(&other.chain))
    }
}

impl PartialOrd for SearchNode {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}

/// Backward type-directed A* over the atom catalog. Returns `None`
/// when no chain ≤ `max_depth` reduces the goal's input frontier to
/// the `available_inputs` set. See module-level documentation above
/// for the algorithm and determinism guarantees.
pub fn search_backward<'a>(input: BackwardSearchInput<'a>) -> Option<Vec<ResolvedAtom<'a>>> {
    let available: BTreeSet<String> = input.available_inputs.iter().cloned().collect();
    let mut heap: BinaryHeap<SearchNode> = BinaryHeap::new();
    heap.push(SearchNode {
        frontier: [input.goal_data.clone()].into_iter().collect(),
        chain: Vec::new(),
        cost: 1,
    });

    while let Some(node) = heap.pop() {
        if node.chain.len() > input.max_depth {
            continue;
        }
        // Subtract any types already available. A type is "available"
        // when it matches an entry in `available` exactly OR an
        // available type is its EDAM subtype (the available type is
        // more specific and therefore satisfies the broader goal).
        let remaining: BTreeSet<&String> = node
            .frontier
            .iter()
            .filter(|t| {
                !available
                    .iter()
                    .any(|a| a == *t || crate::edam::is_subtype_of(a, t))
            })
            .collect();
        if remaining.is_empty() {
            // Chain complete — resolve atom ids back to references and
            // reverse so the producer of the goal lands last.
            let mut out = Vec::with_capacity(node.chain.len());
            for id in node.chain.iter().rev() {
                if let Some(a) = input.atom_registry.get(id) {
                    out.push(ResolvedAtom { atom: a });
                }
            }
            return Some(out);
        }
        // Expand the first unsatisfied type. (BTreeSet iteration is
        // sorted by IRI string so the expansion order is deterministic.)
        let target = match remaining.iter().next() {
            Some(t) => (*t).clone(),
            None => continue,
        };
        // When the goal target is the original `input.goal_data` AND a
        // `goal_format` was supplied, narrow producers to those whose
        // output port also matches the format. This disambiguates
        // among many atoms producing the same semantic_type — without
        // it, the search picks whatever atom sorts first
        // alphabetically (e.g., `spatially_variable_genes` for a
        // peaks-shaped goal that should resolve to `peak_calling`).
        let need_format_match = target == input.goal_data && input.goal_format.is_some();
        let goal_fmt = input.goal_format.as_deref();
        let producers: Vec<&AtomDefinition> = input
            .atom_registry
            .iter()
            .map(|(_, a)| a)
            .filter(|a| {
                if need_format_match {
                    atom_produces_with_format(a, &target, goal_fmt)
                } else {
                    atom_produces(a, &target)
                }
            })
            .collect();
        for p in producers {
            // Cycle guard — never revisit an atom already in the chain.
            if node.chain.contains(&p.id) {
                continue;
            }
            let mut next_frontier: BTreeSet<String> = node
                .frontier
                .iter()
                .filter(|t| *t != &target)
                .cloned()
                .collect();
            // The producer's inputs become new sub-goals.
            for input_port in &p.inputs {
                if let Some(iri) = semantic_iri(&input_port.semantic_type) {
                    next_frontier.insert(iri);
                }
            }
            let mut next_chain = node.chain.clone();
            next_chain.push(p.id.clone());
            heap.push(SearchNode {
                cost: next_chain.len() + next_frontier.len(),
                frontier: next_frontier,
                chain: next_chain,
            });
        }
    }
    None
}

/// True when one of `a`'s output ports produces the target IRI
/// (exact-match or EDAM-subtype). Handles `OntologyTerm` and
/// `LocalExtension`'s `proposed_parent_terms`; `Opaque` and `Union`
/// outputs never satisfy a typed goal (the planner uses a different
/// route for opaque-typed edges).
fn atom_produces(a: &AtomDefinition, target: &str) -> bool {
    a.outputs.iter().any(|o| match &o.semantic_type {
        SemanticType::OntologyTerm { iri, .. } => {
            iri == target || crate::edam::is_subtype_of(iri, target)
        }
        SemanticType::LocalExtension {
            proposed_parent_terms,
            ..
        } => proposed_parent_terms
            .iter()
            .any(|p| p == target || crate::edam::is_subtype_of(p, target)),
        _ => false,
    })
}

/// Variant of [`atom_produces`] that additionally requires the
/// producer's port to carry the goal's `physical_format`. Used at the
/// goal layer of [`search_backward`] to disambiguate among atoms
/// producing the same semantic_type but different formats (e.g.,
/// `peak_calling` outputs BED while `vdj_reconstruction` outputs JSON;
/// both produce `data:1255` Feature record).
fn atom_produces_with_format(
    a: &AtomDefinition,
    target_data: &str,
    target_format: Option<&str>,
) -> bool {
    a.outputs.iter().any(|o| {
        let data_ok = match &o.semantic_type {
            SemanticType::OntologyTerm { iri, .. } => {
                iri == target_data || crate::edam::is_subtype_of(iri, target_data)
            }
            SemanticType::LocalExtension {
                proposed_parent_terms,
                ..
            } => proposed_parent_terms
                .iter()
                .any(|p| p == target_data || crate::edam::is_subtype_of(p, target_data)),
            _ => false,
        };
        if !data_ok {
            return false;
        }
        match (
            target_format,
            o.physical_format.as_ref().map(|f| f.iri.as_str()),
        ) {
            (None, _) => true,
            (Some(want), Some(got)) => want == got,
            (Some(_), None) => false,
        }
    })
}

/// Extract a representative semantic-type IRI from a `SemanticType`
/// for the frontier set. `OntologyTerm` carries its IRI directly;
/// `LocalExtension` uses its first proposed parent term so the search
/// still treats it as an EDAM-shaped sub-goal; `Opaque` and `Union`
/// return `None` (the search has no IRI handle to chain through).
fn semantic_iri(s: &SemanticType) -> Option<String> {
    match s {
        SemanticType::OntologyTerm { iri, .. } => Some(iri.clone()),
        SemanticType::LocalExtension {
            proposed_parent_terms,
            ..
        } => proposed_parent_terms.first().cloned(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_intent_yields_empty_requirements() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent::default();
        let reqs = backward_search(&intent, &atom_reg, 4, 64);
        assert!(reqs.is_empty());
    }

    #[test]
    fn empty_registry_yields_empty_requirements() {
        let atom_reg = AtomRegistry::default();
        let intent = WorkflowIntent {
            desired_outputs: vec![DesiredOutput {
                label: "DE table".into(),
                edam_data: Some("data:0951".into()),
                edam_format: Some("format:3475".into()),
                human_readable: false,
            }],
            ..Default::default()
        };
        let reqs = backward_search(&intent, &atom_reg, 4, 64);
        assert!(reqs.is_empty());
    }

    #[test]
    fn lift_desired_output_skips_missing_edam_data() {
        let d = DesiredOutput {
            label: "x".into(),
            edam_data: None,
            edam_format: None,
            human_readable: false,
        };
        assert!(lift_desired_output(&d).is_none());
    }

    #[test]
    fn lift_desired_output_carries_iri() {
        let d = DesiredOutput {
            label: "DE table".into(),
            edam_data: Some("data:0951".into()),
            edam_format: None,
            human_readable: false,
        };
        let dp = lift_desired_output(&d).unwrap();
        assert_eq!(dp.semantic_type.stable_id(), "data:0951");
    }

    /// Debug: enumerate every atom output port that matches the
    /// canonical DE goal — used to root-cause why
    /// `differential_expression` was missing from the chain in early
    /// drafts of the backward-search algorithm. Eyeball the matches
    /// to confirm semantic + privacy + facet unification all line up.
    #[test]
    #[ignore = "diagnostic-only — keeps the matching matrix visible when tightening fixtures"]
    fn debug_de_goal_matches_against_registry() {
        let atom_reg =
            AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
        let goal = DataProductContract::sample_de_table();
        let goal_port = synthesize_port_from_data_product(&goal);
        eprintln!("goal_port={goal_port:#?}");

        let engine = DeterministicCompatibilityEngine::new();
        let cctx = CompatibilityContext::default();

        for (atom_id, atom) in atom_reg.iter() {
            for (i, port) in atom.outputs.iter().enumerate() {
                let result = engine.prove(port, &goal_port, &cctx);
                eprintln!(
                    "{atom_id} out[{i}]={} type={} -> {:?}",
                    port.name,
                    port.semantic_type.stable_id(),
                    match &result {
                        CompatibilityResult::Compatible(_) => "Compatible".to_string(),
                        CompatibilityResult::CompatibleWithAdapters { .. } =>
                            "CompatibleWithAdapters".to_string(),
                        CompatibilityResult::Incompatible(r) =>
                            format!("Incompat: {:?}", r.reasons),
                        CompatibilityResult::Unknown(_) => "Unknown".to_string(),
                    }
                );
            }
        }
    }
}
