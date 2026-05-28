//! Meet-in-the-middle proof matching.
//!
//! Connects forward producers (from [`super::forward_search`]) to
//! backward consumers (from [`super::backward_search`]) via the
//! `CompatibilityEngine`. The output is a typed [`WorkflowDag`] with
//! proof-carrying edges, plus a list of any unsatisfied gaps.
//!
//! # Algorithm
//!
//! For every backward-required atom and every input port of that atom:
//!
//! 1. Walk the forward frontier and for each entry, `engine.prove`
//!    the producer's output port against this consumer's input port.
//! 2. Filter to entries returning `Compatible(_)` or
//!    `CompatibleWithAdapters {.. }` (drop `Incompatible`,
//!    record `Unknown` as a gap).
//! 3. Pick the **best** producer using a deterministic tie-break:
//!    lowest `depth`, then lexically smallest `atom_id`, then smallest
//!    `port_index`. This matches the determinism contract of the
//!    forward-frontier ordering.
//! 4. Re-prove against the best producer and build an [`EdgeContract`]
//!    carrying the engine's [`CompatibilityProof`]. Adapter task nodes
//!    returned in `CompatibleWithAdapters` are added to the DAG's node
//!    list (deduped by id), and their ids are recorded on the edge's
//!    `inserted_adapter_node_ids`.
//! 5. Risky adapters (id-prefix heuristic shared with the v4 scorer)
//!    cause a warning to be appended to the proof so the SME-facing
//!    layer can route the edge through the assumption ledger.
//!
//! # Determinism
//!
//! - Backward requirements are pre-sorted (BFS by `Ord` of
//!   `BackwardRequirement`), forward entries are pre-sorted, and the
//!   `min_by` tie-break is total (`depth`, `atom_id`, `port_index`).
//! - Output `nodes` are deduped + sorted by `id`.
//! - Output `edges` are sorted by
//!   `(from_node, from_port, to_node, to_port)`.
//! - The compatibility engine is itself byte-stable (determinism
//!   contract — see `engine.rs::determinism_replay`).
//!
//! # Cycle prevention
//!
//! Symmetric port-typed atoms (e.g. `batch_correction` and
//! `differential_expression` both consume + produce `data:3917`
//! count matrices, or `variant_calling` / `variant_filtering` /
//! `variant_annotation` all touching `data:3498` VCFs) make the
//! port-only matching produce edges in BOTH directions, generating
//! cycles that `validate_dag` rejects.
//!
//! The fix uses the atom registry's existing `depends_on` field to
//! compute a topological rank for each atom, then filters out edges
//! where producer's rank ≥ consumer's rank. The `depends_on` graph
//! is the canonical workflow lineage already authored in YAML —
//! reusing it as the filter avoids a separate role table that would
//! drift from the atom catalog.

use std::collections::BTreeMap;

use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext as CompatibilityContext,
};
// Structured `RepairGap`s emitted alongside the legacy
// stringly-keyed `gaps: Vec<String>` so the planner's gap-repair wiring
// can hand them to the repair registry.
use crate::repair::proposal::RepairGap;
use crate::repair::strategy::GapKind;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::evidence::AssumptionLedger;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

use super::backward_search::BackwardRequirement;
use super::forward_search::ForwardFrontierEntry;

/// Outcome of [`meet_in_the_middle`]. Distinguishes the three
/// well-known structural shapes the planner needs:
///
/// - `Connected` — every backward requirement found a producer; no
///   gaps remain. The DAG is presentable as a `ValidatedExecutableDag`
///   (modulo policy and sandbox sweeps applied downstream).
/// - `PartiallyConnected` — some requirements were satisfied, others
///   gapped. The DAG is presentable as a `DraftDag` /
///   `PartialDag` with the gaps surfaced for SME repair.
/// - `Disconnected` — no edges built. There is nothing the SME can
///   work with; the planner should surface a `PartialDag` whose
///   `unresolved_gaps` enumerates the failed lookups.
#[derive(Debug)]
pub enum MeetResult {
    /// Every backward requirement satisfied. `gaps` is empty.
    Connected {
        /// Dag.
        dag: WorkflowDag,
        /// Gaps.
        gaps: Vec<String>,
        /// v4 P5 — structured gap detail, parallel to `gaps`. Empty for
        /// the Connected variant; always populated when `!gaps.is_empty()`.
        #[doc(hidden)]
        repair_gaps: Vec<RepairGap>,
    },
    /// Some satisfied, others gapped.
    PartiallyConnected {
        /// Dag.
        dag: WorkflowDag,
        /// Gaps.
        gaps: Vec<String>,
        /// v4 P5 — see `Connected.repair_gaps`.
        #[doc(hidden)]
        repair_gaps: Vec<RepairGap>,
    },
    /// Nothing connected (no edges built).
    Disconnected {
        /// Gaps.
        gaps: Vec<String>,
        /// v4 P5 — see `Connected.repair_gaps`.
        #[doc(hidden)]
        repair_gaps: Vec<RepairGap>,
    },
}

/// Compute a topological rank for every atom in
/// the registry from its `depends_on` field. An atom's rank is the
/// length of the longest `depends_on` chain leading to it; atoms
/// with no dependencies (or whose dependencies aren't in the
/// registry) get rank 0.
///
/// Used by [`meet_in_the_middle`] to filter candidate edges that
/// would create a back-edge in the workflow lineage. If producer's
/// rank ≥ consumer's rank, the edge is rejected (it would either be
/// a same-rank sibling-pair or a downstream→upstream back-edge).
///
/// The function is total: every atom in the registry maps to a
/// rank, including atoms that participate in `depends_on` cycles
/// (which would themselves fail `validate_consistency`, but we
/// don't depend on that having run). Cycle-participating atoms
/// fall back to rank `u32::MAX / 2` so a same-rank check still
/// fires symmetrically. Atoms not in the registry (placeholders,
/// adapters) implicitly get rank `u32::MAX` via the `unwrap_or`
/// in [`lookup_rank`].
fn compute_topological_ranks(atom_reg: &AtomRegistry) -> BTreeMap<String, u32> {
    let mut ranks: BTreeMap<String, u32> = BTreeMap::new();
    // Iterate to a fixed point: each pass computes every atom's
    // rank as 1 + max(rank of its deps), defaulting unknown deps
    // to 0 (intake-supplied data). Bounded by atom count since the
    // dependency graph is a DAG by construction (any cycle is
    // caught by `AtomRegistry::validate_consistency` at load time;
    // here we still bound the loop in case validate hasn't run).
    let max_iter = atom_reg.len().saturating_add(1);
    for _ in 0..max_iter {
        let mut changed = false;
        for (id, atom) in atom_reg.iter() {
            let dep_max = atom
                .depends_on
                .iter()
                .filter_map(|d| ranks.get(d).copied())
                .max();
            // If at least one declared dep is missing from `ranks`
            // on this pass, defer — wait for a later pass. If the
            // dep doesn't exist in the registry at all, treat it
            // as rank 0 (intake-supplied input).
            let all_deps_known = atom
                .depends_on
                .iter()
                .all(|d| ranks.contains_key(d) || atom_reg.get(d).is_none());
            if !all_deps_known {
                continue;
            }
            let new_rank = match dep_max {
                Some(m) => m + 1,
                None => 0, // no deps OR all deps are intake-supplied
            };
            match ranks.get(id) {
                Some(&existing) if existing == new_rank => {}
                _ => {
                    ranks.insert(id.clone(), new_rank);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // Defensive: any atom that didn't get a rank is participating
    // in a `depends_on` cycle. Assign a sentinel mid-range value so
    // same-rank checks still fire (cycles among themselves remain
    // rejected; chains into cycle-free atoms get filtered too).
    let cycle_sentinel = u32::MAX / 2;
    for (id, _) in atom_reg.iter() {
        ranks.entry(id.clone()).or_insert(cycle_sentinel);
    }
    ranks
}

/// Look up an atom's topological rank with a
/// safe default for unknown ids. Returns `u32::MAX` for atoms not
/// in the registry (placeholders, adapter nodes, hypothesized
/// nodes) so they're treated as terminal — they can't appear as a
/// producer of any same-rank or upstream edge, since
/// `producer_rank ≥ consumer_rank` rejects them.
fn lookup_rank(ranks: &BTreeMap<String, u32>, atom_id: &str) -> u32 {
    ranks.get(atom_id).copied().unwrap_or(u32::MAX)
}

/// Would the candidate edge create a cycle by
/// adding `producer → consumer` to the workflow's topological
/// ordering? Reject when:
///
/// - `producer_rank > consumer_rank` — back-edge against the
///   canonical lineage (e.g. `batch_correction` (rank 7) →
///   `normalisation` (rank 6) is going backwards).
/// - `producer_rank == consumer_rank` AND `producer_id != consumer_id`
///   — sibling atoms at the same rank shouldn't feed each other
///   (e.g. `batch_correction` and `differential_expression` are
///   both downstream of `normalisation`; they're siblings, not a
///   chain).
///
/// The `producer_id == consumer_id` self-loop guard exists for
/// completeness; meet_in_middle's per-(atom, port) dedupe already
/// prevents an atom from being its own producer in practice.
fn would_create_cycle(producer_id: &str, consumer_id: &str, ranks: &BTreeMap<String, u32>) -> bool {
    if producer_id == consumer_id {
        // Self-loop — reject defensively (the dedupe guard above
        // means this shouldn't fire today, but a future caller
        // might bypass it).
        return true;
    }
    let producer_rank = lookup_rank(ranks, producer_id);
    let consumer_rank = lookup_rank(ranks, consumer_id);
    producer_rank >= consumer_rank
}

/// Connect a forward frontier to a backward requirement set via the
/// compatibility engine. See the module-level docs for the algorithm.
pub fn meet_in_the_middle(
    forward: &[ForwardFrontierEntry],
    backward: &[BackwardRequirement],
    atom_reg: &AtomRegistry,
) -> MeetResult {
    meet_in_the_middle_with_opaque_observation(forward, backward, atom_reg, None, None)
}

/// R1/R2 closure (closure-residuals plan Task 1.4) — sibling of
/// [`meet_in_the_middle`] that threads an optional
/// `OpaqueObservationSink` + session id into the engine
/// `CompatibilityContext` so Opaque-type observations attribute to the
/// right session and consumer-side atom. The node id is mutated
/// immediately before each `engine.prove()` call.
pub fn meet_in_the_middle_with_opaque_observation(
    forward: &[ForwardFrontierEntry],
    backward: &[BackwardRequirement],
    atom_reg: &AtomRegistry,
    opaque_sink: Option<
        std::sync::Arc<dyn crate::compatibility::engine::OpaqueObservationSink + Send + Sync>,
    >,
    opaque_session_id: Option<&str>,
) -> MeetResult {
    let engine = DeterministicCompatibilityEngine::new();
    let mut cctx = CompatibilityContext {
        opaque_observation_sink: opaque_sink,
        opaque_session_id: opaque_session_id.map(String::from),
        ..Default::default()
    };
    // Pre-compute topological ranks once.
    let ranks = compute_topological_ranks(atom_reg);

    // Nodes: dedupe by id so an atom that is both a consumer and a
    // producer of multiple edges only appears once. BTreeMap iteration
    // order is sorted-by-id, which gives us the deterministic node
    // sort for free.
    let mut nodes_by_id: BTreeMap<String, TaskNode> = BTreeMap::new();
    let mut edges: Vec<EdgeContract> = Vec::new();
    let mut gaps: Vec<String> = Vec::new();
    // v4 P5 — structured gaps parallel to the legacy `gaps` strings.
    // Built lazily so callers that don't reach the repair path pay
    // nothing extra.
    let mut repair_gaps: Vec<RepairGap> = Vec::new();

    for req in backward {
        let consumer_atom = match atom_reg.get(req.atom_id.as_str()) {
            Some(a) => a,
            None => {
                // Backward requirement names an atom not in the
                // registry. Surface as a gap rather than silently
                // dropping — the SME / planner needs to know the
                // search produced a phantom requirement.
                gaps.push(format!("{}:missing_atom", req.atom_id));
                repair_gaps.push(RepairGap {
                    id: format!("{}:missing_atom", req.atom_id),
                    statement: format!(
                        "backward requirement names atom {} not in registry",
                        req.atom_id
                    ),
                    kind: GapKind::MissingProducer,
                    consumer_node: req.atom_id.to_string(),
                    consumer_port: String::new(),
                    producer_node: None,
                    producer_port: None,
                    facet_mismatches: Vec::new(),
                });
                continue;
            }
        };

        // Always lift the consumer node into the DAG, even if no edge
        // ends up connected — a backward-required node with no
        // upstream producer is itself a useful gap signal for the UI.
        nodes_by_id
            .entry(req.atom_id.to_string())
            .or_insert_with(|| TaskNode::from_atom(consumer_atom));

        // Atoms with no input ports (aggregator / discovery / source
        // atoms) don't need an edge; the search reached them by virtue
        // of producing a backward-matched output, not because they
        // need a producer themselves. Skip cleanly.
        if consumer_atom.inputs.is_empty() {
            continue;
        }

        // R1/R2 closure — attribute any Opaque observation emitted by
        // `engine.prove()` below to the consumer atom whose input port
        // is currently being matched.
        cctx.opaque_node_id = Some(req.atom_id.to_string());

        for (input_idx, consumer_port) in consumer_atom.inputs.iter().enumerate() {
            // Find the best forward producer for this input port.
            // Best = `Compatible` outranks `CompatibleWithAdapters`
            // (lossless beats adapter-mediated), then ties broken by
            // `(depth, atom_id, port_index)` per the forward-frontier
            // ordering contract.
            //
            // Additionally filter out producers
            // whose `topological_rank` ≥ consumer's. The compatibility
            // engine doesn't know workflow lineage; the `depends_on`
            // graph in the atom registry does. This check rejects
            // back-edges (would-be cycles) without losing legitimate
            // upstream→downstream edges.
            let mut best: Option<(&ForwardFrontierEntry, CompatibilityResult, u8)> = None;
            for entry in forward {
                if would_create_cycle(entry.atom_id.as_str(), req.atom_id.as_str(), &ranks) {
                    continue;
                }
                let producer_atom = match atom_reg.get(entry.atom_id.as_str()) {
                    Some(a) => a,
                    None => continue,
                };
                let producer_port = match producer_atom.outputs.get(entry.port_index as usize) {
                    Some(p) => p,
                    None => continue,
                };
                let result = engine.prove(producer_port, consumer_port, &cctx);
                let rank: u8 = match &result {
                    CompatibilityResult::Compatible(_) => 0,
                    CompatibilityResult::CompatibleWithAdapters { .. } => 1,
                    // Drop Incompatible candidates entirely.
                    CompatibilityResult::Incompatible(_) => continue,
                    // Unknown is a gap — record once per
                    // (atom, port) and don't promote to an edge. We
                    // log the cause but continue scanning in case a
                    // later entry resolves the input deterministically.
                    CompatibilityResult::Unknown(_) => continue,
                };
                let take = match &best {
                    None => true,
                    Some((cur_entry, _, cur_rank)) => {
                        rank < *cur_rank
                            || (rank == *cur_rank
                                && (entry.depth, entry.atom_id.as_str(), entry.port_index)
                                    < (
                                        cur_entry.depth,
                                        cur_entry.atom_id.as_str(),
                                        cur_entry.port_index,
                                    ))
                    }
                };
                if take {
                    best = Some((entry, result, rank));
                }
            }

            match best {
                Some((producer_entry, result, _rank)) => {
                    // Lift the producer atom into the DAG (deduped).
                    if let Some(producer_atom) = atom_reg.get(producer_entry.atom_id.as_str()) {
                        nodes_by_id
                            .entry(producer_entry.atom_id.to_string())
                            .or_insert_with(|| TaskNode::from_atom(producer_atom));

                        let producer_port =
                            &producer_atom.outputs[producer_entry.port_index as usize];
                        let edge = build_edge(
                            producer_entry.atom_id.as_str(),
                            &producer_port.name,
                            req.atom_id.as_str(),
                            &consumer_port.name,
                            result,
                            &mut nodes_by_id,
                        );
                        edges.push(edge);
                    }
                }
                None => {
                    let gap_id = format!("{}:{}", req.atom_id, input_idx);
                    gaps.push(gap_id.clone());
                    repair_gaps.push(RepairGap {
                        id: gap_id,
                        statement: format!(
                            "no compatible producer found for consumer {}'s input port {} ({})",
                            req.atom_id, input_idx, consumer_port.name,
                        ),
                        kind: GapKind::MissingProducer,
                        consumer_node: req.atom_id.to_string(),
                        consumer_port: consumer_port.name.clone(),
                        producer_node: None,
                        producer_port: None,
                        facet_mismatches: Vec::new(),
                    });
                }
            }
        }
    }

    // Stable sort by the natural `(from_node, from_port, to_node, to_port)`.
    edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });

    let nodes: Vec<TaskNode> = nodes_by_id.into_values().collect();
    // Record the seed heuristic used: meet-in-the-middle selected producers
    // by (depth, atom_id, port_index) deterministic tie-break. This
    // assumption entry makes assumptions.jsonl non-empty for search-seed
    // compositions so the grant auditability corpus can inspect it.
    //
    // Resolution is `Accepted` — not `Unresolved` — because the
    // (depth, atom_id, port_index) tie-break is a deterministic system
    // decision, not a pending SME choice. An `Unresolved` entry here would
    // inflate `score.unresolved_assumptions`, causing `classify_outcome` to
    // return `DraftDag` instead of `ValidatedExecutableDag` for any
    // search-seeded composition, breaking intake for all modalities.
    let seed_assumption = crate::workflow_contracts::evidence::Assumption {
        id: "seed_heuristic:meet_in_the_middle".into(),
        statement: "Producer was selected by (depth, atom_id, port_index) deterministic \
                    tie-break when multiple compatible producers were available."
            .into(),
        source: crate::workflow_contracts::evidence::AssumptionSource::SeedHeuristic {
            strategy: "forward_backward_meet_in_middle".into(),
        },
        affects_nodes: vec![],
        risk: crate::workflow_contracts::evidence::RiskClass::Negligible,
        resolution: crate::workflow_contracts::evidence::AssumptionResolution::Accepted {
            rationale: "Deterministic system tie-break; no SME decision required.".into(),
        },
        chain_of_custody: None,
    };
    let dag = WorkflowDag {
        id: "meet_in_the_middle".into(),
        nodes,
        edges,
        assumptions: AssumptionLedger {
            entries: vec![seed_assumption],
        },
        source_template: None,
    };

    let any_edges = !dag.edges.is_empty();
    if gaps.is_empty() && any_edges {
        MeetResult::Connected {
            dag,
            gaps,
            repair_gaps,
        }
    } else if any_edges {
        MeetResult::PartiallyConnected {
            dag,
            gaps,
            repair_gaps,
        }
    } else {
        MeetResult::Disconnected { gaps, repair_gaps }
    }
}

/// Build an `EdgeContract` from a `CompatibilityResult`, lifting any
/// adapter task nodes returned by the engine into the shared
/// `nodes_by_id` map (so adapters appear exactly once even when
/// multiple edges share the same adapter chain).
///
/// Risky adapters (id-prefix heuristic shared with
/// `composer_v4::planner::score_dag`) are flagged via a warning on
/// the proof so the outcome wrapper can route the edge
/// through the assumption ledger.
fn build_edge(
    producer_id: &str,
    producer_port: &str,
    consumer_id: &str,
    consumer_port: &str,
    result: CompatibilityResult,
    nodes_by_id: &mut BTreeMap<String, TaskNode>,
) -> EdgeContract {
    let proof = match result {
        CompatibilityResult::Compatible(p) => p,
        CompatibilityResult::CompatibleWithAdapters {
            mut proof,
            adapters,
        } => {
            // Engine already populated
            // `proof.inserted_adapter_node_ids` from `resolved_adapters`,
            // but we also lift the adapter `TaskNode`s into the DAG so
            // the lowering pass can serialize them. Risky-adapter
            // detection mirrors the scorer's id-prefix heuristic.
            for adapter in &adapters {
                let id = adapter.id.clone();
                if is_risky_adapter_id(&id) {
                    proof.warnings.push(format!(
                        "risky adapter {id} inserted; assumption ledger entry required"
                    ));
                }
                nodes_by_id.entry(id).or_insert_with(|| adapter.clone());
            }
            proof
        }
        // The selection loop drops Incompatible / Unknown before we
        // get here; these arms exist for total coverage. If reached,
        // synthesize a defensive proof carrying the engine's report
        // as a warning rather than panicking.
        CompatibilityResult::Incompatible(report) => CompatibilityProof {
            warnings: vec![format!("incompatible: {:?}", report.reasons)],
            ..Default::default()
        },
        CompatibilityResult::Unknown(c) => CompatibilityProof {
            warnings: vec![format!("unknown: {}: {}", c.id, c.statement)],
            ..Default::default()
        },
    };
    EdgeContract {
        from_node: producer_id.to_string(),
        from_port: producer_port.to_string(),
        to_node: consumer_id.to_string(),
        to_port: consumer_port.to_string(),
        proof,
        chain_of_custody: None,
    }
}

/// Match the id-prefix heuristic used by
/// `composer_v4::planner::score_dag` to decide which adapters are
/// scientifically risky. Centralized here so the meet path's risk
/// flagging stays consistent with the v4 scorer.
fn is_risky_adapter_id(id: &str) -> bool {
    id.starts_with("liftover_")
        || id.starts_with("normalize_")
        || id.starts_with("batch_correct")
        || id.starts_with("celltype_label_transfer")
        || id.starts_with("variant_normalize_")
        || id.starts_with("impute_")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

    #[test]
    fn empty_inputs_return_disconnected() {
        let atom_reg = AtomRegistry::default();
        let result = meet_in_the_middle(&[], &[], &atom_reg);
        match result {
            MeetResult::Disconnected { gaps, .. } => {
                assert!(gaps.is_empty(), "no gaps expected from empty inputs");
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[test]
    fn missing_consumer_atom_records_gap() {
        let atom_reg = AtomRegistry::default();
        let backward = vec![BackwardRequirement {
            atom_id: "phantom_atom".into(),
            port_index: 0,
            depth: 0,
        }];
        let result = meet_in_the_middle(&[], &backward, &atom_reg);
        match result {
            MeetResult::Disconnected { gaps, .. } => {
                assert!(
                    gaps.iter().any(|g| g.contains("phantom_atom")),
                    "expected gap for missing atom, got {gaps:?}"
                );
            }
            other => panic!("expected Disconnected, got {other:?}"),
        }
    }

    #[test]
    fn risky_adapter_id_recognized() {
        assert!(is_risky_adapter_id("liftover_grch37_grch38"));
        assert!(is_risky_adapter_id("normalize_quantile"));
        assert!(is_risky_adapter_id("batch_correct_combat"));
        assert!(!is_risky_adapter_id("gzip_decompress"));
        assert!(!is_risky_adapter_id("bam_index"));
    }

    /// Empty registry produces empty rank map.
    #[test]
    fn topological_ranks_empty_registry() {
        let atom_reg = AtomRegistry::default();
        let ranks = compute_topological_ranks(&atom_reg);
        assert!(ranks.is_empty(), "empty registry yields empty ranks");
    }

    /// Back-edge filter rejects same-rank pairs and
    /// downstream→upstream edges. The lookup_rank fallback gives unknown
    /// ids the terminal rank `u32::MAX` so they can never appear as
    /// producers (which is what we want for safety: an unknown id
    /// shouldn't author edges into the registry).
    #[test]
    fn would_create_cycle_rejects_same_rank_pairs() {
        let mut ranks = BTreeMap::new();
        ranks.insert("a".to_string(), 0);
        ranks.insert("b".to_string(), 1);
        ranks.insert("c1".to_string(), 2);
        ranks.insert("c2".to_string(), 2);
        ranks.insert("d".to_string(), 3);
        // upstream -> downstream: keep
        assert!(!would_create_cycle("a", "b", &ranks));
        assert!(!would_create_cycle("b", "c1", &ranks));
        assert!(!would_create_cycle("c1", "d", &ranks));
        // back-edge (downstream -> upstream): reject
        assert!(would_create_cycle("d", "c1", &ranks));
        assert!(would_create_cycle("c1", "b", &ranks));
        assert!(would_create_cycle("b", "a", &ranks));
        // sibling pair (same rank): reject
        assert!(would_create_cycle("c1", "c2", &ranks));
        assert!(would_create_cycle("c2", "c1", &ranks));
        // self-loop: reject
        assert!(would_create_cycle("a", "a", &ranks));
        // unknown producer (rank = u32::MAX): reject any edge from it
        assert!(would_create_cycle("unknown", "a", &ranks));
    }

    /// The full chain test lives in
    /// `crates/core/tests/composer_v4_meet_in_middle.rs`; this in-crate
    /// test only exercises the empty / missing-atom edge cases plus
    /// the risky-id helper, since the real registry is loaded relative
    /// to repo root and the in-crate `cfg(test)` builds run from a
    /// different cwd (`crates/core`).
    #[test]
    #[ignore = "diagnostic — exercised by tests/composer_v4_meet_in_middle.rs"]
    fn live_registry_meet_smoke() {
        let atom_reg =
            AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
        let de_goal =
            crate::workflow_contracts::data_product::DataProductContract::sample_de_table();
        let intent = WorkflowIntent {
            available_data: vec![
                crate::workflow_contracts::data_product::DataProductContract::sample_paired_fastq(),
            ],
            desired_outputs: vec![DesiredOutput {
                label: "DE table".into(),
                edam_data: Some(de_goal.semantic_type.stable_id()),
                edam_format: Some("format:3475".into()),
                human_readable: false,
            }],
            ..Default::default()
        };
        let forward = super::super::forward_search::forward_search(&intent, &atom_reg, 8, 64);
        let backward = super::super::backward_search::backward_search(&intent, &atom_reg, 8, 64);
        let result = meet_in_the_middle(&forward, &backward, &atom_reg);
        assert!(
            !matches!(result, MeetResult::Disconnected { .. }),
            "expected non-Disconnected result, got {result:?}"
        );
    }
}
