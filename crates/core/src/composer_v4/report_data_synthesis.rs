//! Synthesize the `assemble_report_data` companion node on a v4
//! [`WorkflowDag`].
//!
//! The core `assemble_report_data` assembler
//! (`report_contract::assemble::assemble_report_data`) reads every
//! terminal analytical atom's declared
//! [`ResultSchema`](crate::report_contract::ResultSchema) and writes
//! `runtime/outputs/reporting/report-data.json` for the reporting
//! agent to narrate over. This pass is what wires that assembler into
//! the DAG as a real task: it collects the `(stage_id, ResultSchema)`
//! pairs present in the composed DAG, synthesizes one
//! `assemble_report_data` node stamped with that map (so the lowering
//! pass can fold it into the task's `spec.report_schemas` — see
//! `backend_emitters/workflow_json.rs`), and wires it downstream of
//! every schema-bearing stage and upstream of the `reporting` /
//! `final_reporting` terminals.
//!
//! # Skip rule
//!
//! When NO node in the DAG resolves to an atom with a declared
//! `result_schema`, the pass is a no-op — a workflow with no tabular
//! analytical result (e.g. a pure QC/discovery pipeline) gets the
//! reduced contract: no assembler task, no `report-data.json`.
//!
//! # Idempotency
//!
//! A node id `"assemble_report_data"` already present in the DAG is
//! the idempotency guard — re-running the pass is a no-op.
//!
//! # Determinism
//!
//! - The schema map is a `BTreeMap` keyed by stage id, so its
//!   serialized form is byte-stable regardless of node iteration
//!   order.
//! - After appending the new node + edges, `dag.nodes` and
//!   `dag.edges` are re-sorted by the same canonical keys the sibling
//!   synthesis passes use (id for nodes; `(from, from_port, to,
//!   to_port)` for edges) — see
//!   `companion_synthesis::synthesize_validate_companions`.

use std::collections::BTreeMap;

use crate::atom_registry::AtomRegistry;
use crate::report_contract::ResultSchema;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// The synthesized node's id — also its `atom_id` / `builtin` marker.
const NODE_ID: &str = "assemble_report_data";

/// Reporting-terminal ids the synthesized node feeds when present.
/// Deliberately narrow (exact-id match only) — this pass doesn't
/// attempt the wider `_reporting`/`_final_reporting` alias matching
/// `reporting_consumer_synthesis` does; it only targets the two
/// canonical terminal ids.
const REPORTING_TERMINAL_IDS: [&str; 2] = ["reporting", "final_reporting"];

/// Collect every `(stage_id, ResultSchema)` pair declared on `dag`'s
/// nodes and, if any exist, synthesize the `assemble_report_data`
/// companion + wire it between the schema-bearing stages and the
/// `reporting`/`final_reporting` terminals. No-op when the DAG
/// declares no result schemas, or when `assemble_report_data` already
/// exists (idempotent). See module docs.
pub fn synthesize_report_data_companion(dag: &mut WorkflowDag, atom_reg: &AtomRegistry) {
    // Idempotency: already synthesized.
    if dag.nodes.iter().any(|n| n.id == NODE_ID) {
        return;
    }

    // Resolve each node's atom — prefer an exact registry hit on the
    // node's own id, else fall back to its `attributes["atom_id"]`
    // back-reference (synthesized companions stamp this even though
    // their node id may differ from the underlying atom id).
    let mut schemas: BTreeMap<String, ResultSchema> = BTreeMap::new();
    let mut node_ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
    node_ids.sort();
    for node in &dag.nodes {
        let atom = atom_reg.get(&node.id).or_else(|| {
            node.attributes
                .get("atom_id")
                .and_then(|v| v.as_str())
                .and_then(|id| atom_reg.get(id))
        });
        if let Some(atom) = atom {
            if let Some(schema) = &atom.result_schema {
                schemas.insert(node.id.clone(), schema.clone());
            }
        }
    }

    // Reduced contract: no schema-bearing stage → no assembler task.
    if schemas.is_empty() {
        return;
    }

    let mut node = TaskNode::skeleton(
        NODE_ID,
        "Assemble the deterministic report-data contract from every terminal analytical result",
    );
    node.attributes.insert(
        "role".into(),
        serde_json::to_value(crate::atom::AtomRole::Operation).unwrap_or(serde_json::Value::Null),
    );
    node.attributes.insert(
        "assignee".into(),
        serde_json::to_value(crate::atom::AtomAssignee::Agent).unwrap_or(serde_json::Value::Null),
    );
    node.attributes
        .insert("atom_id".into(), serde_json::Value::String(NODE_ID.into()));
    // Marker a later harness task keys on to run the core assembler
    // (as opposed to dispatching an executing agent).
    node.attributes
        .insert("builtin".into(), serde_json::Value::String(NODE_ID.into()));
    node.attributes.insert(
        "report_schemas".into(),
        serde_json::to_value(&schemas).unwrap_or(serde_json::Value::Null),
    );
    // The assembler reads every schema-bearing stage's result artifact
    // directly off disk by declared path (not through typed data-flow
    // edges alone), so it needs the same broad read scope a validator
    // gets — see `companion_synthesis.rs`'s identical rationale.
    node.attributes.insert(
        "read_allowance".into(),
        serde_json::to_value(vec![crate::atom::ReadAllowance {
            scope: crate::atom::ReadAllowanceScope::AnyUpstreamStage,
            rationale: "assemble_report_data reads every schema-bearing terminal analytical \
                        stage's declared result artifact directly off disk; those reads are not \
                        all declared edges of the assembler itself."
                .into(),
        }])
        .unwrap_or(serde_json::Value::Null),
    );
    node.lifecycle_state = crate::workflow_contracts::lifecycle::LifecycleState::Production;
    node.outputs = vec![PortContract::from_edam(
        "report_data",
        Some("data:2048"),
        Some("format:3464"),
    )];

    let mut new_edges: Vec<EdgeContract> = Vec::new();

    // Schema-bearing stage -> assemble_report_data.
    for stage_id in schemas.keys() {
        new_edges.push(ordering_edge(stage_id, "report", NODE_ID, "analysis_result"));
    }

    // assemble_report_data -> reporting / final_reporting, only for
    // terminals that already exist as node ids in the dag.
    for terminal_id in REPORTING_TERMINAL_IDS {
        if dag.nodes.iter().any(|n| n.id == terminal_id) {
            new_edges.push(ordering_edge(NODE_ID, "report_data", terminal_id, "tributaries"));
        }
    }

    dag.nodes.push(node);
    dag.edges.extend(new_edges);

    // Re-sort to keep the WorkflowDag byte-stable. Same keys as the
    // sibling synthesis passes.
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Build an `OrderingOnly` edge; the port strings are diagnostic —
/// `lower_to_workflow_json` only reads `from_node`/`to_node` for
/// `depends_on`. Mirrors `interpretation_synthesis.rs::ordering_edge`
/// exactly (the proven-safe pattern for reporting-adjacent synthesized
/// edges).
fn ordering_edge(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
    EdgeContract {
        from_node: from.into(),
        from_port: from_port.into(),
        to_node: to.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof {
            rationale: Some(format!(
                "report_data_synthesis: wired {from} -> {to} ({to_port})"
            )),
            ..Default::default()
        },
        kind: EdgeKind::OrderingOnly,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use crate::workflow_contracts::evidence::AssumptionLedger;

    fn atom_registry() -> AtomRegistry {
        AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
            .expect("stage-atoms load")
    }

    fn plain_node(id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, format!("intent for {id}"));
        n.outputs = vec![PortContract::from_edam(
            "out",
            Some("data:0006"),
            Some("format:2331"),
        )];
        n.inputs = vec![PortContract::from_edam(
            "in",
            Some("data:0006"),
            Some("format:2331"),
        )];
        n
    }

    fn dag_with(nodes: Vec<TaskNode>, edges: Vec<EdgeContract>) -> WorkflowDag {
        WorkflowDag {
            id: "t".into(),
            nodes,
            edges,
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    fn simple_edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: "out".into(),
            to_node: to.into(),
            to_port: "in".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::OrderingOnly,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }
    }

    /// Real `differential_expression` atom (declares `result_schema`
    /// per `config/stage-atoms/differential_expression.yaml`) plus
    /// `reporting` and `final_reporting` terminals, with a pre-existing
    /// `reporting -> final_reporting` edge. Asserts the synthesized
    /// node + every wiring rule.
    #[test]
    fn synthesizes_node_and_wires_schema_bearing_stage_to_terminals() {
        let reg = atom_registry();
        let de_atom = reg.get("differential_expression").expect("atom present");
        assert!(
            de_atom.result_schema.is_some(),
            "precondition: differential_expression must declare result_schema"
        );
        let de_node = TaskNode::from_atom(de_atom);

        let mut dag = dag_with(
            vec![de_node, plain_node("reporting"), plain_node("final_reporting")],
            vec![simple_edge("reporting", "final_reporting")],
        );

        synthesize_report_data_companion(&mut dag, &reg);

        let assembled = dag
            .nodes
            .iter()
            .find(|n| n.id == "assemble_report_data")
            .expect("assemble_report_data node must be synthesized");
        assert_eq!(
            assembled.attributes.get("builtin").and_then(|v| v.as_str()),
            Some("assemble_report_data")
        );
        let schemas_val = assembled
            .attributes
            .get("report_schemas")
            .expect("report_schemas attribute must be present");
        let schemas: BTreeMap<String, ResultSchema> =
            serde_json::from_value(schemas_val.clone()).expect("report_schemas deserializes");
        assert!(
            !schemas.is_empty(),
            "report_schemas must be non-empty when a schema-bearing stage exists"
        );
        assert!(schemas.contains_key("differential_expression"));

        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "differential_expression"
                    && e.to_node == "assemble_report_data"),
            "differential_expression -> assemble_report_data edge missing; edges={:?}",
            dag.edges
                .iter()
                .map(|e| (e.from_node.as_str(), e.to_node.as_str()))
                .collect::<Vec<_>>()
        );
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "assemble_report_data" && e.to_node == "reporting"),
            "assemble_report_data -> reporting edge missing"
        );
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "assemble_report_data" && e.to_node == "final_reporting"),
            "assemble_report_data -> final_reporting edge missing"
        );
        // Pre-existing edge into reporting must survive untouched.
        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "reporting" && e.to_node == "final_reporting"),
            "pre-existing reporting -> final_reporting edge must be preserved"
        );
    }

    /// Idempotency: running twice yields exactly one
    /// `assemble_report_data` node and no duplicate edges.
    #[test]
    fn synthesis_is_idempotent() {
        let reg = atom_registry();
        let de_node = TaskNode::from_atom(reg.get("differential_expression").expect("atom"));
        let mut dag = dag_with(
            vec![de_node, plain_node("reporting"), plain_node("final_reporting")],
            vec![simple_edge("reporting", "final_reporting")],
        );

        synthesize_report_data_companion(&mut dag, &reg);
        let n0 = dag.nodes.len();
        let e0 = dag.edges.len();
        synthesize_report_data_companion(&mut dag, &reg);
        assert_eq!(
            dag.nodes
                .iter()
                .filter(|n| n.id == "assemble_report_data")
                .count(),
            1,
            "second pass duplicated the assemble_report_data node"
        );
        assert_eq!(dag.nodes.len(), n0, "second pass added nodes (not idempotent)");
        assert_eq!(dag.edges.len(), e0, "second pass added edges (not idempotent)");
    }

    /// Skip rule: a DAG whose nodes declare NO result_schema gets no
    /// `assemble_report_data` node.
    #[test]
    fn skips_when_no_node_declares_result_schema() {
        let reg = atom_registry();
        let mut dag = dag_with(
            vec![plain_node("alignment"), plain_node("final_reporting")],
            vec![simple_edge("alignment", "final_reporting")],
        );
        synthesize_report_data_companion(&mut dag, &reg);
        assert!(
            !dag.nodes.iter().any(|n| n.id == "assemble_report_data"),
            "assemble_report_data must not be synthesized when no stage declares a result_schema"
        );
    }

    /// Only the terminals actually present in the dag get wired —
    /// when only `final_reporting` exists (no intermediate
    /// `reporting`), the assembler still wires into it.
    #[test]
    fn wires_only_terminals_present_in_dag() {
        let reg = atom_registry();
        let de_node = TaskNode::from_atom(reg.get("differential_expression").expect("atom"));
        let mut dag = dag_with(vec![de_node, plain_node("final_reporting")], vec![]);

        synthesize_report_data_companion(&mut dag, &reg);

        assert!(
            dag.edges
                .iter()
                .any(|e| e.from_node == "assemble_report_data" && e.to_node == "final_reporting"),
            "assemble_report_data -> final_reporting edge missing"
        );
        assert!(
            !dag.edges.iter().any(|e| e.to_node == "reporting"),
            "no edge should target a nonexistent 'reporting' node"
        );
    }

    /// Lowering the synthesized dag through the real emitter entry point
    /// proves the `workflow_json.rs` allowlist folds `builtin` +
    /// `report_schemas` into the emitted task's `spec`.
    #[test]
    fn lowered_task_spec_carries_builtin_and_report_schemas() {
        use crate::backend_emitters::workflow_json::{lower_to_workflow_json, EmitContext};

        let reg = atom_registry();
        let de_node = TaskNode::from_atom(reg.get("differential_expression").expect("atom"));
        let mut dag = dag_with(
            vec![de_node, plain_node("reporting"), plain_node("final_reporting")],
            vec![simple_edge("reporting", "final_reporting")],
        );
        synthesize_report_data_companion(&mut dag, &reg);

        let artifact = lower_to_workflow_json(&dag, &EmitContext::defaults())
            .expect("lowering the synthesized dag must succeed");
        let task = artifact
            .dag
            .tasks
            .get("assemble_report_data")
            .expect("assemble_report_data task must be present in the lowered DAG");
        let spec = task
            .spec
            .as_ref()
            .expect("assemble_report_data task must carry a spec");
        assert_eq!(
            spec.get("builtin").and_then(|v| v.as_str()),
            Some("assemble_report_data"),
            "lowered spec must carry 'builtin'; spec={spec:?}"
        );
        let schemas_val = spec
            .get("report_schemas")
            .expect("lowered spec must carry 'report_schemas'");
        let schemas: BTreeMap<String, ResultSchema> =
            serde_json::from_value(schemas_val.clone()).expect("report_schemas deserializes");
        assert!(schemas.contains_key("differential_expression"));
    }
}
