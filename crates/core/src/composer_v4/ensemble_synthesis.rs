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

/// The synthesized cross-axis ensemble-aggregator node id: the fan-in
/// over the K×M interpretation grid (`builtin`-marked stub; Plan 3 fills
/// the aggregation logic).
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
    let agg = builtin_aggregator(
        ENSEMBLE_STAT_AGGREGATOR_ID,
        "Aggregate cross-method statistical distribution (Plan 3 fills the logic)",
        "aggregates every method variant's result artifact off disk",
        "stat_distribution",
    );

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

    // ---- Contextualization fan-out (M) + K×M interpretation grid + ensemble aggregator ----
    //
    // The interpretation grid centers on ONE primary statistical base:
    // the first (sorted) schema-bearing target. v1 fans a single primary
    // statistical node out over the interpretive lenses; any additional
    // schema-bearing targets keep their own K statistical variants
    // (fanned out above) but do not seed their own interpretation grid.
    let base_id = targets[0].clone();

    // M contextualization variants — each reads the pooled statistical
    // distribution off the statistical aggregator.
    for lens in &roster.interpretive_lenses {
        let ctx_id = format!("contextualize_findings_with_literature__v_{}", lens.id);
        let mut cn = variant_node(
            atom_reg,
            "contextualize_findings_with_literature",
            &ctx_id,
            "contextualization variant",
        );
        cn.attributes.insert(
            "ensemble_variant".into(),
            serde_json::json!({
                "axis": "contextualization",
                "persona_ref": lens.persona_ref,
                "retrieval": lens.retrieval,
                "lens": lens.id,
            }),
        );
        new_edges.push(ordering_edge(
            ENSEMBLE_STAT_AGGREGATOR_ID,
            "stat_distribution",
            &ctx_id,
            "findings",
        ));
        new_nodes.push(cn);
    }

    // K×M interpretation grid (or the fractional balanced subset). Each
    // cell reads its method variant's result and its lens's literature
    // concordance, and feeds the cross-axis ensemble aggregator.
    for (k, m) in roster.selected_cells() {
        let method = &roster.statistical_variants[k];
        let lens = &roster.interpretive_lenses[m];
        let cell_id = format!(
            "biological_interpretation__m_{}__lens_{}",
            method.id, lens.id
        );
        let mut cell = variant_node(
            atom_reg,
            "biological_interpretation",
            &cell_id,
            "interpretation cell",
        );
        cell.attributes.insert(
            "ensemble_variant".into(),
            serde_json::json!({
                "axis": "interpretive",
                "method": method.tool,
                "method_variant": method.id,
                "persona_ref": lens.persona_ref,
                "model_tier": lens.model_tier,
                "lens": lens.id,
            }),
        );
        // Method-variant result -> cell.
        new_edges.push(ordering_edge(
            &format!("{base_id}__v_{}", method.id),
            "report",
            &cell_id,
            "method_result",
        ));
        // Lens contextualization -> cell.
        new_edges.push(ordering_edge(
            &format!("contextualize_findings_with_literature__v_{}", lens.id),
            "findings",
            &cell_id,
            "literature_concordance",
        ));
        // Cell -> cross-axis ensemble aggregator.
        new_edges.push(ordering_edge(
            &cell_id,
            "interpretation",
            ENSEMBLE_AGGREGATOR_ID,
            "cell_result",
        ));
        new_nodes.push(cell);
    }

    // Cross-axis ensemble aggregator (builtin -> skipped by validate-companion synthesis).
    new_nodes.push(builtin_aggregator(
        ENSEMBLE_AGGREGATOR_ID,
        "Aggregate cross-axis ensemble distribution over the K×M interpretation grid",
        "aggregates every interpretation cell's result artifact off disk",
        "ensemble_distribution",
    ));

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

/// Build a `builtin`-marked aggregator stub: `AtomRole::Operation`,
/// `AnyUpstreamStage` read allowance (it reads every upstream variant's
/// result artifact off disk), `Production` lifecycle, and a single EDAM
/// output port. Shared by the statistical (`stat_distribution`) and the
/// cross-axis ensemble (`ensemble_distribution`) aggregators.
fn builtin_aggregator(id: &str, intent: &str, read_rationale: &str, output_name: &str) -> TaskNode {
    let mut agg = TaskNode::skeleton(id, intent);
    agg.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    agg.attributes
        .insert("builtin".into(), serde_json::Value::String(id.into()));
    agg.attributes.insert(
        "read_allowance".into(),
        serde_json::to_value(vec![crate::atom::ReadAllowance {
            scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
            rationale: read_rationale.into(),
        }])
        .unwrap_or(serde_json::Value::Null),
    );
    agg.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    agg.outputs = vec![PortContract::from_edam(
        output_name,
        Some("data:2048"),
        Some("format:3464"),
    )];
    agg
}

/// Build an ensemble variant node from its base atom (schema/safety
/// inherited) — falling back to a skeleton when the atom is absent — and
/// stamp its id/human_name/machine_name plus the `atom_id` back-reference
/// so schema/safety resolution still finds the underlying atom. The
/// caller stamps `ensemble_variant` and wires edges.
fn variant_node(
    atom_reg: &AtomRegistry,
    atom_id: &str,
    node_id: &str,
    skeleton_intent: &str,
) -> TaskNode {
    let mut n = match atom_reg.get(atom_id) {
        Some(atom) => TaskNode::from_atom(atom),
        None => TaskNode::skeleton(node_id, skeleton_intent),
    };
    n.id = node_id.to_string();
    n.human_name = node_id.to_string();
    n.machine_name = node_id.to_string();
    n.attributes
        .insert("atom_id".into(), serde_json::Value::String(atom_id.into()));
    n
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

    /// Full K×M grid over the shipped `bulk_rnaseq` roster (K=3, M=3):
    /// 3 contextualization variants, 9 interpretation cells (each with
    /// both inbound edges + `ensemble_variant.axis="interpretive"`), and
    /// the `assemble_ensemble_distribution` builtin aggregator fed by
    /// every cell. Also asserts the pass never synthesizes
    /// `assemble_report_data`.
    #[test]
    fn interpretation_grid_full_topology() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        assert_eq!(roster.statistical_variants.len(), 3, "K=3 fixture");
        assert_eq!(roster.interpretive_lenses.len(), 3, "M=3 fixture");

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

        // M contextualization variants.
        for lens in &roster.interpretive_lenses {
            let cid = format!("contextualize_findings_with_literature__v_{}", lens.id);
            let node = dag
                .nodes
                .iter()
                .find(|n| n.id == cid)
                .unwrap_or_else(|| panic!("contextualize variant {cid} present"));
            assert_eq!(
                node.attributes
                    .get("ensemble_variant")
                    .and_then(|v| v.get("axis"))
                    .and_then(|v| v.as_str()),
                Some("contextualization"),
                "{cid} axis"
            );
            assert_eq!(
                node.attributes.get("atom_id").and_then(|v| v.as_str()),
                Some("contextualize_findings_with_literature")
            );
            // Fed by the stat aggregator.
            assert!(
                dag.edges.iter().any(|e| e.from_node == ENSEMBLE_STAT_AGGREGATOR_ID
                    && e.to_node == cid),
                "stat aggregator -> {cid}"
            );
        }
        let ctx_count = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("contextualize_findings_with_literature__v_"))
            .count();
        assert_eq!(ctx_count, 3, "3 contextualization variants");

        // K×M interpretation cells.
        let mut cell_count = 0;
        for method in &roster.statistical_variants {
            for lens in &roster.interpretive_lenses {
                let cell_id =
                    format!("biological_interpretation__m_{}__lens_{}", method.id, lens.id);
                let node = dag
                    .nodes
                    .iter()
                    .find(|n| n.id == cell_id)
                    .unwrap_or_else(|| panic!("cell {cell_id} present"));
                cell_count += 1;
                let variant = node.attributes.get("ensemble_variant").unwrap();
                assert_eq!(
                    variant.get("axis").and_then(|v| v.as_str()),
                    Some("interpretive"),
                    "{cell_id} axis"
                );
                assert_eq!(
                    variant.get("method_variant").and_then(|v| v.as_str()),
                    Some(method.id.as_str())
                );
                assert_eq!(
                    variant.get("persona_ref").and_then(|v| v.as_str()),
                    Some(lens.persona_ref.as_str())
                );
                assert_eq!(
                    variant.get("model_tier").and_then(|v| v.as_str()),
                    Some(lens.model_tier.as_str())
                );
                assert_eq!(
                    node.attributes.get("atom_id").and_then(|v| v.as_str()),
                    Some("biological_interpretation")
                );

                // Inbound: method variant -> cell (method_result).
                let method_src = format!("differential_expression__v_{}", method.id);
                assert!(
                    dag.edges.iter().any(|e| e.from_node == method_src
                        && e.to_node == cell_id
                        && e.to_port == "method_result"),
                    "{method_src} -> {cell_id} (method_result)"
                );
                // Inbound: lens contextualize -> cell (literature_concordance).
                let lens_src =
                    format!("contextualize_findings_with_literature__v_{}", lens.id);
                assert!(
                    dag.edges.iter().any(|e| e.from_node == lens_src
                        && e.to_node == cell_id
                        && e.to_port == "literature_concordance"),
                    "{lens_src} -> {cell_id} (literature_concordance)"
                );
                // Outbound: cell -> ensemble aggregator (cell_result).
                assert!(
                    dag.edges.iter().any(|e| e.from_node == cell_id
                        && e.to_node == ENSEMBLE_AGGREGATOR_ID
                        && e.to_port == "cell_result"),
                    "{cell_id} -> ensemble aggregator (cell_result)"
                );
            }
        }
        assert_eq!(cell_count, 9, "K*M = 9 interpretation cells");

        // Ensemble aggregator is a builtin node.
        let agg = dag
            .nodes
            .iter()
            .find(|n| n.id == ENSEMBLE_AGGREGATOR_ID)
            .expect("ensemble aggregator present");
        assert!(agg.attributes.contains_key("builtin"), "aggregator is builtin");

        // The pass never synthesizes assemble_report_data.
        assert!(
            !dag.nodes.iter().any(|n| n.id == "assemble_report_data"),
            "ensemble pass does not depend on assemble_report_data"
        );
    }

    /// Fractional roster: the grid has exactly `selected_cells().len()`
    /// interpretation cells, and every method + lens is still covered.
    #[test]
    fn interpretation_grid_fractional_cell_count() {
        use crate::atom_registry::AtomRegistry;
        use crate::ensemble_roster::EnsembleRosterProvider;
        use crate::ensemble_roster::FactorialMode;
        use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).unwrap();
        let mut roster = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"))
            .roster_for("bulk_rnaseq")
            .cloned()
            .unwrap();
        roster.factorial = FactorialMode::Fractional;
        let expected = roster.selected_cells().len(); // max(3,3) = 3

        let de = TaskNode::from_atom(reg.get("differential_expression").unwrap());
        let mut dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![de, TaskNode::skeleton("final_reporting", "f")],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };

        synthesize_ensemble_fanout(&mut dag, &reg, &roster);

        let cells = dag
            .nodes
            .iter()
            .filter(|n| n.id.starts_with("biological_interpretation__m_"))
            .count();
        assert_eq!(cells, expected, "fractional cell count == selected_cells().len()");
    }
}
