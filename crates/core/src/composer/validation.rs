//! Six-item formal validation pass.
//!
//! Runs over a `CompositionResult` independently of which composer
//! path produced it (archetype fast-path, backward-chain fallback, or
//! v4 proof-carrying planner). The checks are:
//!
//! 1. Exclusion consistency — no atom's `excludes` set intersects with
//!    the composed atom set.
//! 2. Acyclicity — Kahn topological sort over `depends_on`.
//! 3. Goal reachability — at least one atom's `edam_data`/`edam_format`
//!    or its output ports' semantic+physical type matches the goal.
//! 4. Input satisfiability — every `depends_on` resolves within the
//!    composition or to an intake-supplied input.
//! 5. Attribute resolution — `method_choice.deferred_to` references a
//!    Discovery atom in the composition.
//! 6. Gate well-formedness — `excludes:` entries reference real atoms.
//!
//! 7. Multi-modal `joint_with` constraints (lhs and rhs must share
//!    the same `source_atom` attribute).

use super::{ComposedAtom, CompositionError, CompositionResult};
use crate::atom::AtomRole;
use crate::atom_registry::AtomRegistry;
use crate::compatibility::engine::{DeterministicCompatibilityEngine, PlanningContext};
use crate::composer_v4::prune_unsourced::{
    any_output_satisfies, is_prunable_required_input, transitive_ancestors,
};
use crate::edam::is_subtype_of;
use crate::goal_spec::GoalSpec;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::WorkflowDag;
use std::collections::{BTreeMap, BTreeSet};

/// Six-item formal validation. Runs over a
/// `CompositionResult` independently of which path produced it.
///
/// `workflow_dag` carries the v4 planner's typed `WorkflowDag` when one
/// is available (the proof-carrying path); `None` for legacy
/// archetype/backward-chain compositions and tests. When present, the
/// emit-time backstop [`no_unsourced_required_inputs`] runs as an extra
/// (8th) check: every retained atom's REQUIRED input port must be
/// satisfiable by an upstream-reachable producer.
pub(super) fn validate_composition(
    result: &CompositionResult,
    atom_reg: &AtomRegistry,
    workflow_dag: Option<&WorkflowDag>,
) -> Result<(), CompositionError> {
    let composed_ids: BTreeSet<&str> = result.atoms.iter().map(|c| c.stage_id.as_str()).collect();

    // 1. Exclusion consistency.
    for c in &result.atoms {
        for excl in &c.atom.excludes {
            if composed_ids.contains(excl.as_str()) {
                let archetype_id = result
                    .matched_archetype
                    .clone()
                    .unwrap_or_else(|| "<backward-chain>".to_string());
                return Err(CompositionError::ExclusionConflict {
                    archetype_id,
                    atom_a: c.stage_id.to_string(),
                    atom_b: excl.clone(),
                });
            }
        }
    }

    // 2. Acyclicity.
    if let Some(cycle) = detect_cycle(&result.atoms, &composed_ids) {
        return Err(CompositionError::CycleDetected { cycle });
    }

    // 3. Goal reachability.
    //
    // Wildcard goal: empty edam_data OR the legacy `data:9999`
    // placeholder (defense-in-depth for sessions persisted before
    // the data:9999 placeholder was retired). Treat both as "no
    // constraint" — modality archetype default already produced
    // the right atom set, and the v4 composer must not crash
    // `GoalUnreachable` on a wildcard goal.
    if result.goal.edam_data.is_empty() || result.goal.edam_data == "data:9999" {
        return Ok(());
    }

    // An atom satisfies the goal when EITHER:
    // (a) its legacy top-level `edam_data` / `edam_format` matches
    // the goal — the v2 archetype-path convention where the
    // atom's "primary tag" is its driving data type. Many of
    // today's atoms encode the *input* type at the top level
    // and the *output* format at the top-level format (see
    // `variant_calling.yaml` / `quantification.yaml`), so this
    // branch matches by convention rather than by output port.
    // (b) any of the atom's `outputs[*].semantic_type` IRIs match
    // the goal IRI directly (or via curated subtype edges) AND
    // the matching port's `physical_format.iri` matches the
    // goal format (when set). This is the v4 port-typed
    // convention: the goal is what an SME asks for as output,
    // so the goal-reachability check verifies that some atom's
    // OUTPUT port produces it.
    //
    // Branch (b) handles the chip-seq + atac-seq peak-calling case
    // where the v4-aligned goal `data:1255 / format:3003` (Feature
    // record / BED) doesn't match `peak_calling`'s top-level
    // `edam_data: data:0863` (BAM, the input type). The output port
    // (`data:1255 / format:3003`) is the right thing to match.
    use crate::workflow_contracts::semantic_type::SemanticType;
    let goal_data = result.goal.edam_data.as_str();
    let goal_format = result.goal.edam_format.as_deref();
    let output_port_matches_goal = |c: &ComposedAtom| -> bool {
        c.atom.outputs.iter().any(|p| {
            let data_ok = match &p.semantic_type {
                SemanticType::OntologyTerm { iri, .. } => {
                    iri == goal_data || is_subtype_of(iri, goal_data)
                }
                SemanticType::LocalExtension {
                    proposed_parent_terms,
                    ..
                } => proposed_parent_terms
                    .iter()
                    .any(|parent| parent == goal_data || is_subtype_of(parent, goal_data)),
                SemanticType::Opaque { .. } => false,
                // Union output ports match when any member IRI matches the goal.
                SemanticType::Union { members } => members.iter().any(|m| match m {
                    SemanticType::OntologyTerm { iri, .. } => {
                        iri == goal_data || is_subtype_of(iri, goal_data)
                    }
                    SemanticType::LocalExtension {
                        proposed_parent_terms,
                        ..
                    } => proposed_parent_terms
                        .iter()
                        .any(|parent| parent == goal_data || is_subtype_of(parent, goal_data)),
                    _ => false,
                }),
            };
            let format_ok = match (
                goal_format,
                p.physical_format.as_ref().map(|f| f.iri.as_str()),
            ) {
                (None, _) => true,
                (Some(want), Some(got)) => want == got,
                (Some(_), None) => false,
            };
            data_ok && format_ok
        })
    };
    let any_reaches = result.atoms.iter().any(|c| {
        let data_ok = c
            .atom
            .edam_data
            .as_deref()
            .map(|d| d == goal_data || is_subtype_of(d, goal_data))
            .unwrap_or(false);
        let format_ok = match (goal_format, c.atom.edam_format.as_deref()) {
            (None, _) => true,
            (Some(want), Some(got)) => want == got,
            (Some(_), None) => false,
        };
        (data_ok && format_ok) || output_port_matches_goal(c)
    });
    if !any_reaches {
        return Err(CompositionError::GoalUnreachable {
            goal: format_goal(&result.goal),
        });
    }

    // 4. Input satisfiability.
    for c in &result.atoms {
        for dep in &c.depends_on {
            if composed_ids.contains(dep.as_str()) {
                continue;
            }
            if result.atoms.iter().any(|x| x.atom.id == *dep) {
                continue;
            }
            if atom_reg.get(dep).is_some() {
                return Err(CompositionError::InputUnsatisfied {
                    atom: c.stage_id.to_string(),
                    missing: dep.clone(),
                });
            }
        }
    }

    // 5. Attribute resolution.
    for c in &result.atoms {
        if let Some(mc) = &c.atom.method_choice {
            let target = result
                .atoms
                .iter()
                .find(|x| x.stage_id == mc.deferred_to || x.atom.id == mc.deferred_to);
            let resolved = target
                .map(|t| matches!(t.atom.role, AtomRole::Discovery))
                .unwrap_or(false);
            if !resolved {
                return Err(CompositionError::MethodChoiceUnresolved {
                    atom: c.stage_id.to_string(),
                    deferred_to: mc.deferred_to.clone(),
                });
            }
        }
    }

    // 6. Gate well-formedness.
    for c in &result.atoms {
        for excl in &c.atom.excludes {
            if atom_reg.get(excl).is_none() {
                return Err(CompositionError::MalformedExclusion {
                    atom: c.stage_id.to_string(),
                    excluded: excl.clone(),
                });
            }
        }
    }

    // 7. multi-modal joint-source constraints. For
    // Each atom that declared `joint_with: [{lhs, rhs},...]`, the
    // composed producers of `lhs` and `rhs` must share the same
    // `attributes.source_atom` value. Missing attributes on either
    // side are treated as None — diverging None from a concrete
    // value is a mismatch (the constraint is "joint", which
    // requires both to declare a source).
    for c in &result.atoms {
        for joint in &c.atom.joint_with {
            let lhs_source = result
                .atoms
                .iter()
                .find(|x| x.atom.id == joint.lhs)
                .and_then(|x| x.atom.attributes.get("source_atom"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let rhs_source = result
                .atoms
                .iter()
                .find(|x| x.atom.id == joint.rhs)
                .and_then(|x| x.atom.attributes.get("source_atom"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if lhs_source != rhs_source || lhs_source.is_none() {
                return Err(CompositionError::JointSourceMismatch {
                    atom: c.stage_id.to_string(),
                    lhs: joint.lhs.clone(),
                    rhs: joint.rhs.clone(),
                    lhs_source,
                    rhs_source,
                });
            }
        }
    }

    // 8. Emit-time backstop — no retained atom may carry a REQUIRED input
    // port that no upstream-reachable producer can source. Defense in
    // depth: `composer_v4::prune_unsourced::prune_unsourced_atoms` drops
    // such atoms at rebuild time, but if one ever survives to emit (a
    // composer bug, a manually-spliced node, a future path that skips the
    // prune) this turns a silently-undispatchable DAG into a typed
    // composition failure. Only runs when a typed `WorkflowDag` is
    // available — the legacy `CompositionResult`-only paths carry no port
    // graph to check against.
    if let Some(dag) = workflow_dag {
        no_unsourced_required_inputs(dag)?;
    }

    Ok(())
}

/// Emit-time backstop invariant (B1): every retained atom in `dag` whose
/// REQUIRED input ports cannot be satisfied by an upstream-reachable
/// producer fails composition with
/// [`CompositionError::UnsourcedRequiredInput`].
///
/// This REUSES the exact predicate logic the rebuild-time pruner uses
/// (`is_required` / `any_output_satisfies` / `transitive_ancestors` from
/// `composer_v4::prune_unsourced`) so the backstop and the pruner agree
/// on the definition of "sourced" by construction (DRY). It differs from
/// the pruner only in disposition: the pruner DROPS the offending atom +
/// rewires; the backstop REPORTS the first offender as an error.
///
/// Reachability mirrors the pruner: a node is checked against the OUTPUT
/// ports of every transitive ancestor (every upstream-reachable node).
/// Source nodes (no incoming edges) are never flagged — their inputs are
/// satisfied externally by registered intake data. Deterministic: nodes
/// and required input ports are scanned in stored order over sorted
/// reachability containers, so the first offender reported is stable.
pub(super) fn no_unsourced_required_inputs(dag: &WorkflowDag) -> Result<(), CompositionError> {
    // Incoming edges per node id (consumer -> [producer ids]); BTreeMap
    // for deterministic iteration — mirrors `prune_unsourced_atoms`.
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

    // node id -> &TaskNode for output-port lookup during sourcing.
    let by_id: BTreeMap<&str, &crate::workflow_contracts::task_node::TaskNode> =
        dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    let engine = DeterministicCompatibilityEngine::new();
    let ctx = PlanningContext::default();

    for node in &dag.nodes {
        // Source nodes (no incoming edges) are satisfied externally by
        // registered intake data — never flag them.
        let preds = incoming
            .get(node.id.as_str())
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if preds.is_empty() {
            continue;
        }

        // Output ports of every upstream-reachable ancestor — the same
        // producer set the pruner sources against.
        let ancestors = transitive_ancestors(node.id.as_str(), &incoming);
        let producer_ports: Vec<&PortContract> = ancestors
            .iter()
            .filter_map(|anc| by_id.get(anc))
            .flat_map(|anc| anc.outputs.iter())
            .collect();

        for input in node.inputs.iter().filter(|p| is_prunable_required_input(p)) {
            if !any_output_satisfies(&engine, &ctx, &producer_ports, input) {
                return Err(CompositionError::UnsourcedRequiredInput {
                    atom_id: node.id.clone(),
                    port: input.name.clone(),
                });
            }
        }
    }

    Ok(())
}

/// Kahn's algorithm + cycle reconstruction.
pub(super) fn detect_cycle(
    atoms: &[ComposedAtom],
    composed_ids: &BTreeSet<&str>,
) -> Option<Vec<String>> {
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
    for c in atoms {
        deps.entry(c.stage_id.to_string()).or_default();
        indegree.entry(c.stage_id.to_string()).or_insert(0);
    }
    for c in atoms {
        for d in &c.depends_on {
            let resolved = if composed_ids.contains(d.as_str()) {
                Some(d.clone())
            } else {
                atoms
                    .iter()
                    .find(|x| x.atom.id == *d)
                    .map(|x| x.stage_id.to_string())
            };
            if let Some(stage_id) = resolved {
                deps.get_mut(&stage_id)
                    .unwrap()
                    .push(c.stage_id.to_string());
                *indegree.entry(c.stage_id.to_string()).or_insert(0) += 1;
            }
        }
    }
    let mut queue: Vec<String> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(k, _)| k.clone())
        .collect();
    queue.sort();
    let mut popped: usize = 0;
    while let Some(node) = queue.pop() {
        popped += 1;
        if let Some(out) = deps.get(&node) {
            for next in out {
                let e = indegree.entry(next.clone()).or_insert(0);
                *e = e.saturating_sub(1);
                if *e == 0 {
                    queue.push(next.clone());
                    queue.sort();
                }
            }
        }
    }
    if popped == atoms.len() {
        None
    } else {
        let mut cycle: Vec<String> = indegree
            .iter()
            .filter(|(_, &d)| d > 0)
            .map(|(k, _)| k.clone())
            .collect();
        cycle.sort();
        Some(cycle)
    }
}

fn format_goal(goal: &GoalSpec) -> String {
    match &goal.edam_format {
        Some(f) => format!("{} ({})", goal.edam_data, f),
        None => goal.edam_data.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::ResourceEstimate;

    fn wildcard_result(edam_data: String) -> CompositionResult {
        CompositionResult {
            matched_archetype: Some("test_archetype".into()),
            match_score: 0,
            atoms: Vec::new(),
            goal: GoalSpec {
                edam_data,
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.5,
            },
            rationale: String::new(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: ResourceEstimate::default(),
        }
    }

    #[test]
    fn wildcard_data_9999_does_not_trigger_goal_unreachable() {
        let result = wildcard_result("data:9999".into());
        let atom_reg = AtomRegistry::default();
        let outcome = validate_composition(&result, &atom_reg, None);
        assert!(
            outcome.is_ok(),
            "data:9999 must be wildcard, got {:?}",
            outcome
        );
    }

    #[test]
    fn empty_edam_data_does_not_trigger_goal_unreachable() {
        let result = wildcard_result(String::new());
        let atom_reg = AtomRegistry::default();
        let outcome = validate_composition(&result, &atom_reg, None);
        assert!(
            outcome.is_ok(),
            "empty edam_data must be wildcard, got {:?}",
            outcome
        );
    }

    // ---- B1 backstop: no_unsourced_required_inputs ------------------

    use crate::composer_v4::source_typing::GENE_SET_SEMANTIC_IRI;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
    use crate::workflow_contracts::port::Cardinality;
    use crate::workflow_contracts::semantic_type::SemanticType;
    use crate::workflow_contracts::task_node::TaskNode;
    // `PortContract` and `WorkflowDag` come in via `super::*` (they are
    // imported at module scope for `no_unsourced_required_inputs`).

    /// EDAM IRI for the DE-results that flow `de → pathway` (distinct
    /// from the gene-set IRI so the two pathway inputs are independent).
    const DE_RESULTS_IRI: &str = "data:3753";
    /// Generic upstream artifact the anchor always produces (so DE's own
    /// input is sourceable in these fixtures).
    const COHORT_IRI: &str = "data:2531";

    fn required_input(name: &str, iri: &str) -> PortContract {
        PortContract {
            cardinality: Cardinality::One,
            ..PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
        }
    }
    fn output_port(name: &str, iri: &str) -> PortContract {
        PortContract::with_semantic_type(name, SemanticType::edam(iri, ""))
    }
    fn node_with(id: &str, inputs: Vec<PortContract>, outputs: Vec<PortContract>) -> TaskNode {
        let mut n = TaskNode::skeleton(id, id);
        n.inputs = inputs;
        n.outputs = outputs;
        n
    }
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

    /// A retained atom whose required `gene_set_collection` input has no
    /// upstream producer → the backstop reports `UnsourcedRequiredInput`.
    #[test]
    fn backstop_flags_retained_atom_with_unsourced_required_input() {
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
        // pathway's gene-set input is unsourced (no data:2600 producer).
        let pathway = node_with(
            "pathway_enrichment",
            vec![
                required_input("ranked_de_results", DE_RESULTS_IRI),
                required_input("gene_set_collection", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, pathway],
            edges: vec![typed_edge("data_acquisition", "de"), typed_edge("de", "pathway_enrichment")],
            ..Default::default()
        };

        let err = no_unsourced_required_inputs(&dag)
            .expect_err("an unsourced required gene-set input must fail the backstop");
        match err {
            CompositionError::UnsourcedRequiredInput { atom_id, port } => {
                assert_eq!(atom_id, "pathway_enrichment");
                assert_eq!(port, "gene_set_collection");
            }
            other => panic!("expected UnsourcedRequiredInput, got {other:?}"),
        }
    }

    /// Same topology, but the anchor exposes a gene-set output → every
    /// required input is upstream-sourceable → backstop passes.
    #[test]
    fn backstop_passes_when_all_required_inputs_are_sourced() {
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
            "pathway_enrichment",
            vec![
                required_input("ranked_de_results", DE_RESULTS_IRI),
                required_input("gene_set_collection", GENE_SET_SEMANTIC_IRI),
            ],
            vec![output_port("enrichment", "data:3953")],
        );
        let dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![data_acq, de, pathway],
            edges: vec![typed_edge("data_acquisition", "de"), typed_edge("de", "pathway_enrichment")],
            ..Default::default()
        };

        assert!(
            no_unsourced_required_inputs(&dag).is_ok(),
            "all required inputs are upstream-sourceable; backstop must pass"
        );
    }

    /// End-to-end through `validate_composition`: a goal-reachable
    /// composition whose accompanying `WorkflowDag` carries a retained
    /// atom with an unsourced required input → `validate_composition`
    /// surfaces `UnsourcedRequiredInput`. A clean DAG (same composition)
    /// → `Ok`.
    #[test]
    fn validate_composition_runs_unsourced_backstop_when_dag_present() {
        // `ComposedAtom`, `CompositionResult`, `GoalSpec`, `AtomRegistry`
        // all come in via `super::*`.
        //
        // A composed atom whose top-level `edam_data` matches the goal so
        // the goal-reachability check (3) passes via the legacy branch and
        // execution flows to the backstop (check 8). The atom shape is
        // irrelevant to the backstop — the WorkflowDag below is what it
        // inspects — so source it from the live registry.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/stage-atoms");
        let reg = AtomRegistry::load_from_dir(&dir).expect("live atom registry must load");
        let atom = reg
            .get("differential_expression")
            .expect("differential_expression atom must exist")
            .clone();
        let goal_data = atom.edam_data.clone().unwrap_or_else(|| "data:0951".into());

        let composed = ComposedAtom {
            stage_id: atom.id.clone().into(),
            atom,
            depends_on: Vec::new(),
            required: true,
            bindings: Vec::new(),
            container: None,
        };
        let result = CompositionResult {
            matched_archetype: Some("test_archetype".into()),
            match_score: 0,
            atoms: vec![composed],
            goal: GoalSpec {
                edam_data: goal_data,
                edam_format: None,
                modifiers: BTreeMap::new(),
                source_prose: None,
                confidence: 0.9,
            },
            rationale: String::new(),
            atom_rationales: BTreeMap::new(),
            resource_estimate: crate::composer::ResourceEstimate::default(),
        };

        // Unsourced DAG: pathway's gene-set input has no upstream producer.
        let dirty_dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![
                node_with(
                    "data_acquisition",
                    vec![],
                    vec![output_port("cohort", COHORT_IRI)],
                ),
                node_with(
                    "de",
                    vec![required_input("cohort_in", COHORT_IRI)],
                    vec![output_port("de_results", DE_RESULTS_IRI)],
                ),
                node_with(
                    "pathway_enrichment",
                    vec![
                        required_input("ranked_de_results", DE_RESULTS_IRI),
                        required_input("gene_set_collection", GENE_SET_SEMANTIC_IRI),
                    ],
                    vec![output_port("enrichment", "data:3953")],
                ),
            ],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "pathway_enrichment"),
            ],
            ..Default::default()
        };

        let atom_reg = AtomRegistry::default();
        let err = validate_composition(&result, &atom_reg, Some(&dirty_dag))
            .expect_err("validate_composition must surface the unsourced required input");
        assert!(
            matches!(err, CompositionError::UnsourcedRequiredInput { .. }),
            "expected UnsourcedRequiredInput, got {err:?}"
        );

        // Clean DAG: anchor exposes the gene-set output → backstop passes,
        // and the rest of validate_composition is satisfied → Ok.
        let clean_dag = WorkflowDag {
            id: "test".into(),
            nodes: vec![
                node_with(
                    "data_acquisition",
                    vec![],
                    vec![
                        output_port("cohort", COHORT_IRI),
                        output_port("gene_set", GENE_SET_SEMANTIC_IRI),
                    ],
                ),
                node_with(
                    "de",
                    vec![required_input("cohort_in", COHORT_IRI)],
                    vec![output_port("de_results", DE_RESULTS_IRI)],
                ),
                node_with(
                    "pathway_enrichment",
                    vec![
                        required_input("ranked_de_results", DE_RESULTS_IRI),
                        required_input("gene_set_collection", GENE_SET_SEMANTIC_IRI),
                    ],
                    vec![output_port("enrichment", "data:3953")],
                ),
            ],
            edges: vec![
                typed_edge("data_acquisition", "de"),
                typed_edge("de", "pathway_enrichment"),
            ],
            ..Default::default()
        };
        assert!(
            validate_composition(&result, &atom_reg, Some(&clean_dag)).is_ok(),
            "a clean DAG must pass validate_composition"
        );
    }
}
