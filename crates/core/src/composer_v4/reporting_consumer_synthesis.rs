//! Wire dangling analytical atoms into the reporting terminal on a
//! v4 [`WorkflowDag`] after meet-in-the-middle (or after the archetype
//! seed lifts a DAG) and after `synthesize_validate_companions` runs.
//!
//! # Motivation
//!
//! Archetype YAMLs and `compose:` inheritance can leave load-bearing
//! analytical atoms with no downstream consumer other than their own
//! synthesized `validate_*` companion. That makes the atom's output
//! *exist* in the emitted package but never flow into the SME-facing
//! reporting terminal — the SME's narrative report doesn't mention
//! results from atoms whose outputs no path leads to.
//!
//! Concrete patterns this post-pass addresses:
//!
//! 1. **Slot-introduced atoms.** The multi-omics integrator slots
//!    (`integrator: mofa|snf|diablo`) and the single-cell protocol slot
//!    (`protocol: multiome_arc|share_seq`) add atoms via `extra_atoms`
//!    but cannot rewrite an existing atom's `depends_on` to consume the
//!    new atom's output. Without this pass, the integration atom has
//!    only its `validate_*` companion as consumer.
//!
//! 2. **Cross-omics `compose:` branches.** Archetypes like
//!    `cross_omics_rnaseq_atac_chip` pull in three full single-modality
//!    archetypes via `compose:` prefix-rewriting. Each branch brings
//!    its own per-modality `reporting` + `final_reporting` tail, but
//!    the cross-omics `cross_omics_thematic_comparison` only depends on
//!    `cross_omics_alignment_check`, stranding the per-branch
//!    analytical chains.
//!
//! 3. **Optional atoms omitted from reporting's `depends_on`.** When an
//!    archetype's `reporting` node lists only a subset of its
//!    analytical siblings (e.g. only `differential_expression` and
//!    `peak_to_gene_linking`, omitting `pathway_enrichment`), the
//!    omitted atoms become strands.
//!
//! # The fix
//!
//! Walk every node in `dag`. For each *analytical* node (not
//! `validate_*`, `discover_*`, `adapter_*`, the reporting terminals
//! themselves), check whether any downstream consumer in the DAG that
//! ISN'T a validator transitively reaches a reporting terminal. If
//! not, the node is "stranded" and an edge from it to the appropriate
//! intermediate reporting node is synthesized so the strand flows into
//! the SME's report.
//!
//! The pass is conservative: it only ADDS edges. It never removes
//! nodes or rewrites existing edges. Idempotent — re-running on a
//! DAG that already carries the synthesized edges is a no-op.
//!
//! # Choice of reporting consumer
//!
//! The pass picks the consumer for a strand by preferring (in order):
//!
//! 1. An intermediate `reporting`-class node — exact id `reporting`,
//!    or any id ending in `_reporting` or `_thematic_comparison` —
//!    BUT NOT a `final_reporting` (those should aggregate via the
//!    intermediates).
//! 2. A `final_reporting`-class node — exact id `final_reporting`,
//!    or any id ending in `_final_reporting`.
//!
//! When multiple candidates exist, the pass prefers the candidate
//! whose existing `depends_on` overlap with the strand's "branch
//! namespace" (atoms sharing a stage-id prefix). This keeps per-branch
//! strands flowing into per-branch reporting nodes rather than
//! cross-pollinating to other branches' reporting.
//!
//! When NO reporting node exists in the DAG, the pass is a no-op —
//! emit-time validation has bigger problems than strands.
//!
//! # Determinism
//!
//! - The pass iterates `dag.nodes` and `dag.edges` in slice order
//!   (BTreeMap-keyed at every level of upstream code).
//! - New edges sort into `dag.edges` via the same
//!   `(from_node, from_port, to_node, to_port)` key the validate
//!   companion synthesizer uses.
//! - Selection of the reporting consumer is deterministic by id
//!   (alphabetical tie-break on prefix match).

use std::collections::{BTreeMap, BTreeSet};

use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// Walk every analytical node in `dag` and synthesize an edge to the
/// appropriate reporting consumer when the node is stranded (no
/// path to any reporting terminal via non-validator consumers).
/// Mutates `dag` in place; idempotent — re-running is a no-op once
/// strands are wired.
///
/// See module docs for skip rules, consumer selection, and
/// determinism guarantees.
pub fn wire_dangling_analytical_atoms_to_reporting(dag: &mut WorkflowDag) {
    // First, ensure a universal terminal exists. If the DAG only
    // carries aliased reporting nodes (`cross_omics_final_reporting`,
    // `_thematic_comparison`, per-branch `<modality>_final_reporting`),
    // synthesize a bare `final_reporting` node and wire every aliased
    // terminal into it. This makes the DAG's terminal shape
    // compatible with downstream audit tooling that only recognizes
    // the canonical `final_reporting` / `reporting` / `generic_summary`
    // ids (e.g. `scripts/sweep_strand.py`), and gives the SME a single
    // canonical report entry-point regardless of which archetype
    // produced the DAG.
    synthesize_universal_terminal_if_missing(dag);

    // Determine the *universal* terminal set — the nodes whose ids
    // match the audit-script's narrow definition (`reporting`,
    // `final_reporting`, `generic_summary`). These are the ONLY
    // terminals the downstream `sweep_strand.py` script accepts as
    // reachability roots, so reachability for strand-detection must
    // be computed against this set, not the wider `_reporting` /
    // `_final_reporting` / `_thematic_comparison` family.
    let universal: Vec<&TaskNode> = dag
        .nodes
        .iter()
        .filter(|n| is_universal_terminal(&n.id))
        .collect();
    let universal_ids: BTreeSet<&str> = universal.iter().map(|n| n.id.as_str()).collect();
    let universal_present = !universal.is_empty();

    // The "ultimate sink" is what un-routed strands get wired into
    // (transitively, via the per-branch reporting hierarchy when one
    // exists). Prefer the universal terminal; fall back to the
    // alphabetically-first aliased final_reporting / reporting /
    // thematic_comparison.
    let ultimate_sink: Option<String> = if let Some(n) = universal.first() {
        Some(n.id.clone())
    } else {
        // Look for `_final_reporting`-class aliases first
        // (canonical SME-facing terminal shape).
        let mut finals: Vec<&str> = dag
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| id.ends_with("_final_reporting"))
            .collect();
        finals.sort();
        if let Some(pick) = finals.first() {
            Some((*pick).to_string())
        } else {
            // Fall back to any wider reporting-class node.
            let mut wider: Vec<&str> = dag
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .filter(|id| is_reporting_terminal(id))
                .collect();
            wider.sort();
            wider.first().map(|s| (*s).to_string())
        }
    };
    let ultimate_sink = match ultimate_sink {
        Some(s) => s,
        None => return, // no reporting node exists — nothing to do.
    };

    // Reachability roots — what counts as "this node reaches a
    // canonical terminal". When a universal terminal exists, only IT
    // counts (matches sweep_strand.py's narrow definition). Otherwise
    // fall back to the ultimate sink alone — strands all converge
    // onto the same terminal so the SME report is non-fragmented.
    let reachable_roots: BTreeSet<&str> = if universal_present {
        universal_ids.clone()
    } else {
        let mut s = BTreeSet::new();
        s.insert(ultimate_sink.as_str());
        s
    };
    // Always include any node that the ultimate sink already depends
    // on transitively — those are clearly part of the "reaches
    // terminal" closure and should not be re-wired.

    // Build adjacency maps. `consumers[id]` lists nodes whose
    // depends_on includes `id` (modeled via the edge.from_node →
    // edge.to_node direction).
    let node_ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut consumers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &dag.edges {
        if node_ids.contains(e.from_node.as_str()) && node_ids.contains(e.to_node.as_str()) {
            consumers
                .entry(e.from_node.as_str())
                .or_default()
                .insert(e.to_node.as_str());
        }
    }

    // Forward closure of "reaches a reachable_root via non-validator
    // consumers". Iterative fixpoint over dag.nodes.
    let mut effective: BTreeSet<&str> = reachable_roots.clone();
    // Expand the reachable_roots themselves through their producer
    // chain (any node feeding a root via the standard depends_on
    // chain) since those are already reachable.
    // Node lookup for role-aware validator check (catches
    // `external_validation` etc — atoms with role: validation but no
    // `validate_` id prefix).
    let node_by_id: BTreeMap<&str, &TaskNode> =
        dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let is_terminal_consumer = |c: &str| -> bool {
        if is_discoverer(c) {
            return true;
        }
        if let Some(n) = node_by_id.get(c) {
            return is_validator_node(n);
        }
        is_validator(c)
    };
    loop {
        let before = effective.len();
        for n in &dag.nodes {
            if effective.contains(n.id.as_str()) {
                continue;
            }
            if let Some(cs) = consumers.get(n.id.as_str()) {
                for c in cs {
                    if is_terminal_consumer(c) {
                        continue;
                    }
                    if effective.contains(*c) {
                        effective.insert(n.id.as_str());
                        break;
                    }
                }
            }
        }
        if effective.len() == before {
            break;
        }
    }
    // Discard local before borrow; keep effective for later loops.
    let _ = &reachable_roots;

    // Identify strands: nodes NOT in `effective`. We include aliased
    // reporting nodes (e.g. `cross_omics_diablo_final_reporting`) as
    // strand candidates when they don't reach the universal terminal —
    // they themselves need a downstream consumer to merge into the
    // SME's report.
    let mut strands: Vec<&TaskNode> = Vec::new();
    for n in &dag.nodes {
        if !is_eligible_strand_candidate_strict(n, universal_present, &ultimate_sink) {
            continue;
        }
        if effective.contains(n.id.as_str()) {
            continue;
        }
        strands.push(n);
    }
    if strands.is_empty() {
        return;
    }

    // Pick the appropriate reporting consumer for each strand. The
    // consumer must transitively reach the ultimate sink, so we
    // restrict the candidate set to reporting-class nodes whose id is
    // in `effective` (= already reaches the universal terminal) OR
    // is the ultimate sink itself.
    let mut new_edges: Vec<EdgeContract> = Vec::new();
    // Existing edges set for idempotency (string keys).
    let existing_edges: BTreeSet<(String, String)> = dag
        .edges
        .iter()
        .map(|e| (e.from_node.clone(), e.to_node.clone()))
        .collect();

    let candidate_consumer_ids: Vec<String> = dag
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .filter(|id| {
            is_reporting_terminal(id) && (effective.contains(id.as_str()) || id == &ultimate_sink)
        })
        .collect();

    for strand in &strands {
        // An aliased final-reporting strand (e.g.
        // `cross_omics_diablo_final_reporting` when a universal
        // `final_reporting` exists) must wire DIRECTLY into the
        // ultimate sink — its outputs are SME-facing reports that
        // need to converge on the canonical terminal.
        let is_aliased_final =
            strand.id.ends_with("_final_reporting") || strand.id.ends_with("_thematic_comparison");

        let consumer_id = if is_aliased_final && strand.id != ultimate_sink {
            Some(ultimate_sink.clone())
        } else {
            pick_reporting_consumer(
                &strand.id,
                &candidate_consumer_ids,
                &dag.edges,
                &ultimate_sink,
            )
        };
        let consumer_id = match consumer_id {
            Some(c) => c,
            None => continue,
        };

        // Don't wire a node to itself (paranoia guard).
        if consumer_id == strand.id {
            continue;
        }

        // Idempotency guard.
        if existing_edges.contains(&(strand.id.clone(), consumer_id.clone())) {
            continue;
        }

        // Cycle-safety guard: if the proposed consumer already
        // reaches the strand transitively (via existing edges),
        // wiring strand → consumer would close a cycle. This happens
        // when the v4 search wired an analytical atom (e.g.
        // `external_validation`) as a CONSUMER of a reporting
        // terminal — adding a back-edge from the validator to the
        // terminal would Kahn-fail. Skip wiring in this case; the
        // strand stays orphaned (the SME just won't see its output
        // in the canonical report), which is strictly safer than
        // breaking the topological sort.
        if reaches_via_edges(&consumer_id, &strand.id, &dag.edges) {
            continue;
        }

        // Find the consumer node so we can pull its first input port
        // for the diagnostic edge label. Port strings are mostly
        // diagnostic — `lower_to_workflow_json` reads `from_node` /
        // `to_node` for `depends_on`, not the port names.
        let consumer_node = dag.nodes.iter().find(|n| n.id == consumer_id);
        let consumer_port = consumer_node
            .and_then(|n| n.inputs.first())
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let producer_port = strand
            .outputs
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();

        let proof = CompatibilityProof {
            producer_type: strand
                .outputs
                .first()
                .map(|p| p.semantic_type.stable_id())
                .unwrap_or_default(),
            consumer_type: consumer_node
                .and_then(|n| n.inputs.first())
                .map(|p| p.semantic_type.stable_id())
                .unwrap_or_default(),
            rationale: Some(format!(
                "reporting_consumer_synthesis: wired stranded analytical atom {} → {} \
                 (no other non-validator consumer reached a reporting terminal)",
                strand.id, consumer_id
            )),
            ..Default::default()
        };

        new_edges.push(EdgeContract {
            from_node: strand.id.clone(),
            from_port: producer_port,
            to_node: consumer_id,
            to_port: consumer_port,
            proof,
            chain_of_custody: None,
        });
    }

    if new_edges.is_empty() {
        return;
    }

    dag.edges.extend(new_edges);
    // Re-sort edges to keep WorkflowDag byte-stable. Same sort keys
    // as `companion_synthesis.rs::synthesize_validate_companions`.
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Synthesize a bare `final_reporting` node when the DAG only carries
/// aliased reporting nodes (`cross_omics_final_reporting`,
/// per-branch `<modality>_final_reporting`, `_thematic_comparison`).
/// The synthesized node depends on every existing aliased
/// `_final_reporting` node so it transitively aggregates every
/// branch's report. No-op when a bare universal terminal already
/// exists, when the DAG carries no reporting nodes at all, or when
/// the synthesized node would have no upstream (degenerate DAGs).
///
/// Idempotent — re-running on a DAG that already carries the
/// synthesized node is a no-op (the universal-terminal check returns
/// early).
fn synthesize_universal_terminal_if_missing(dag: &mut WorkflowDag) {
    // Early exit when a bare universal terminal is already present.
    if dag.nodes.iter().any(|n| is_universal_terminal(&n.id)) {
        return;
    }

    // Find aliased `_final_reporting` nodes — these are the canonical
    // SME-facing terminals per archetype (per-modality + cross-omics).
    // We aggregate them under a synthesized bare `final_reporting`.
    // Fall back to any `_reporting` / `_thematic_comparison` aliases
    // when no `_final_reporting` aliases exist.
    let mut upstream_aliases: Vec<String> = dag
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .filter(|id| id.ends_with("_final_reporting") && !is_validator(id) && !is_discoverer(id))
        .map(|s| s.to_string())
        .collect();
    if upstream_aliases.is_empty() {
        upstream_aliases = dag
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| {
                (id.ends_with("_reporting") || id.ends_with("_thematic_comparison"))
                    && !is_validator(id)
                    && !is_discoverer(id)
            })
            .map(|s| s.to_string())
            .collect();
    }
    if upstream_aliases.is_empty() {
        // Last-resort fallback: no reporting-class node at all (e.g.
        // search-driven DAGs from the v4 forward/backward search that
        // reach a goal-producing atom but never synthesize a
        // reporting node). Synthesize a bare `final_reporting` AND
        // wire every leaf analytical node (one whose only consumers
        // are validators or which has no consumers at all) into it.
        // This is a structural rescue — the DAG is otherwise stranded
        // from the SME's perspective.
        upstream_aliases = collect_leaf_analytical_nodes(dag);
        if upstream_aliases.is_empty() {
            return; // truly nothing to wire
        }
    }
    upstream_aliases.sort();

    // Build the bare `final_reporting` node. The skeleton-shape +
    // role: Operation attribute is enough for the lowering pass
    // (`backend_emitters/workflow_json.rs::lower_to_workflow_json`)
    // to produce a `Task { kind: TaskKind::Operation,.. }` whose
    // depends_on captures the per-branch reporting tails.
    let mut node = TaskNode::skeleton(
        "final_reporting",
        "Aggregated SME-facing report (synthesized from per-branch reporting tails)",
    );
    node.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    node.attributes.insert(
        "assignee".into(),
        serde_json::to_value(crate::atom::AtomAssignee::Agent).unwrap_or(serde_json::Value::Null),
    );
    node.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;

    // Add a generic output port so the validate-companion synthesizer
    // doesn't skip this node (it requires non-empty outputs).
    // Reporting nodes typically emit a CSV/HTML SME report.
    use crate::workflow_contracts::port::PortContract;
    node.outputs = vec![PortContract::from_edam(
        "report",
        Some("data:0006"),
        Some("format:2331"),
    )];
    node.inputs = vec![PortContract::from_edam(
        "tributaries",
        Some("data:0006"),
        Some("format:2331"),
    )];

    // Build edges from each upstream alias into the new
    // `final_reporting` node.
    let new_node_id = node.id.clone();
    let mut new_edges: Vec<EdgeContract> = Vec::with_capacity(upstream_aliases.len());
    for upstream_id in &upstream_aliases {
        let upstream_outputs = dag
            .nodes
            .iter()
            .find(|n| &n.id == upstream_id)
            .and_then(|n| n.outputs.first().cloned());
        new_edges.push(EdgeContract {
            from_node: upstream_id.clone(),
            from_port: upstream_outputs
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            to_node: new_node_id.clone(),
            to_port: "tributaries".into(),
            proof: CompatibilityProof {
                producer_type: upstream_outputs
                    .as_ref()
                    .map(|p| p.semantic_type.stable_id())
                    .unwrap_or_default(),
                consumer_type: "data:0006".into(),
                rationale: Some(format!(
                    "reporting_consumer_synthesis: synthesized universal terminal \
                     'final_reporting' to aggregate aliased branch tail {upstream_id}"
                )),
                ..Default::default()
            },
            chain_of_custody: None,
        });
    }

    dag.nodes.push(node);
    dag.edges.extend(new_edges);
    // Re-sort to keep WorkflowDag byte-stable.
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Cycle-safety check. Returns `true` when `target` is reachable
/// FORWARD from `source` by following `dag.edges` in their natural
/// direction. Used by the strand-wiring step to refuse adding a
/// back-edge that would close a cycle.
fn reaches_via_edges(source: &str, target: &str, edges: &[EdgeContract]) -> bool {
    let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for e in edges {
        adj.entry(e.from_node.as_str())
            .or_default()
            .push(e.to_node.as_str());
    }
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut stack: Vec<&str> = vec![source];
    while let Some(x) = stack.pop() {
        if x == target {
            return true;
        }
        if !visited.insert(x) {
            continue;
        }
        if let Some(ds) = adj.get(x) {
            for d in ds {
                stack.push(*d);
            }
        }
    }
    false
}

/// Collect leaf analytical nodes — those whose ONLY consumers (or
/// zero consumers) are validators/discoverers. These are the natural
/// upstreams for a synthesized bare `final_reporting` when the DAG
/// carries no reporting-class node at all (e.g. v4 search-driven
/// DAGs that reach a goal-producing atom but never synthesize
/// a reporting node).
fn collect_leaf_analytical_nodes(dag: &WorkflowDag) -> Vec<String> {
    let mut consumers: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for e in &dag.edges {
        consumers
            .entry(e.from_node.as_str())
            .or_default()
            .insert(e.to_node.as_str());
    }
    // Build a lookup of id → node for the role-aware validator check.
    let node_by_id: BTreeMap<&str, &TaskNode> =
        dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let is_terminal_consumer = |c: &str| -> bool {
        if is_discoverer(c) {
            return true;
        }
        if let Some(n) = node_by_id.get(c) {
            return is_validator_node(n);
        }
        is_validator(c)
    };
    let mut leaves: Vec<String> = Vec::new();
    for n in &dag.nodes {
        // Use the narrow id-based predicate here — role-validators
        // like `external_validation` produce SME-facing artifacts and
        // DO need to be wired into the report.
        if is_validator(&n.id) || is_discoverer(&n.id) {
            continue;
        }
        if n.id.starts_with("adapter_") || n.id.contains("_adapter_") {
            continue;
        }
        if n.outputs.is_empty() {
            continue;
        }
        let cs = consumers.get(n.id.as_str());
        let all_non_analytic = cs
            .map(|set| set.iter().all(|c| is_terminal_consumer(c)))
            .unwrap_or(true);
        if all_non_analytic {
            leaves.push(n.id.clone());
        }
    }
    leaves
}

/// Reporting terminal predicate (WIDE). Matches the union of every
/// shape an archetype might use as a terminal, including aliased
/// `cross_omics_final_reporting` / `cross_omics_thematic_comparison`
/// and per-branch `<modality>_reporting` / `<modality>_final_reporting`.
/// Used to decide whether a node is a "reporting-class" candidate at
/// all — strand detection uses the narrower `is_universal_terminal`.
fn is_reporting_terminal(id: &str) -> bool {
    id == "reporting"
        || id == "final_reporting"
        || id == "generic_summary"
        || id.ends_with("_final_reporting")
        || id.ends_with("_reporting")
        || id.ends_with("_thematic_comparison")
}

/// Universal terminal predicate (NARROW). Matches the audit-script's
/// exact-id definition: only `reporting`, `final_reporting`, and
/// `generic_summary` count as roots for "this node reaches the SME's
/// canonical report". When a DAG carries any of these the
/// strand-detection pass treats them as the sole reachability
/// targets; otherwise the pass falls back to an aliased terminal
/// (selected by `ultimate_sink`).
fn is_universal_terminal(id: &str) -> bool {
    id == "reporting" || id == "final_reporting" || id == "generic_summary"
}

/// Intermediate reporting predicate. Same as `is_reporting_terminal`
/// EXCEPT for the `final_reporting` family. Intermediate reporting
/// nodes are the preferred consumer for stranded analytical atoms —
/// `final_reporting` aggregates via the intermediates.
fn is_intermediate_reporting(id: &str) -> bool {
    // Exclude the final_reporting family up front so the suffix-based
    // _reporting test below doesn't accept "final_reporting"
    // (which ends in "_reporting" but is conceptually a terminal,
    // not an intermediate).
    if id == "final_reporting" || id.ends_with("_final_reporting") {
        return false;
    }
    id == "reporting"
        || id == "generic_summary"
        || id.ends_with("_reporting")
        || id.ends_with("_thematic_comparison")
}

/// Validator predicate (id-based). Matches the convention enforced
/// by `companion_synthesis.rs::is_eligible_for_validate_companion`.
fn is_validator(id: &str) -> bool {
    id.starts_with("validate_")
}

/// Validator predicate (node-aware). Catches both `validate_*`-prefix
/// ids AND atoms whose `attributes["role"] == "validation"` (e.g.
/// `external_validation`, which has role: validation but no
/// `validate_` prefix). The role-aware check is needed for cycle
/// safety in the strand-wiring pass — a role-validator that consumes
/// a reporting terminal is a sink, not a strand to be wired back.
fn is_validator_node(node: &TaskNode) -> bool {
    if is_validator(&node.id) {
        return true;
    }
    if let Some(role_value) = node.attributes.get("role") {
        if let Some(role_str) = role_value.as_str() {
            return role_str == "validation";
        }
    }
    false
}

/// Discovery predicate. `discover_*` atoms are self-describing and
/// don't participate in the reporting chain.
fn is_discoverer(id: &str) -> bool {
    id.starts_with("discover_")
}

/// Strict eligibility predicate. Same skip rules as the legacy form,
/// plus the universal-vs-aliased terminal distinction: aliased
/// reporting nodes (`X_final_reporting`, `X_reporting`,
/// `X_thematic_comparison`) ARE strand candidates when a universal
/// terminal is present in the DAG — they themselves need to flow
/// into the universal terminal so the SME's canonical report is
/// non-fragmented. When no universal terminal exists, the alias that
/// IS the ultimate_sink is exempt (it has nowhere to go), but all
/// other aliases are still strand candidates.
fn is_eligible_strand_candidate_strict(
    node: &TaskNode,
    universal_present: bool,
    ultimate_sink: &str,
) -> bool {
    // Only `validate_*`-prefixed ids are pure validators (the audit
    // sweep script's narrow predicate). Atoms with `role: validation`
    // but no `validate_` prefix (e.g. `external_validation`,
    // `calibration_audit`) DO produce SME-facing artifacts that need
    // to reach the canonical report — they're strand candidates too,
    // even though `is_validator_node` excludes them from counting as
    // a downstream terminal in the forward closure.
    if is_validator(&node.id) || is_discoverer(&node.id) {
        return false;
    }
    if node.id.starts_with("adapter_") || node.id.contains("_adapter_") {
        return false;
    }
    if node.outputs.is_empty() {
        return false;
    }
    // Universal terminals are never strand candidates (they ARE the
    // reachability roots).
    if is_universal_terminal(&node.id) {
        return false;
    }
    // The ultimate_sink (whether universal or aliased) is the
    // canonical SME terminal — it can't be its own consumer.
    if node.id == ultimate_sink {
        return false;
    }
    // Otherwise, the node IS a strand candidate. Aliased reporting
    // nodes that don't reach the universal terminal need to be
    // routed; non-reporting analytical atoms need to be routed. The
    // strand-detection step's reachability check decides per-node
    // whether to wire it.
    let _ = universal_present;
    true
}

/// Pick the best reporting consumer for `strand_id`. Returns the
/// chosen node id, or `None` when no reporting node exists.
///
/// Selection rule:
/// 1. Compute the strand's "branch prefix" — the longest common
///    prefix it shares with any other node id, separated at `_`
///    boundaries. (e.g. `rnaseq_pathway_enrichment` → `rnaseq_`).
/// 2. Among reporting nodes, prefer one that:
///    - is an *intermediate* reporting (not `final_reporting`-class),
///    - has a depends_on or id sharing the strand's branch prefix.
/// 3. Fall back to the alphabetically-first intermediate reporting
///    node, then to the alphabetically-first final_reporting node.
fn pick_reporting_consumer(
    strand_id: &str,
    candidate_consumer_ids: &[String],
    edges: &[EdgeContract],
    ultimate_sink: &str,
) -> Option<String> {
    if candidate_consumer_ids.is_empty() {
        return Some(ultimate_sink.to_string());
    }
    let branch_prefix = strand_branch_prefix(strand_id);

    // Pass 1a: intermediate reporting node whose own id starts with
    // the strand's branch prefix — strongest signal that this
    // reporting node owns the branch.
    if let Some(prefix) = &branch_prefix {
        let mut direct: Vec<&String> = candidate_consumer_ids
            .iter()
            .filter(|id| is_intermediate_reporting(id))
            .filter(|id| id.starts_with(prefix))
            .collect();
        direct.sort();
        if let Some(pick) = direct.first() {
            return Some((*pick).clone());
        }

        // Pass 1b: intermediate reporting node whose existing
        // upstreams (via edges) include nodes sharing the strand's
        // branch prefix. Weaker signal but still preserves per-branch
        // grouping.
        let mut indirect: Vec<&String> = candidate_consumer_ids
            .iter()
            .filter(|id| is_intermediate_reporting(id))
            .filter(|id| {
                edges
                    .iter()
                    .any(|e| e.to_node == **id && e.from_node.starts_with(prefix))
            })
            .collect();
        indirect.sort();
        if let Some(pick) = indirect.first() {
            return Some((*pick).clone());
        }
    }

    // Pass 2: any intermediate reporting node.
    let mut intermediates: Vec<&String> = candidate_consumer_ids
        .iter()
        .filter(|id| is_intermediate_reporting(id))
        .collect();
    intermediates.sort();
    if let Some(pick) = intermediates.first() {
        return Some((*pick).clone());
    }

    // Pass 3: fall back to the ultimate sink.
    Some(ultimate_sink.to_string())
}

/// Extract the longest leading `<prefix>_` segment from `id` that's
/// likely a branch namespace. Returns `None` when the id has no
/// underscore-segmented prefix.
fn strand_branch_prefix(id: &str) -> Option<String> {
    // Recognized branch namespaces are the per-modality prefixes used
    // in cross-omics composition: `rnaseq_`, `atac_`, `chip_`,
    // `proteomics_`, `methylation_`, `wgs_`, `gwas_`, `metabolomics_`,
    // `microbiome_`, `metagenomics_`. We don't hardcode the set —
    // instead we treat any `<word>_` leading segment as a candidate.
    if let Some(idx) = id.find('_') {
        // Guard: don't treat single-letter prefixes (e.g. `a_`) as a
        // branch namespace.
        if idx >= 2 {
            return Some(id[..=idx].to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use crate::workflow_contracts::port::PortContract;

    fn node_with_output(id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, format!("intent for {id}"));
        n.outputs = vec![PortContract::from_edam(
            "out",
            Some("data:0006"),
            Some("format:1915"),
        )];
        n.inputs = vec![PortContract::from_edam(
            "in",
            Some("data:0006"),
            Some("format:1915"),
        )];
        n
    }

    fn simple_edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        }
    }

    fn dag_with(nodes: Vec<TaskNode>, edges: Vec<EdgeContract>) -> WorkflowDag {
        WorkflowDag {
            id: "test".into(),
            nodes,
            edges,
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    /// Basic strand: integrate atom + validate companion + reporting.
    /// Without the post-pass `integrate` reaches only its validator.
    #[test]
    fn strand_wires_integration_to_reporting() {
        let mut dag = dag_with(
            vec![
                node_with_output("data_acquisition"),
                node_with_output("integrate_multi_omics_mofa"),
                node_with_output("validate_integrate_multi_omics_mofa"),
                node_with_output("differential_expression"),
                node_with_output("final_reporting"),
            ],
            vec![
                simple_edge("data_acquisition", "integrate_multi_omics_mofa"),
                simple_edge(
                    "integrate_multi_omics_mofa",
                    "validate_integrate_multi_omics_mofa",
                ),
                simple_edge("differential_expression", "final_reporting"),
            ],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        let has_wire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "integrate_multi_omics_mofa" && e.to_node == "final_reporting");
        assert!(
            has_wire,
            "strand-to-reporting wire missing; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Validators must not count as a "reaches reporting" consumer —
    /// they're terminal companions.
    #[test]
    fn validator_consumer_does_not_count_as_reporting_path() {
        let mut dag = dag_with(
            vec![
                node_with_output("strand_atom"),
                node_with_output("validate_strand_atom"),
                node_with_output("final_reporting"),
            ],
            vec![simple_edge("strand_atom", "validate_strand_atom")],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        let has_wire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "strand_atom" && e.to_node == "final_reporting");
        assert!(
            has_wire,
            "strand wasn't wired despite only consumer being a validator; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Idempotency: running twice doesn't duplicate edges.
    #[test]
    fn idempotent_when_strand_already_wired() {
        let mut dag = dag_with(
            vec![
                node_with_output("strand_atom"),
                node_with_output("final_reporting"),
            ],
            vec![simple_edge("strand_atom", "final_reporting")],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);
        let after_first = dag.edges.len();
        wire_dangling_analytical_atoms_to_reporting(&mut dag);
        assert_eq!(
            dag.edges.len(),
            after_first,
            "second pass added edges (not idempotent)"
        );
    }

    /// Strand prefers an intermediate reporting node over the final
    /// reporting when both exist.
    #[test]
    fn prefers_intermediate_reporting_over_final() {
        let mut dag = dag_with(
            vec![
                node_with_output("strand_atom"),
                node_with_output("reporting"),
                node_with_output("final_reporting"),
            ],
            vec![simple_edge("reporting", "final_reporting")],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        let has_intermediate = dag
            .edges
            .iter()
            .any(|e| e.from_node == "strand_atom" && e.to_node == "reporting");
        let has_final = dag
            .edges
            .iter()
            .any(|e| e.from_node == "strand_atom" && e.to_node == "final_reporting");
        assert!(
            has_intermediate,
            "strand should route into intermediate reporting; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
        assert!(
            !has_final,
            "strand routed into final_reporting despite intermediate existing"
        );
    }

    /// Discoveries and adapters are not strand candidates — they
    /// don't need a reporting consumer.
    #[test]
    fn skips_discoveries_and_adapters() {
        let mut dag = dag_with(
            vec![
                node_with_output("discover_alignment"),
                node_with_output("adapter_count_matrix"),
                node_with_output("final_reporting"),
            ],
            vec![],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        for e in &dag.edges {
            assert!(
                e.from_node != "discover_alignment" && e.from_node != "adapter_count_matrix",
                "synthesized edge from a non-eligible node: {} → {}",
                e.from_node,
                e.to_node
            );
        }
    }

    /// Validators themselves are skipped — no self-strand-wiring.
    #[test]
    fn skips_validator_nodes() {
        let mut dag = dag_with(
            vec![
                node_with_output("validate_strand_atom"),
                node_with_output("final_reporting"),
            ],
            vec![],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        for e in &dag.edges {
            assert!(
                e.from_node != "validate_strand_atom",
                "synthesized edge from a validator: {} → {}",
                e.from_node,
                e.to_node
            );
        }
    }

    /// Last-resort rescue: when no reporting node exists at all
    /// (e.g. v4 search-driven DAG that lands on a goal-producing atom
    /// without synthesizing a reporting node), the pass synthesizes
    /// a bare `final_reporting` node and wires every leaf analytical
    /// node into it. Without this rescue the SME's emitted package
    /// has no canonical report and every analytical atom is stranded
    /// from the audit-script's perspective.
    #[test]
    fn synthesizes_final_reporting_when_no_reporting_exists() {
        let mut dag = dag_with(
            vec![
                node_with_output("strand_atom"),
                node_with_output("validate_strand_atom"),
            ],
            vec![simple_edge("strand_atom", "validate_strand_atom")],
        );
        wire_dangling_analytical_atoms_to_reporting(&mut dag);
        let ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();
        assert!(
            ids.contains("final_reporting"),
            "rescue pass should synthesize a bare final_reporting node; got {ids:?}"
        );
        // The leaf analytical atom should be wired into the synthesized
        // terminal directly (not just left stranded via a validator).
        let has_wire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "strand_atom" && e.to_node == "final_reporting");
        assert!(
            has_wire,
            "strand should be wired to synthesized final_reporting; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
    }

    /// Per-branch routing: a `rnaseq_*` strand prefers an intermediate
    /// reporting node that already aggregates rnaseq atoms.
    #[test]
    fn prefers_branch_matching_reporting_when_present() {
        let mut dag = dag_with(
            vec![
                node_with_output("rnaseq_differential_expression"),
                node_with_output("rnaseq_pathway_enrichment"),
                node_with_output("rnaseq_reporting"),
                node_with_output("atac_peak_calling"),
                node_with_output("atac_reporting"),
                node_with_output("cross_omics_thematic_comparison"),
                node_with_output("cross_omics_final_reporting"),
            ],
            vec![
                simple_edge("rnaseq_differential_expression", "rnaseq_reporting"),
                simple_edge("atac_peak_calling", "atac_reporting"),
                simple_edge("rnaseq_reporting", "cross_omics_thematic_comparison"),
                simple_edge("atac_reporting", "cross_omics_thematic_comparison"),
                simple_edge(
                    "cross_omics_thematic_comparison",
                    "cross_omics_final_reporting",
                ),
            ],
        );

        wire_dangling_analytical_atoms_to_reporting(&mut dag);

        // rnaseq_pathway_enrichment is a strand; should route to
        // rnaseq_reporting (matches branch prefix), not into the
        // cross-omics tail.
        let has_rnaseq_wire = dag
            .edges
            .iter()
            .any(|e| e.from_node == "rnaseq_pathway_enrichment" && e.to_node == "rnaseq_reporting");
        assert!(
            has_rnaseq_wire,
            "branch-namespace pathway_enrichment didn't route into branch-matching reporting; \
             edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
    }
}
