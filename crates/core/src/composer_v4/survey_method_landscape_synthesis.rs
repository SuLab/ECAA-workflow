//! Synthesize the single `survey_method_landscape` task on a v4
//! [`WorkflowDag`] and wire it as the upstream gate of every
//! `discover_*` companion.
//!
//! This pass MUST run immediately AFTER
//! [`super::discover_companion_synthesis::synthesize_discover_companions`]
//! so the discover companions already exist in `dag.nodes` when we scan
//! for them. When at least one `discover_*` node is present we insert
//! one `survey_method_landscape` node (built from the registry atom so
//! it carries the atom's safety / validators / required_artifacts) and
//! one ordering edge `survey_method_landscape → <each discover_*>`. The
//! survey itself is wired downstream of any data-characterization
//! producer present in the DAG (`survey` consumes the run's
//! intake-facts + QC characterization), so it never floats as a
//! `depends_on=[]` root in `WORKFLOW.json`.
//!
//! # Why edges, not id-prefix sniffing
//!
//! The lowering pass (`backend_emitters/workflow_json.rs`) builds
//! `Task.depends_on` straight off `dag.edges`. A survey node with no
//! `survey → discover` edge would lower into a `WORKFLOW.json` where the
//! discover companions never wait on the method landscape — the agent
//! would rank from a table that may not exist yet. Mirrors
//! `discover_companion_synthesis` exactly.
//!
//! # Determinism
//!
//! Snapshot existing ids, append node + edges in traversal order, then
//! re-sort `dag.nodes` by id and `dag.edges` by
//! `(from_node, from_port, to_node, to_port)` — same discipline as
//! `discover_companion_synthesis` / `meet_in_middle` / `companion_synthesis`.
//! Idempotent: a second pass on a DAG that already carries the survey
//! node + its edges is a no-op.

use std::collections::BTreeSet;

use crate::atom_registry::AtomRegistry;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// Stable id of the synthesized survey task — matches the registry
/// atom id so the lowering pass resolves safety / required_artifacts.
const SURVEY_ID: &str = "survey_method_landscape";

/// Candidate data-characterization producer ids whose output the survey
/// consumes. When present AND not transitively downstream of the survey
/// (see the reachability cycle-guard in
/// [`synthesize_survey_method_landscape`]) we add a
/// `producer → survey_method_landscape` ordering edge so the survey does
/// not lower as a `depends_on=[]` root. Conditional on presence so DAGs
/// without these stages still lower cleanly.
///
/// In practice the survey gates the earliest discover_* stages
/// (sequence_trimming / alignment / quantification), so QC/alignment
/// stages that sit downstream of those are skipped by the reachability
/// guard; the surviving anchor is typically the ingest root
/// (`data_acquisition` / `data_import`).
const DATA_CHARACTERIZATION_PRODUCERS: &[&str] = &[
    "data_acquisition",
    "data_import",
    "qc_preprocessing",
    "raw_qc",
];

/// Insert one `survey_method_landscape` task upstream of every
/// synthesized `discover_*` companion and downstream of any
/// data-characterization producer present. Mutates `dag` in place.
/// Idempotent. Keeps the DAG byte-stable (re-sorts nodes + edges).
pub fn synthesize_survey_method_landscape(dag: &mut WorkflowDag, atom_reg: &AtomRegistry) {
    let discover_ids: Vec<String> = dag
        .nodes
        .iter()
        .filter(|n| n.id.starts_with("discover_"))
        .map(|n| n.id.clone())
        .collect();
    // No discover companions → nothing to gate; leave the DAG untouched.
    if discover_ids.is_empty() {
        return;
    }

    let existing_ids: BTreeSet<String> = dag.nodes.iter().map(|n| n.id.clone()).collect();

    // Build the survey node from the registry atom so it carries the
    // atom's safety / validators / required_artifacts. Fall back to a
    // skeleton if the atom is somehow absent (keeps the gate working
    // even on an overlay registry that omits the atom).
    if !existing_ids.contains(SURVEY_ID) {
        let mut node = match atom_reg.get(SURVEY_ID) {
            Some(atom) => TaskNode::from_atom(atom),
            None => TaskNode::skeleton(SURVEY_ID, "Survey the method landscape from literature."),
        };
        // Stamp `atom_id` so `lower_to_workflow_json` populates
        // `Task.source_atom_id` for per-task image + safety enforcement
        // (mirrors discover_companion_synthesis). `from_atom` does NOT
        // set this key.
        node.attributes.insert(
            "atom_id".into(),
            serde_json::Value::String(SURVEY_ID.into()),
        );
        dag.nodes.push(node);
    }

    // Edge: survey → each discover_* (survey gates the discover step).
    let mut survey_edges: Vec<EdgeContract> = Vec::new();
    for d in &discover_ids {
        let already = dag
            .edges
            .iter()
            .any(|e| e.from_node == SURVEY_ID && e.to_node == *d);
        if already {
            continue;
        }
        survey_edges.push(EdgeContract {
            from_node: SURVEY_ID.into(),
            from_port: String::new(),
            to_node: d.clone(),
            to_port: String::new(),
            proof: ordering_proof(SURVEY_ID, d),
            chain_of_custody: None,
        });
    }
    dag.edges.extend(survey_edges);

    // Edge: each present data-characterization producer → survey, so
    // the survey waits on the run's characterized inputs and never
    // floats as a depends_on=[] root in WORKFLOW.json.
    //
    // Cycle guard: the survey gates the earliest discover_* stages
    // (sequence_trimming / alignment / quantification), so any node that
    // is transitively DOWNSTREAM of the survey (reachable via the
    // survey → discover_X → X → … edges just added) must NOT feed the
    // survey — that would close a cycle. Compute the set reachable from
    // the survey over the current edge set and skip any producer in it.
    let downstream_of_survey = reachable_from(&dag.edges, SURVEY_ID);
    let mut producer_edges: Vec<EdgeContract> = Vec::new();
    for producer in DATA_CHARACTERIZATION_PRODUCERS {
        if !existing_ids.contains(*producer) {
            continue;
        }
        if downstream_of_survey.contains(*producer) {
            // Survey-gated (directly or transitively); wiring it would
            // create a cycle. Leave it unconnected to the survey.
            continue;
        }
        let already = dag
            .edges
            .iter()
            .any(|e| e.from_node == *producer && e.to_node == SURVEY_ID);
        if already {
            continue;
        }
        producer_edges.push(EdgeContract {
            from_node: (*producer).into(),
            from_port: String::new(),
            to_node: SURVEY_ID.into(),
            to_port: String::new(),
            proof: ordering_proof(producer, SURVEY_ID),
            chain_of_custody: None,
        });
    }
    dag.edges.extend(producer_edges);

    // Re-sort to keep WorkflowDag byte-stable — same sort keys as
    // discover_companion_synthesis.
    dag.nodes.sort_by(|a, b| a.id.cmp(&b.id));
    dag.edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
}

/// Set of node ids reachable from `start` by following edge direction
/// (`from_node → to_node`) over `edges`. `start` itself is not
/// included. Deterministic: the result is a `BTreeSet`, and the
/// traversal order does not affect membership.
fn reachable_from(edges: &[EdgeContract], start: &str) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = vec![start.to_string()];
    while let Some(cur) = stack.pop() {
        for e in edges.iter().filter(|e| e.from_node == cur) {
            if seen.insert(e.to_node.clone()) {
                stack.push(e.to_node.clone());
            }
        }
    }
    seen
}

/// Build a `workflow_ordering_edge` proof for a survey ordering edge.
/// A method-landscape gate is not a port-typed data flow, but
/// `score_dag` rejects edges whose `proof.producer_type` is empty, so
/// we set a stable sentinel on both ends and attach the ordering
/// warning — identical convention to discover_companion_synthesis.
fn ordering_proof(from: &str, to: &str) -> CompatibilityProof {
    CompatibilityProof {
        producer_type: "ecaax:method_discovery_signal".into(),
        consumer_type: "ecaax:method_discovery_signal".into(),
        warnings: vec![
            "workflow_ordering_edge: survey_method_landscape gates the method-discovery signal, \
             no port-typed data flow"
                .into(),
        ],
        rationale: Some(format!("survey_method_landscape synthesis: {from} -> {to}")),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::evidence::AssumptionLedger;
    use std::path::Path;

    fn real_registry() -> AtomRegistry {
        AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms"))
            .expect("load stage-atoms registry")
    }

    fn dag_with(nodes: Vec<TaskNode>) -> WorkflowDag {
        WorkflowDag {
            id: "test".into(),
            nodes,
            edges: Vec::new(),
            assumptions: AssumptionLedger::default(),
            source_template: None,
        }
    }

    #[test]
    fn inserts_one_survey_upstream_of_every_discover() {
        let reg = real_registry();
        let mut dag = dag_with(vec![
            TaskNode::skeleton("alignment", "t"),
            TaskNode::skeleton("differential_expression", "t"),
        ]);
        crate::composer_v4::discover_companion_synthesis::synthesize_discover_companions(
            &mut dag, &reg,
        );
        synthesize_survey_method_landscape(&mut dag, &reg);

        let surveys: Vec<&TaskNode> = dag
            .nodes
            .iter()
            .filter(|n| n.id == "survey_method_landscape")
            .collect();
        assert_eq!(surveys.len(), 1, "exactly one survey task");

        // Every discover_* depends on the survey: edge survey -> discover_*.
        for n in dag.nodes.iter().filter(|n| n.id.starts_with("discover_")) {
            assert!(
                dag.edges
                    .iter()
                    .any(|e| e.from_node == "survey_method_landscape" && e.to_node == n.id),
                "discover node {} must depend on survey_method_landscape; edges={:?}",
                n.id,
                dag.edges
            );
        }

        // The survey carries the registry atom's network safety + its
        // locator-anchored validators (built via from_atom).
        let survey = surveys[0];
        assert_eq!(survey.safety.level, crate::atom::SafetyLevel::Network);
        assert!(
            survey.validators.iter().any(|v| v.id == "source_resolves"),
            "survey must carry source_resolves; got {:?}",
            survey.validators
        );
        assert_eq!(
            survey.attributes.get("atom_id"),
            Some(&serde_json::Value::String("survey_method_landscape".into())),
            "survey must stamp atom_id for source_atom_id lowering"
        );

        // dag.nodes sorted by id, dag.edges sorted by tuple.
        let node_ids: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = node_ids.clone();
        sorted.sort();
        assert_eq!(node_ids, sorted, "dag.nodes must be sorted by id");

        // Idempotent.
        let before = (dag.nodes.len(), dag.edges.len());
        synthesize_survey_method_landscape(&mut dag, &reg);
        assert_eq!(
            (dag.nodes.len(), dag.edges.len()),
            before,
            "second pass is a no-op"
        );
    }

    #[test]
    fn no_discover_companions_means_no_survey() {
        let reg = real_registry();
        // `data_acquisition` has no method-discovery signal, so no
        // discover companion is synthesized → no survey either.
        let mut dag = dag_with(vec![TaskNode::skeleton("data_acquisition", "t")]);
        crate::composer_v4::discover_companion_synthesis::synthesize_discover_companions(
            &mut dag, &reg,
        );
        synthesize_survey_method_landscape(&mut dag, &reg);
        assert!(
            !dag.nodes.iter().any(|n| n.id == "survey_method_landscape"),
            "no discover companions → no survey task"
        );
        assert!(
            dag.edges.is_empty(),
            "no survey synthesized but edges were emitted: {:?}",
            dag.edges
        );
    }

    /// Load-bearing lower-through assertion: after survey synthesis the
    /// lowering pass (`build_dag_from_workflow_dag` →
    /// `lower_to_workflow_json`) must produce a `DAG` where the survey
    /// task lowers AND every `discover_*` task's `depends_on` includes
    /// `survey_method_landscape`. The lowering pass builds
    /// `Task.depends_on` straight off `dag.edges`, so without the
    /// `survey → discover_*` edges the discover companions would never
    /// wait on the method-landscape table.
    #[test]
    fn lowered_discover_depends_on_survey() {
        let reg = real_registry();
        let mut dag = dag_with(vec![TaskNode::skeleton("alignment", "t")]);
        crate::composer_v4::discover_companion_synthesis::synthesize_discover_companions(
            &mut dag, &reg,
        );
        synthesize_survey_method_landscape(&mut dag, &reg);

        let lowered =
            crate::builder::build_dag_from_workflow_dag(&dag, "wf").expect("lower v4 dag");

        // The survey task itself must lower into WORKFLOW.json.
        assert!(
            lowered.tasks.contains_key("survey_method_landscape"),
            "survey_method_landscape must lower into the DAG; tasks={:?}",
            lowered.tasks.keys().collect::<Vec<_>>()
        );

        // discover_alignment.depends_on must include survey_method_landscape.
        let disc = lowered
            .tasks
            .get("discover_alignment")
            .expect("discover_alignment task must lower");
        assert!(
            disc.depends_on
                .iter()
                .any(|d| d == "survey_method_landscape"),
            "discover_alignment.depends_on must include survey_method_landscape, got {:?}",
            disc.depends_on
        );

        // The survey carries the source-atom back-reference so the
        // harness recovers its registry atom for per-task image + safety.
        let survey = lowered
            .tasks
            .get("survey_method_landscape")
            .expect("survey task");
        assert_eq!(
            survey.source_atom_id.as_deref(),
            Some("survey_method_landscape"),
            "survey task must carry source_atom_id for image + safety enforcement"
        );
    }

    #[test]
    fn survey_depends_on_present_data_characterization_producer() {
        let reg = real_registry();
        // data_acquisition is a data-characterization producer; when
        // present, an edge data_acquisition -> survey is added.
        let mut dag = dag_with(vec![
            TaskNode::skeleton("data_acquisition", "t"),
            TaskNode::skeleton("alignment", "t"),
        ]);
        crate::composer_v4::discover_companion_synthesis::synthesize_discover_companions(
            &mut dag, &reg,
        );
        synthesize_survey_method_landscape(&mut dag, &reg);
        assert!(
            dag.edges.iter().any(|e| e.from_node == "data_acquisition"
                && e.to_node == "survey_method_landscape"),
            "expected data_acquisition -> survey edge; got {:?}",
            dag.edges
        );
    }
}
