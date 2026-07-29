//! Shared, pure rewire-or-drop helper consumed by both the
//! conversation crate's `prune_excluded_atoms` post-pass and the
//! upcoming `prune_unsourced_atoms` pass (atoms whose required inputs
//! have no surviving upstream source).
//!
//! The helper is **pure**: it only receives `(&mut WorkflowDag,
//! &BTreeSet<String>)` and carries no session state. Callers build the
//! `dropped` set with their own policy (exclusion list, unsourced
//! detection, etc.) and hand it to `rewire_or_drop` which applies a
//! fixpoint rewire-or-drop sweep:
//!
//!  1. Surviving nodes whose *every* incoming edge came from a dropped
//!     node are **rewired** to `data_acquisition` (the synthetic upstream
//!     anchor when the SME's data is post-pipeline).
//!  2. When `data_acquisition` was itself dropped (rare), those nodes are
//!     **cascade-dropped** instead.  Newly cascade-dropped nodes are added
//!     to the working drop set immediately so that their own successors
//!     can be cascade-dropped in the next fixpoint round, producing full
//!     transitive (multi-hop) cascade in a single call.
//!  3. All edges referencing dropped nodes are removed.
//!  4. All dropped nodes are removed.
//!  5. New `OrderingOnly` rewire edges are added (idempotent).

use std::collections::{BTreeMap, BTreeSet};

use crate::compatibility::engine::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::port::{Cardinality, PortContract};
use crate::workflow_contracts::task_node::WorkflowDag;

/// The synthetic upstream fallback node id.  Any surviving node whose
/// every incoming edge came from a dropped node gets rewired here,
/// keeping it reachable without a cascade-drop.
const DATA_ACQ_ID: &str = "data_acquisition";

/// Apply a rewire-or-drop sweep to `dag` given a pre-computed set of
/// node ids to drop.  The sweep iterates to a fixpoint to reproduce the
/// transitive multi-hop cascade of the original `prune_excluded_atoms`
/// implementation.
///
/// # Semantics
///
/// * **Rewire** — a surviving node that lost all its incoming edges
///   because every upstream was in `dropped` (or was itself cascade-
///   dropped) is reconnected to `data_acquisition` via an `OrderingOnly`
///   edge rather than cascade-dropped.  The rationale on the proof reads:
///   `"rewired to data_acquisition because upstream atom(s) were excluded by SME"`.
/// * **Cascade-drop** — when `data_acquisition` was itself dropped (or
///   does not exist), the orphaned surviving node is itself dropped.
///   Newly cascade-dropped nodes are added to the effective drop set
///   immediately, enabling their own downstream successors to be
///   cascade-dropped in the next fixpoint round.  This produces full
///   transitive (multi-hop) cascade in a single call.
/// * **Idempotency** — rewire edges are only added when no edge with
///   the same `(from_node, to_node)` pair already exists.
///
/// # Determinism
///
/// `dropped` is a `BTreeSet` so iteration order is stable and the
/// resulting node + edge vecs are produced in the same order on every
/// call with identical inputs.
///
/// # Modifies
///
/// `dag.nodes`, `dag.edges`.  `dag.assumptions` and
/// `dag.source_template` are left untouched.
pub fn rewire_or_drop(dag: &mut WorkflowDag, dropped: &BTreeSet<String>) {
    // Nothing to do — fast-path avoids even scanning nodes/edges.
    if dropped.is_empty() {
        return;
    }

    // `data_acquisition` must both exist as a node AND not itself be in
    // the drop set for rewiring to be possible.
    let data_acq_present =
        dag.nodes.iter().any(|n| n.id == DATA_ACQ_ID) && !dropped.contains(DATA_ACQ_ID);

    // Working drop set: starts as a clone of the caller's set, then grows
    // transitively via fixpoint iteration.  The original `prune_excluded_atoms`
    // mutated `dropped` in-place during the loop; we reproduce that
    // transitive, multi-hop cascade by iterating to a fixpoint — each round
    // may discover new orphans whose every upstream is now in `effective`,
    // and those get cascade-dropped into `effective`, enabling the next round
    // to discover their own orphaned successors.
    let mut effective: BTreeSet<String> = dropped.clone();

    // Rewire candidates are collected AFTER the fixpoint so that we only
    // rewire nodes that truly survive (i.e. are not themselves cascade-dropped
    // by a later fixpoint round).
    let mut rewires: Vec<(String, String)> = Vec::new();

    // Fixpoint: keep scanning until no new cascade-drops are added.
    loop {
        let prev_len = effective.len();
        for node in dag.nodes.iter() {
            if effective.contains(&node.id) {
                continue;
            }
            let incoming: Vec<&str> = dag
                .edges
                .iter()
                .filter(|e| e.to_node == node.id)
                .map(|e| e.from_node.as_str())
                .collect();
            if incoming.is_empty() {
                // No incoming edges at all — node is a source; not affected.
                continue;
            }
            let all_dropped = incoming.iter().all(|src| effective.contains(*src));
            if !all_dropped {
                continue;
            }
            // All incoming edges are from dropped nodes.  Cascade-drop when
            // `data_acquisition` is unavailable; otherwise mark as rewire
            // candidate (resolved after the fixpoint).
            if data_acq_present && node.id != DATA_ACQ_ID {
                // Rewire candidate — do NOT add to `effective`; it survives.
                // (We add to `rewires` after the fixpoint to avoid duplicates.)
            } else {
                effective.insert(node.id.clone());
            }
        }
        if effective.len() == prev_len {
            // No new cascade-drops this round — fixpoint reached.
            break;
        }
    }

    // Now collect rewire candidates: surviving nodes whose every incoming
    // edge is from a node in `effective` (the final drop set).
    for node in dag.nodes.iter() {
        if effective.contains(&node.id) {
            continue;
        }
        if node.id == DATA_ACQ_ID {
            continue;
        }
        let incoming: Vec<&str> = dag
            .edges
            .iter()
            .filter(|e| e.to_node == node.id)
            .map(|e| e.from_node.as_str())
            .collect();
        if incoming.is_empty() {
            continue;
        }
        if incoming.iter().all(|src| effective.contains(*src)) && data_acq_present {
            rewires.push((DATA_ACQ_ID.to_string(), node.id.clone()));
        }
    }

    // Build an &str view of the effective drop set for the retain calls.
    let effective_drop: BTreeSet<&str> = effective.iter().map(String::as_str).collect();

    // Drop edges referencing any dropped node, then drop the nodes.
    dag.edges.retain(|e| {
        !effective_drop.contains(e.from_node.as_str())
            && !effective_drop.contains(e.to_node.as_str())
    });
    dag.nodes
        .retain(|n| !effective_drop.contains(n.id.as_str()));

    // Add rewire edges (after dropping the now-dead originals).
    // Minimal EdgeContract with sentinel ports + a rationale so the
    // downstream lowering path can pick up the edge.
    for (from_node, to_node) in rewires {
        let already = dag
            .edges
            .iter()
            .any(|e| e.from_node == from_node && e.to_node == to_node);
        if already {
            continue;
        }
        let proof = CompatibilityProof {
            rationale: Some(
                "rewired to data_acquisition because upstream atom(s) were excluded by SME"
                    .to_string(),
            ),
            ..Default::default()
        };
        dag.edges.push(EdgeContract {
            from_node,
            from_port: "_excluded_rewire".into(),
            to_node,
            to_port: "_excluded_rewire".into(),
            proof,
            // Structural re-wire after an SME atom exclusion: an
            // ordering edge, not a port-typed data flow.
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        });
    }
}

/// A port is REQUIRED when its cardinality is not `Optional`. An
/// `Optional` input may be left unwired, so a missing producer for it
/// must not justify pruning its consumer.
///
/// `pub(crate)` so the emit-time backstop in
/// `composer::validation::no_unsourced_required_inputs` reuses the exact
/// same predicate the pruner uses (DRY — one definition of "required").
pub(crate) fn is_required(port: &PortContract) -> bool {
    !matches!(port.cardinality, Cardinality::Optional)
}

/// A required input port is *prunable-when-unsourced* only when it is a
/// **gene-set collection** input (semantic type [`GENE_SET_SEMANTIC_IRI`],
/// `data:2600`). This is the one input class the unsourced-prune feature
/// is designed to gate: `source_typing` surfaces a `data:2600` output on
/// the ingest anchor IFF an SME registered a gene-set, so a gene-set input
/// with no upstream `data:2600` producer means "no gene-set registered →
/// drop the (gene-set-dependent) atom".
///
/// Every OTHER required input is satisfied by a channel the composer
/// already accounted for but which the type-only `any_output_satisfies`
/// re-derivation cannot see, so flagging them over-prunes legitimately-
/// wired atoms. Concretely, on the live atom catalog:
///   * **intake-supplied metadata** — e.g. `differential_expression`'s
///     `experimental_design` input (`topic:3678`, the SME's contrast /
///     reference-level design) is never produced by ANY upstream atom; it
///     comes from registered intake, so the composer composes DE without a
///     producer for it. A blanket "no producer → prune" rule deletes DE
///     (and everything downstream) in every RNA-seq DAG.
///   * **ordering-/cross-branch-wired inputs** — e.g. proteomics reuses
///     the `differential_expression` atom whose `normalized_counts` input
///     is typed `data:3917` (RNA count matrix) while the proteomics branch
///     produces `ecaax:protein_abundance_matrix`; the composer wires this
///     as an `ordering_only`/residual edge that the strict IRI-unification
///     in `any_output_satisfies` cannot reproduce.
///
/// Scoping the prune to the gene-set sentinel keeps the keystone
/// `pathway_enrichment` behavior (drop when no gene-set is registered,
/// keep when one is) while leaving every other composer-wired atom intact.
///
/// `pub(crate)` so the emit-time backstop in
/// `composer::validation::no_unsourced_required_inputs` reuses the exact
/// same predicate (DRY — one definition of "prunable-when-unsourced").
pub(crate) fn is_prunable_required_input(port: &PortContract) -> bool {
    is_required(port)
        && port.semantic_type.stable_id()
            == crate::composer_v4::source_typing::GENE_SET_SEMANTIC_IRI
}

/// Does some producer output port satisfy `consumer` per the
/// compatibility engine? Reuses the exact predicate
/// `forward_search`/`meet_in_middle` use: `Compatible` or
/// `CompatibleWithAdapters` count as "sourced"; `Incompatible` and
/// `Unknown` (opaque short-circuit, undecided facets) do not.
///
/// `pub(crate)` so the emit-time backstop in
/// `composer::validation::no_unsourced_required_inputs` reuses the exact
/// same compatibility predicate the pruner uses (DRY).
pub(crate) fn any_output_satisfies(
    engine: &DeterministicCompatibilityEngine,
    ctx: &PlanningContext,
    producers: &[&PortContract],
    consumer: &PortContract,
) -> bool {
    producers.iter().any(|prod| {
        matches!(
            engine.prove(prod, consumer, ctx),
            CompatibilityResult::Compatible(_) | CompatibilityResult::CompatibleWithAdapters { .. }
        )
    })
}

/// Pure pass that drops every atom whose REQUIRED input port(s) cannot
/// be SOURCED, then rewires-or-drops the resulting orphans via
/// [`rewire_or_drop`].
///
/// "Sourced" = some UPSTREAM-REACHABLE node (following `edges` backward
/// from the consumer) has an OUTPUT port whose semantic type is
/// COMPATIBLE with the required input port's semantic type, where
/// compatibility is decided by the shared
/// [`DeterministicCompatibilityEngine`] (`Compatible` /
/// `CompatibleWithAdapters`). Because `data_acquisition` is the root
/// source ancestor of everything and (post `source_typing`) carries
/// registered intake inputs as typed output ports, a registered gene-set
/// makes a pathway atom's gene-set input sourceable; with no gene-set
/// registered, it is not.
///
/// # Semantics
///
/// * A non-source node is unsourced when its **gene-set-collection**
///   required input (see [`is_prunable_required_input`]) has no compatible
///   producer among its transitive ancestors. Other required inputs are
///   NOT prune triggers — they are satisfied by channels the composer
///   already accounted for (intake-supplied metadata, ordering-only /
///   cross-branch edges) that the type-only sourcing check cannot see, so
///   flagging them would over-prune legitimately-wired atoms. Source nodes
///   (no incoming edges) are never pruned for lack of an upstream source —
///   their inputs are satisfied externally (registered intake data).
/// * `discover_<base>` / `validate_<base>` companions of any dropped
///   `<base>` are dropped alongside it (mirroring the
///   `format!("discover_{...}")` / `format!("validate_{...}")` naming the
///   companion-synthesis passes use).
/// * FIXPOINT — dropping a producer can un-source a downstream consumer,
///   so the unsourced scan is repeated until no new node is added.
/// * The accumulated drop set is then handed to [`rewire_or_drop`], which
///   rewires surviving orphans to `data_acquisition` (or cascade-drops
///   them when the anchor itself is gone).
///
/// # Determinism
///
/// Reachability and the drop set use sorted (`BTreeMap` / `BTreeSet`)
/// containers and the nodes/edges are scanned in their stored order, so
/// the result is byte-stable across runs given identical input. No I/O,
/// no randomness.
///
/// # Not wired
///
/// This pass is standalone; it is NOT invoked by composition / rebuild
/// today. It mutates only `dag.nodes` and `dag.edges` (through
/// `rewire_or_drop`); no atom YAML is touched.
pub fn prune_unsourced_atoms(dag: &mut WorkflowDag) {
    // Precompute incoming edges per node id (consumer -> [producer ids]).
    // BTreeMap for deterministic iteration; entries appear in stored
    // order within each vec.
    let mut incoming: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for node in &dag.nodes {
        incoming.entry(node.id.as_str()).or_default();
    }
    for edge in &dag.edges {
        incoming
            .entry(edge.to_node.as_str())
            .or_default()
            .push(edge.from_node.as_str());
    }

    // Index node id -> &TaskNode for output-port lookup during sourcing.
    let by_id: BTreeMap<&str, &crate::workflow_contracts::task_node::TaskNode> =
        dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // Transitive ancestors per node (every upstream-reachable node id,
    // following edges backward). Excludes the node itself.
    let ancestors: BTreeMap<&str, BTreeSet<&str>> = dag
        .nodes
        .iter()
        .map(|n| {
            (
                n.id.as_str(),
                transitive_ancestors(n.id.as_str(), &incoming),
            )
        })
        .collect();

    let engine = DeterministicCompatibilityEngine::new();
    let ctx = PlanningContext::default();

    // Set of ids known to exist in this DAG (for companion membership).
    let present: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();

    let mut dropped: BTreeSet<String> = BTreeSet::new();

    // FIXPOINT: each round re-scans every not-yet-dropped node. A round
    // may add a node whose required input is only producible by an
    // already-dropped ancestor whose entire producing sub-chain is gone —
    // i.e. the input was NEVER sourceable from a surviving root.
    //
    // Sourcing is checked against every upstream-reachable producer that
    // is NOT dropped. A node that merely loses its sole *direct* producer
    // to a drop, yet still descends from the surviving `data_acquisition`
    // anchor, is NOT pruned here — it becomes an orphan that
    // `rewire_or_drop` reconnects to the anchor (mirroring the
    // `prune_excluded_atoms` contract, where the shared helper owns the
    // transitive rewire/cascade rather than the drop-set builder).
    loop {
        let before = dropped.len();

        for node in &dag.nodes {
            if dropped.contains(node.id.as_str()) {
                continue;
            }
            // Source nodes (no incoming edges) are satisfied externally —
            // never prune them for lack of an upstream source.
            let preds = incoming
                .get(node.id.as_str())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            if preds.is_empty() {
                continue;
            }

            // A node that still descends from the surviving
            // `data_acquisition` anchor remains reachable: any required
            // input that lost only its direct producer is rewired by
            // `rewire_or_drop`, not pruned here. Pruning is reserved for
            // inputs that are INTRINSICALLY unsourceable (no compatible
            // producer anywhere upstream — e.g. a gene-set input with no
            // registered gene-set source). To decide that, source against
            // every upstream-reachable producer that is still alive.
            let producer_ports: Vec<&PortContract> = ancestors
                .get(node.id.as_str())
                .into_iter()
                .flatten()
                .filter(|anc| !dropped.contains(**anc))
                .filter_map(|anc| by_id.get(anc))
                .flat_map(|anc| anc.outputs.iter())
                .collect();

            // Whether the node still reaches a surviving anchor: if so,
            // a required input that *was* sourceable originally but lost
            // its producer to a drop is a rewire case, not a prune case.
            let reaches_surviving_anchor = ancestors
                .get(node.id.as_str())
                .into_iter()
                .flatten()
                .any(|anc| *anc == DATA_ACQ_ID && !dropped.contains(DATA_ACQ_ID));

            let unsourced = node
                .inputs
                .iter()
                .filter(|p| is_prunable_required_input(p))
                .any(|input| {
                    if any_output_satisfies(&engine, &ctx, &producer_ports, input) {
                        return false;
                    }
                    // No surviving upstream producer for this required input.
                    // Was it ever sourceable in the ORIGINAL DAG (i.e. by some
                    // ancestor, dropped or not)? If yes AND the node still
                    // reaches the anchor, leave it for `rewire_or_drop`.
                    // Otherwise it is intrinsically unsourced → prune.
                    if reaches_surviving_anchor {
                        let original_ports: Vec<&PortContract> = ancestors
                            .get(node.id.as_str())
                            .into_iter()
                            .flatten()
                            .filter_map(|anc| by_id.get(anc))
                            .flat_map(|anc| anc.outputs.iter())
                            .collect();
                        // Sourceable originally → rewire case (not unsourced).
                        // Never sourceable → intrinsically unsourced (prune).
                        !any_output_satisfies(&engine, &ctx, &original_ports, input)
                    } else {
                        true
                    }
                });

            if unsourced {
                dropped.insert(node.id.clone());
            }
        }

        if dropped.len() == before {
            break;
        }
    }

    // Companion expansion: for each dropped <base>, also drop
    // `discover_<base>` and `validate_<base>` when present. Collect first
    // (can't mutate `dropped` while iterating it).
    let companions: Vec<String> = dropped
        .iter()
        .flat_map(|base| [format!("discover_{base}"), format!("validate_{base}")])
        .filter(|cand| present.contains(cand.as_str()))
        .collect();
    dropped.extend(companions);

    rewire_or_drop(dag, &dropped);
}

/// Transitive ancestors of `start` following `incoming` (consumer ->
/// producers) backward. Deterministic BFS over a sorted frontier;
/// excludes `start` itself.
///
/// `pub(crate)` so the emit-time backstop in
/// `composer::validation::no_unsourced_required_inputs` reuses the exact
/// same reachability walk the pruner uses (DRY).
pub(crate) fn transitive_ancestors<'a>(
    start: &'a str,
    incoming: &BTreeMap<&'a str, Vec<&'a str>>,
) -> BTreeSet<&'a str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = incoming.get(start).cloned().unwrap_or_default();
    while let Some(cur) = stack.pop() {
        if cur == start || !seen.insert(cur) {
            continue;
        }
        if let Some(preds) = incoming.get(cur) {
            stack.extend(preds.iter().copied());
        }
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
    use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

    /// Build a minimal typed edge for test DAGs.
    fn typed_edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }
    }

    /// `A → B → C`.  Drop B.  Assert: B gone, A→C rewire edge added.
    ///
    /// This is the primary use-case the upcoming `prune_unsourced_atoms`
    /// caller needs: middle-node removal with downstream preserved.
    #[test]
    fn rewire_or_drop_middle_node_rewires_c_to_data_acquisition() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "middle");
        let c = TaskNode::skeleton("C", "downstream");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a.clone(), b.clone(), c.clone()],
            edges: vec![typed_edge("data_acquisition", "B"), typed_edge("B", "C")],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["B".to_string()].into_iter().collect();
        rewire_or_drop(&mut dag, &dropped);

        // B must be gone.
        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(!ids.contains(&"B"), "B must be dropped; nodes={ids:?}");
        assert!(
            ids.contains(&"data_acquisition"),
            "data_acquisition must survive"
        );
        assert!(ids.contains(&"C"), "C must survive");

        // There must be a rewire edge data_acquisition → C.
        let has_rewire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "data_acquisition" && e.to_node == "C");
        assert!(
            has_rewire,
            "expected a rewire edge data_acquisition→C; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );

        // The old B→C edge must be gone.
        let b_to_c = dag.edges.iter().any(|e| e.from_node == "B");
        assert!(!b_to_c, "stale B→C edge must be removed");

        // The rewire edge must be OrderingOnly.
        let rewire_edge = dag
            .edges
            .iter()
            .find(|e| e.from_node == "data_acquisition" && e.to_node == "C")
            .unwrap();
        assert_eq!(
            rewire_edge.kind,
            EdgeKind::OrderingOnly,
            "rewire edge must be OrderingOnly"
        );
    }

    /// When `data_acquisition` itself is dropped, orphaned downstream
    /// nodes must be cascade-dropped rather than rewired.
    #[test]
    fn rewire_or_drop_cascade_drops_when_data_acq_is_gone() {
        let da = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "only node");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![da, b],
            edges: vec![typed_edge("data_acquisition", "B")],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["data_acquisition".to_string(), "B".to_string()]
            .into_iter()
            .collect();
        rewire_or_drop(&mut dag, &dropped);

        assert!(dag.nodes.is_empty(), "both nodes must be dropped");
        assert!(dag.edges.is_empty(), "all edges must be dropped");
    }

    /// Empty `dropped` set is a fast-path no-op — dag unchanged.
    #[test]
    fn rewire_or_drop_empty_dropped_is_noop() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "node");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a, b],
            edges: vec![typed_edge("data_acquisition", "B")],
            ..Default::default()
        };
        let before_nodes = dag.nodes.len();
        let before_edges = dag.edges.len();

        rewire_or_drop(&mut dag, &BTreeSet::new());

        assert_eq!(dag.nodes.len(), before_nodes, "nodes unchanged");
        assert_eq!(dag.edges.len(), before_edges, "edges unchanged");
    }

    /// Multi-hop cascade: `data_acquisition → X → Y → Z`, only
    /// `data_acquisition` in the initial dropped set.
    ///
    /// Because `data_acquisition` is absent, X loses all incoming edges and
    /// must be cascade-dropped.  With X now dropped, Y also loses all
    /// incoming edges and must be cascade-dropped.  With Y dropped, Z does
    /// likewise.  All four nodes end up removed — the original
    /// `prune_excluded_atoms` achieved this because it mutated the `dropped`
    /// BTreeSet *in-place* during the loop, so each subsequent node could
    /// see newly-cascade-dropped predecessors.  The refactored helper must
    /// reproduce that transitive behaviour (via fixpoint iteration).
    #[test]
    fn rewire_or_drop_multihop_cascade_drops_entire_chain() {
        // data_acquisition → X → Y → Z, only data_acquisition in dropped.
        let da = TaskNode::skeleton("data_acquisition", "source");
        let x = TaskNode::skeleton("X", "hop1");
        let y = TaskNode::skeleton("Y", "hop2");
        let z = TaskNode::skeleton("Z", "hop3");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![da, x, y, z],
            edges: vec![
                typed_edge("data_acquisition", "X"),
                typed_edge("X", "Y"),
                typed_edge("Y", "Z"),
            ],
            ..Default::default()
        };

        // Only the root is in the initial dropped set.
        let dropped: BTreeSet<String> = ["data_acquisition".to_string()].into_iter().collect();
        rewire_or_drop(&mut dag, &dropped);

        // All four nodes must be gone — transitive cascade.
        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            ids.is_empty(),
            "all nodes must be cascade-dropped transitively; surviving={ids:?}"
        );
        assert!(dag.edges.is_empty(), "all edges must be removed");
    }

    // ---- prune_unsourced_atoms ------------------------------------------
    //
    // These tests exercise the higher-level pass that COMPUTES the dropped
    // set (atoms with unsourceable required inputs) and then delegates to
    // `rewire_or_drop`.

    use crate::composer_v4::source_typing::GENE_SET_SEMANTIC_IRI;
    use crate::workflow_contracts::semantic_type::SemanticType;
    // `Cardinality` and `PortContract` come in via `super::*`.

    /// EDAM IRI used for the differential-expression results that flow
    /// `de → pathway`. Any IRI works as long as producer + consumer share
    /// it (identical types unify to `Compatible`). Chosen to be distinct
    /// from the gene-set IRI so the two pathway inputs are independent.
    const DE_RESULTS_IRI: &str = "data:3753";

    /// A REQUIRED (`Cardinality::One`) input port of the given semantic
    /// type.
    fn required_input(name: &str, iri: &str) -> PortContract {
        PortContract {
            cardinality: Cardinality::One,
            ..PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
        }
    }

    /// An output port of the given semantic type.
    fn output_port(name: &str, iri: &str) -> PortContract {
        PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
    }

    /// `data:2531` — a generic upstream artifact the `data_acquisition`
    /// anchor always produces (cohort manifest), so DE's own input is
    /// sourceable in these fixtures.
    const COHORT_IRI: &str = "data:2531";

    fn node_with(id: &str, inputs: Vec<PortContract>, outputs: Vec<PortContract>) -> TaskNode {
        let mut n = TaskNode::skeleton(id, id);
        n.inputs = inputs;
        n.outputs = outputs;
        n
    }

    /// `data_acquisition → de → pathway → reporting`, where
    /// `data_acquisition` does NOT expose a gene-set output. `pathway` has
    /// a required gene-set input (unsourceable) plus a DE-results input
    /// (produced by `de`). Expect `pathway` dropped and `reporting`
    /// rewired to `de` (its sole surviving upstream is now `data_acquisition`
    /// via rewire — but `de` survives, so the original `de→...` chain is
    /// preserved through the rewire-or-drop sweep).
    #[test]
    fn prunes_pathway_like_node_when_gene_set_not_sourced() {
        let data_acq = node_with(
            "data_acquisition",
            vec![],
            vec![output_port("cohort", COHORT_IRI)],
        );
        let de = node_with(
            "de",
            vec![required_input("cohort_in", COHORT_IRI)],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        let pathway = node_with(
            "pathway",
            vec![
                required_input("de_in", DE_RESULTS_IRI),
                required_input("gene_set_in", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let reporting = node_with(
            "reporting",
            vec![required_input("report_in", "data:3953")],
            vec![],
        );
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, pathway, reporting],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "pathway"),
                typed_edge("pathway", "reporting"),
            ],
            ..Default::default()
        };

        prune_unsourced_atoms(&mut dag);

        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            !ids.contains(&"pathway"),
            "pathway has an unsourced gene-set input; must be dropped; nodes={ids:?}"
        );
        assert!(ids.contains(&"de"), "de must survive; nodes={ids:?}");
        assert!(
            ids.contains(&"reporting"),
            "reporting must survive (rewired); nodes={ids:?}"
        );
        // reporting lost its only producer (pathway) → rewired to
        // data_acquisition by rewire_or_drop.
        let rewired = dag
            .edges
            .iter()
            .any(|e| e.from_node == "data_acquisition" && e.to_node == "reporting");
        assert!(
            rewired,
            "reporting must be rewired to data_acquisition after pathway drop; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Same topology, but `data_acquisition` HAS a gene-set output port.
    /// Now pathway's gene-set input is sourceable from an upstream-reachable
    /// node → pathway is retained and nothing is pruned.
    #[test]
    fn keeps_pathway_like_node_when_gene_set_sourced() {
        let data_acq = node_with(
            "data_acquisition",
            vec![],
            vec![
                output_port("cohort", COHORT_IRI),
                output_port("gene_set", GENE_SET_SEMANTIC_IRI),
            ],
        );
        let de = node_with(
            "de",
            vec![required_input("cohort_in", COHORT_IRI)],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        let pathway = node_with(
            "pathway",
            vec![
                required_input("de_in", DE_RESULTS_IRI),
                required_input("gene_set_in", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let reporting = node_with(
            "reporting",
            vec![required_input("report_in", "data:3953")],
            vec![],
        );
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, pathway, reporting],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "pathway"),
                typed_edge("pathway", "reporting"),
            ],
            ..Default::default()
        };

        prune_unsourced_atoms(&mut dag);

        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            ids.contains(&"pathway"),
            "gene-set is sourced from data_acquisition; pathway must be retained; nodes={ids:?}"
        );
        assert_eq!(
            dag.nodes.len(),
            4,
            "nothing should be pruned; nodes={ids:?}"
        );
    }

    /// `discover_<base>` / `validate_<base>` companions of a dropped
    /// `<base>` are dropped alongside it.
    #[test]
    fn prunes_discover_and_validate_companions_of_pruned_atom() {
        let data_acq = node_with(
            "data_acquisition",
            vec![],
            vec![output_port("cohort", COHORT_IRI)],
        );
        let de = node_with(
            "de",
            vec![required_input("cohort_in", COHORT_IRI)],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        // pathway has an unsourced gene-set input → dropped.
        let pathway = node_with(
            "pathway",
            vec![
                required_input("de_in", DE_RESULTS_IRI),
                required_input("gene_set_in", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let discover = TaskNode::skeleton("discover_pathway", "discover pathway method");
        let validate = TaskNode::skeleton("validate_pathway", "validate pathway");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, pathway, discover, validate],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "pathway"),
                typed_edge("data_acquisition", "discover_pathway"),
                typed_edge("pathway", "validate_pathway"),
            ],
            ..Default::default()
        };

        prune_unsourced_atoms(&mut dag);

        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            !ids.contains(&"pathway"),
            "pathway must be dropped; nodes={ids:?}"
        );
        assert!(
            !ids.contains(&"discover_pathway"),
            "discover_pathway companion must be dropped; nodes={ids:?}"
        );
        assert!(
            !ids.contains(&"validate_pathway"),
            "validate_pathway companion must be dropped; nodes={ids:?}"
        );
        assert!(ids.contains(&"de"), "de must survive; nodes={ids:?}");
    }

    /// A normal chain where every required input has an upstream producer
    /// of a compatible type → nothing pruned.
    #[test]
    fn keeps_node_whose_required_inputs_are_all_produced_upstream() {
        let data_acq = node_with(
            "data_acquisition",
            vec![],
            vec![output_port("cohort", COHORT_IRI)],
        );
        let de = node_with(
            "de",
            vec![required_input("cohort_in", COHORT_IRI)],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        let reporting = node_with(
            "reporting",
            vec![required_input("de_in", DE_RESULTS_IRI)],
            vec![],
        );
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, reporting],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "reporting"),
            ],
            ..Default::default()
        };
        let before = dag.nodes.len();

        prune_unsourced_atoms(&mut dag);

        assert_eq!(
            dag.nodes.len(),
            before,
            "all required inputs are sourced; nothing should be pruned; nodes={:?}",
            dag.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>()
        );
    }

    /// REGRESSION (cross-omics over-prune): a node with a REQUIRED input
    /// that has no upstream producer because the input is **intake-supplied
    /// metadata** (here `topic:3678`, mirroring
    /// `differential_expression`'s `experimental_design` input) must NOT be
    /// pruned. Only a gene-set-typed unsourced input justifies a prune.
    ///
    /// Before the fix, `prune_unsourced_atoms` flagged EVERY unsourced
    /// required input, so DE (and its whole downstream chain) was dropped
    /// from every cross-omics DAG.
    #[test]
    fn keeps_node_with_unsourced_intake_supplied_required_input() {
        // Anchor is NOT named `data_acquisition` (mirrors the cross-omics
        // `rnaseq_data_acquisition` alias) so `reaches_surviving_anchor` is
        // false — the exact condition that forced the old over-prune.
        let anchor = node_with(
            "rnaseq_data_acquisition",
            vec![],
            vec![output_port("cohort", COHORT_IRI)],
        );
        let norm = node_with(
            "rnaseq_normalisation",
            vec![required_input("cohort_in", COHORT_IRI)],
            vec![output_port("normalized_counts", "data:3917")],
        );
        // DE: `normalized_counts` IS sourced (data:3917 from norm), but
        // `experimental_design` (topic:3678) is intake-supplied — no atom
        // produces it. DE must survive.
        let de = node_with(
            "rnaseq_differential_expression",
            vec![
                required_input("normalized_counts", "data:3917"),
                required_input("experimental_design", "topic:3678"),
            ],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![anchor, norm, de],
            edges: vec![
                typed_edge("rnaseq_data_acquisition", "rnaseq_normalisation"),
                typed_edge("rnaseq_normalisation", "rnaseq_differential_expression"),
            ],
            ..Default::default()
        };
        let before = dag.nodes.len();

        prune_unsourced_atoms(&mut dag);

        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            ids.contains(&"rnaseq_differential_expression"),
            "DE has an intake-supplied (topic:3678) required input with no \
             producer — it must NOT be pruned; nodes={ids:?}"
        );
        assert_eq!(
            dag.nodes.len(),
            before,
            "nothing should be pruned (no gene-set input anywhere); nodes={ids:?}"
        );
    }

    /// Companion of the regression above: even when the gene-set-gated atom
    /// IS pruned, a sibling with an intake-supplied unsourced required
    /// input (the DE node) survives. Mirrors the cross-omics shape where
    /// `pathway_enrichment` drops but `differential_expression` stays.
    #[test]
    fn prunes_gene_set_atom_but_keeps_de_with_intake_input() {
        let anchor = node_with(
            "rnaseq_data_acquisition",
            vec![],
            vec![output_port("cohort", COHORT_IRI)],
        );
        let de = node_with(
            "rnaseq_differential_expression",
            vec![
                required_input("cohort_in", COHORT_IRI),
                required_input("experimental_design", "topic:3678"),
            ],
            vec![output_port("de_results", DE_RESULTS_IRI)],
        );
        // pathway: gene-set input unsourced → must be pruned.
        let pathway = node_with(
            "rnaseq_pathway_enrichment",
            vec![
                required_input("ranked_de_results", DE_RESULTS_IRI),
                required_input("gene_set_collection", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![anchor, de, pathway],
            edges: vec![
                typed_edge("rnaseq_data_acquisition", "rnaseq_differential_expression"),
                typed_edge(
                    "rnaseq_differential_expression",
                    "rnaseq_pathway_enrichment",
                ),
            ],
            ..Default::default()
        };

        prune_unsourced_atoms(&mut dag);

        let ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(
            !ids.contains(&"rnaseq_pathway_enrichment"),
            "pathway's gene-set input is unsourced; must be pruned; nodes={ids:?}"
        );
        assert!(
            ids.contains(&"rnaseq_differential_expression"),
            "DE's only unsourced required input is intake-supplied \
             (topic:3678); DE must survive; nodes={ids:?}"
        );
    }

    /// Rewire is idempotent — calling twice doesn't add duplicate edges.
    #[test]
    fn rewire_or_drop_rewire_is_idempotent() {
        let a = TaskNode::skeleton("data_acquisition", "source");
        let b = TaskNode::skeleton("B", "middle");
        let c = TaskNode::skeleton("C", "downstream");
        let mut dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![a, b, c],
            edges: vec![typed_edge("data_acquisition", "B"), typed_edge("B", "C")],
            ..Default::default()
        };

        let dropped: BTreeSet<String> = ["B".to_string()].into_iter().collect();
        rewire_or_drop(&mut dag, &dropped);
        // second call: B is already gone, C already rewired — should be a no-op
        let dropped2: BTreeSet<String> = BTreeSet::new();
        rewire_or_drop(&mut dag, &dropped2);

        let rewire_count = dag
            .edges
            .iter()
            .filter(|e| e.from_node == "data_acquisition" && e.to_node == "C")
            .count();
        assert_eq!(
            rewire_count, 1,
            "exactly one rewire edge; got {rewire_count}"
        );
    }
}
