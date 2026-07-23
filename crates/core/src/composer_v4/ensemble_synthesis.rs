//! Fan out the schema-bearing statistical node into K method variants
//! plus a statistical-distribution aggregator stub on a v4
//! [`WorkflowDag`].
//!
//! Gated by `ctx.compose_ensemble` (`ECAA_COMPOSE_ENSEMBLE`, default
//! off) — mirrors the shape of `interpretation_synthesis.rs` and
//! `report_data_synthesis.rs`: a self-contained post-pass invoked from
//! `planner::plan` once a roster is available for the composed
//! modality, no-op otherwise.
//!
//! # Method-neutrality reconciliation
//!
//! The rest of this codebase forbids the LLM from ever selecting a
//! methodological choice (`prompt_role.txt`; `set_intake_method` is
//! allowed only when the SME names a method unprompted). This pass does
//! not violate that contract: the composer — not the LLM — instantiates
//! a config-declared method multiverse. Every variant's tool comes from
//! the modality's shipped `config/ensemble-rosters/<modality>.yaml`
//! (validated against the base atom's `candidate_tools` by
//! `ensemble_roster::validate_variant_tools`), so no runtime inference
//! chooses a method; the roster is authored and reviewed ahead of time,
//! same as an atom or archetype YAML.
//!
//! # Shape
//!
//! For every node in the DAG whose atom (resolved by id, or by its
//! `attributes["atom_id"]` back-reference) declares a `result_schema`
//! (the same fan-out-target selector `report_data_synthesis` uses):
//!
//! - Replace the base node with one `<base_id>__v_<variant.id>` clone
//!   per `roster.statistical_variants` entry. Each clone keeps
//!   `attributes["atom_id"] = <base_id>` (so schema/safety resolution
//!   still finds the underlying atom) and stamps
//!   `attributes["ensemble_variant"] = { axis: "statistical", method,
//!   variant_id, bootstrap_replicates }`.
//! - Rewire every inbound edge that fed the base node onto EACH variant
//!   (fan-out on the input side).
//! - Wire every variant to the `assemble_statistical_distribution`
//!   builtin aggregator (fan-in on the output side; Plan 3 fills in the
//!   aggregation logic — this pass only stubs the node + edges).
//! - Remove the base target nodes and every edge touching them (inbound
//!   AND outbound) — any outbound edge that fed `reporting` /
//!   `final_reporting` / `assemble_report_data` directly is
//!   intentionally dropped here; a later pass wires the aggregator
//!   onward.
//!
//! # Skip rule
//!
//! No-op when no node in the DAG resolves to an atom with a declared
//! `result_schema` — mirrors `report_data_synthesis`'s reduced
//! contract for a workflow with no tabular analytical result.
//!
//! # Determinism + idempotency
//!
//! - A node id `"assemble_statistical_distribution"` already present in
//!   the DAG is the idempotency guard — re-running the pass is a no-op.
//! - Fan-out targets are collected then sorted by id before iteration,
//!   so variant/edge generation order never depends on `dag.nodes`'
//!   incoming order.
//! - After appending the new nodes + edges and removing the base
//!   targets, `dag.nodes` and `dag.edges` are re-sorted by the same
//!   canonical keys the sibling synthesis passes use (id for nodes;
//!   `(from_node, from_port, to_node, to_port)` for edges), so the
//!   emitted DAG stays byte-stable.

use crate::atom_registry::AtomRegistry;
use crate::ensemble_roster::EnsembleRoster;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// The synthesized statistical-aggregator node id (Plan 3 fills the
/// aggregation logic; here it is a `builtin`-marked stub).
pub const ENSEMBLE_STAT_AGGREGATOR_ID: &str = "assemble_statistical_distribution";

/// The synthesized cross-axis ensemble-aggregator node id (reserved for
/// the interpretive-lens fan-out pass; unused by this pass today).
pub const ENSEMBLE_AGGREGATOR_ID: &str = "assemble_ensemble_distribution";

/// Fan the schema-bearing statistical node(s) out over the roster's
/// method variants, and inject the statistical-distribution aggregator
/// stub. No-op when: no schema-bearing node exists, or the pass has
/// already run (idempotent). See module docs.
pub fn synthesize_ensemble_fanout(
    dag: &mut WorkflowDag,
    atom_reg: &AtomRegistry,
    roster: &EnsembleRoster,
) {
    // Idempotency guard.
    if dag
        .nodes
        .iter()
        .any(|n| n.id == ENSEMBLE_STAT_AGGREGATOR_ID)
    {
        return;
    }

    // Fan-out targets: schema-bearing analytical nodes (verbatim
    // selector from `report_data_synthesis.rs`).
    let mut targets: Vec<String> = Vec::new();
    for node in &dag.nodes {
        let atom = atom_reg.get(&node.id).or_else(|| {
            node.attributes
                .get("atom_id")
                .and_then(|v| v.as_str())
                .and_then(|id| atom_reg.get(id))
        });
        if atom.map(|a| a.result_schema.is_some()).unwrap_or(false) {
            targets.push(node.id.clone());
        }
    }
    targets.sort();
    if targets.is_empty() {
        return;
    }

    let mut new_nodes: Vec<TaskNode> = Vec::new();
    let mut new_edges: Vec<EdgeContract> = Vec::new();

    // Statistical aggregator stub (builtin -> skipped by validate-companion synthesis).
    let mut agg = TaskNode::skeleton(
        ENSEMBLE_STAT_AGGREGATOR_ID,
        "Aggregate cross-method statistical distribution (Plan 3 fills the logic)",
    );
    agg.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    agg.attributes.insert(
        "builtin".into(),
        serde_json::Value::String(ENSEMBLE_STAT_AGGREGATOR_ID.into()),
    );
    agg.attributes.insert(
        "read_allowance".into(),
        serde_json::to_value(vec![crate::atom::ReadAllowance {
            scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
            rationale: "aggregates every method variant's result artifact off disk".into(),
        }])
        .unwrap_or(serde_json::Value::Null),
    );
    agg.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    agg.outputs = vec![PortContract::from_edam(
        "stat_distribution",
        Some("data:2048"),
        Some("format:3464"),
    )];

    for base_id in &targets {
        // Capture the base node's inbound edges (upstream -> base) to rewire onto each variant.
        let inbound: Vec<EdgeContract> = dag
            .edges
            .iter()
            .filter(|e| &e.to_node == base_id)
            .cloned()
            .collect();
        let base_node = dag.nodes.iter().find(|n| &n.id == base_id).cloned();
        for sv in &roster.statistical_variants {
            let vid = format!("{base_id}__v_{}", sv.id);
            let mut vn = base_node
                .clone()
                .unwrap_or_else(|| TaskNode::skeleton(&vid, "statistical variant"));
            vn.id = vid.clone();
            vn.human_name = vid.clone();
            vn.machine_name = vid.clone();
            // Keep atom_id pointing at the base atom (schema/safety inherited; resolver still works).
            vn.attributes
                .insert("atom_id".into(), serde_json::Value::String(base_id.clone()));
            vn.attributes.insert(
                "ensemble_variant".into(),
                serde_json::json!({
                    "axis": "statistical",
                    "method": sv.tool,
                    "variant_id": sv.id,
                    "bootstrap_replicates": sv.bootstrap_replicates,
                }),
            );
            // Upstream -> variant (rewire each inbound edge onto the variant).
            for e in &inbound {
                new_edges.push(ordering_edge(&e.from_node, &e.from_port, &vid, "input"));
            }
            // Variant -> statistical aggregator.
            new_edges.push(ordering_edge(
                &vid,
                "report",
                ENSEMBLE_STAT_AGGREGATOR_ID,
                "method_result",
            ));
            new_nodes.push(vn);
        }
    }

    // Remove base target nodes + every edge touching them (replaced by variants + aggregator).
    dag.nodes.retain(|n| !targets.contains(&n.id));
    dag.edges
        .retain(|e| !targets.contains(&e.from_node) && !targets.contains(&e.to_node));

    dag.nodes.push(agg);
    dag.nodes.extend(new_nodes);
    dag.edges.extend(new_edges);

    resort(dag);
}

/// Build an `OrderingOnly` edge; the port strings are diagnostic — the
/// lowering pass only reads `from_node`/`to_node` for `depends_on`.
/// Mirrors `report_data_synthesis.rs::ordering_edge` /
/// `interpretation_synthesis.rs::ordering_edge` exactly.
fn ordering_edge(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
    EdgeContract {
        from_node: from.into(),
        from_port: from_port.into(),
        to_node: to.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof {
            rationale: Some(format!(
                "ensemble_synthesis: wired {from} -> {to} ({to_port})"
            )),
            ..Default::default()
        },
        kind: EdgeKind::OrderingOnly,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

/// Re-sort nodes/edges by their canonical keys so the DAG stays
/// byte-stable regardless of iteration order. Same keys as the sibling
/// synthesis passes.
fn resort(dag: &mut WorkflowDag) {
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `differential_expression` node (declares `result_schema`)
    /// plus `reporting`/`final_reporting` terminals, fanned out over
    /// the shipped `bulk_rnaseq` roster's 3 statistical variants.
    #[test]
    fn statistical_fanout_creates_k_variants_and_aggregator() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![
                de,
                TaskNode::skeleton("reporting", "r"),
                TaskNode::skeleton("final_reporting", "f"),
            ],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster);

        let variant_ids: Vec<&str> = dag
            .nodes
            .iter()
            .map(|n| n.id.as_str())
            .filter(|id| id.starts_with("differential_expression__v_"))
            .collect();
        assert_eq!(variant_ids.len(), 3, "K=3 variants: {variant_ids:?}");
        assert!(
            !dag.nodes.iter().any(|n| n.id == "differential_expression"),
            "base node replaced"
        );
        assert!(
            dag.nodes
                .iter()
                .any(|n| n.id == "assemble_statistical_distribution"
                    && n.attributes.contains_key("builtin")),
            "stat aggregator is builtin"
        );
        for v in &variant_ids {
            assert!(
                dag.edges
                    .iter()
                    .any(|e| e.from_node == **v && e.to_node == "assemble_statistical_distribution"),
                "{v} -> stat aggregator edge"
            );
            let node = dag.nodes.iter().find(|n| &n.id == v).unwrap();
            assert_eq!(
                node.attributes.get("atom_id").and_then(|x| x.as_str()),
                Some("differential_expression")
            );
            assert_eq!(
                node.attributes
                    .get("ensemble_variant")
                    .and_then(|x| x.get("axis"))
                    .and_then(|x| x.as_str()),
                Some("statistical")
            );
        }

        let n0 = dag.nodes.len();
        let e0 = dag.edges.len();
        synthesize_ensemble_fanout(&mut dag, &reg, &roster);
        assert_eq!(dag.nodes.len(), n0, "idempotent nodes");
        assert_eq!(dag.edges.len(), e0, "idempotent edges");
    }
}
