//! First-class multi-branch DAG synthesis for the v4 planner.
//!
//! When a request resolves to >=2 modalities and no registered
//! cross-omics archetype matches, `plan()` delegates here. Each modality
//! is planned through the FULL single-modality planner (a recursive,
//! guarded `plan()` call), its node ids are namespace-prefixed, its
//! per-branch final report is stripped, and the branches are joined at a
//! `multi_modal_thematic_comparison` -> `final_reporting` pair (the
//! existing `reporting`/`final_reporting` atoms reused via alias, exactly
//! as the cross-omics archetypes do). This module holds only pure
//! assembly; the planner-private scoring/classification stays in
//! `plan()`. Determinism: snapshot -> append -> re-sort, identical
//! discipline to discover/survey synthesis.

use std::collections::BTreeSet;

use crate::archetype_registry::ArchetypeRegistry;
use crate::atom_registry::AtomRegistry;
use crate::composer::{ComposedAtom, CompositionResult};
use crate::goal_spec::GoalSpec;
use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use crate::workflow_contracts::lifecycle::LifecycleState;
use crate::workflow_contracts::outcome::{ComposeOutcome, GapReport};
use crate::workflow_contracts::task_node::WorkflowDag;

/// Hard cap on branch count so a pathological modality list can't fan
/// the planner out unboundedly. Truncation is logged, never silent.
const MAX_MODALITY_BRANCHES: usize = 8;

/// Normalize a modality string into a safe, deterministic stage-id
/// prefix ending in `_`. Lowercases, maps non-alphanumerics to `_`,
/// collapses repeats, trims, and dedupes collisions with a numeric
/// suffix (`bulk_rnaseq_`, then `bulk_rnaseq2_`).
fn modality_prefix(modality: &str, used: &mut BTreeSet<String>) -> String {
    let mut base: String = modality
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    base = base.trim_matches('_').to_string();
    if base.is_empty() {
        base = "modality".to_string();
    }
    let mut candidate = format!("{base}_");
    let mut i = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}{i}_");
        i += 1;
    }
    used.insert(candidate.clone());
    candidate
}

/// Namespace-prefix a branch DAG. Renames `id` and `human_name` (for
/// display) and rewrites every intra-branch edge endpoint; **leaves
/// `machine_name` bare** because `discover_companion_synthesis.rs:118`
/// uses `machine_name` as the atom-registry lookup key — and the
/// branch's companions were already synthesized inside the sub-plan, so
/// the whole branch (companions included) is prefixed as a unit.
fn prefix_branch(dag: &mut WorkflowDag, prefix: &str) {
    for n in &mut dag.nodes {
        n.id = format!("{prefix}{}", n.id);
        n.human_name = format!("{prefix}{}", n.human_name);
        // machine_name intentionally NOT prefixed.
    }
    for e in &mut dag.edges {
        e.from_node = format!("{prefix}{}", e.from_node);
        e.to_node = format!("{prefix}{}", e.to_node);
    }
}

/// Drop the branch's project-level final-report node(s) — identified by
/// the bare atom id `final_reporting` in the lift-stamped `atom_id`
/// attribute — and any edge touching them, so branch *analysis*
/// terminals feed the single cross-modality `final_reporting`. Idempotent.
fn strip_branch_final_report(dag: &mut WorkflowDag) {
    let drop: BTreeSet<String> = dag
        .nodes
        .iter()
        .filter(|n| {
            n.attributes.get("atom_id").and_then(|v| v.as_str()) == Some("final_reporting")
        })
        .map(|n| n.id.clone())
        .collect();
    dag.nodes.retain(|n| !drop.contains(&n.id));
    dag.edges
        .retain(|e| !drop.contains(&e.from_node) && !drop.contains(&e.to_node));
}

/// Branch terminals = nodes with no outgoing edge, sorted for
/// determinism. Computed AFTER `strip_branch_final_report` so the
/// pre-final analysis nodes become the feeders into the join.
fn branch_terminals(dag: &WorkflowDag) -> Vec<String> {
    let has_outgoing: BTreeSet<&str> = dag.edges.iter().map(|e| e.from_node.as_str()).collect();
    let mut terms: Vec<String> = dag
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .filter(|id| !has_outgoing.contains(id.as_str()))
        .collect();
    terms.sort();
    terms
}

/// Ordering-edge proof (no port-typed data flow) — same convention as
/// survey/discover synthesis and the cross-omics join.
fn ordering_proof(from: &str, to: &str) -> CompatibilityProof {
    CompatibilityProof {
        producer_type: "ecaax:multi_branch_join_signal".into(),
        consumer_type: "ecaax:multi_branch_join_signal".into(),
        warnings: vec![
            "workflow_ordering_edge: multi-branch join; no port-typed data flow".into(),
        ],
        rationale: Some(format!("multi_branch_synthesis: {from} -> {to}")),
        ..Default::default()
    }
}

/// Build the two-node join sub-DAG by lifting a `CompositionResult`
/// holding the existing `reporting` atom (aliased
/// `multi_modal_thematic_comparison`) + `final_reporting`. Reusing
/// `lift_to_workflow_dag` gives correct ports + the
/// `atom_id`/`stage_id` attributes (so lowering resolves the real
/// atoms — container, safety, figures) + the comparison ->
/// final_reporting edge, for free. Returns `None` if either atom is
/// absent from the registry.
fn build_join_subdag(
    ctx: &crate::composer_v4::PlanningContext,
    goal: &GoalSpec,
    atom_reg: &AtomRegistry,
) -> Option<WorkflowDag> {
    let comparison_atom = atom_reg.get("reporting")?.clone();
    let final_atom = atom_reg.get("final_reporting")?.clone();
    let atoms = vec![
        ComposedAtom {
            stage_id: crate::ids::StageId::from("multi_modal_thematic_comparison"),
            atom: comparison_atom.clone(),
            depends_on: Vec::new(),
            required: true,
            bindings: Vec::new(),
            container: crate::composer::resolve_task_container(&comparison_atom, None, None),
        },
        ComposedAtom {
            stage_id: crate::ids::StageId::from("final_reporting"),
            atom: final_atom.clone(),
            depends_on: vec!["multi_modal_thematic_comparison".to_string()],
            required: true,
            bindings: Vec::new(),
            container: crate::composer::resolve_task_container(&final_atom, None, None),
        },
    ];
    let resource_estimate = crate::composer::aggregate_resources(&atoms);
    let result = CompositionResult {
        matched_archetype: None,
        match_score: 0,
        atoms,
        goal: goal.clone(),
        rationale: "multi-branch cross-modality join".to_string(),
        atom_rationales: Default::default(),
        resource_estimate,
    };
    let mut dag = crate::composer_v4::planner::lift_to_workflow_dag(&result, ctx, goal);
    for n in &mut dag.nodes {
        n.lifecycle_state = LifecycleState::Production;
    }
    Some(dag)
}

/// Merge prefixed branches + the join sub-DAG, wire each branch terminal
/// into `multi_modal_thematic_comparison`, and re-sort for determinism.
/// Idempotent: nodes deduped by id, edges fully deduped.
fn assemble(
    branch_dags: Vec<WorkflowDag>,
    terminals_per_branch: Vec<Vec<String>>,
    ctx: &crate::composer_v4::PlanningContext,
    goal: &GoalSpec,
    atom_reg: &AtomRegistry,
) -> Option<WorkflowDag> {
    let mut nodes: Vec<crate::workflow_contracts::task_node::TaskNode> = Vec::new();
    let mut edges: Vec<EdgeContract> = Vec::new();
    let mut all_terminals: Vec<String> = Vec::new();
    for (dag, terms) in branch_dags.into_iter().zip(terminals_per_branch) {
        all_terminals.extend(terms);
        nodes.extend(dag.nodes);
        edges.extend(dag.edges);
    }

    let join = build_join_subdag(ctx, goal, atom_reg)?;
    nodes.extend(join.nodes);
    edges.extend(join.edges);

    all_terminals.sort();
    all_terminals.dedup();
    for term in &all_terminals {
        edges.push(EdgeContract {
            from_node: term.clone(),
            from_port: String::new(),
            to_node: "multi_modal_thematic_comparison".into(),
            to_port: String::new(),
            proof: ordering_proof(term, "multi_modal_thematic_comparison"),
            chain_of_custody: None,
        });
    }

    nodes.sort_by(|a, b| a.id.cmp(&b.id));
    nodes.dedup_by(|a, b| a.id == b.id);
    edges.sort_by(|a, b| {
        a.from_node
            .cmp(&b.from_node)
            .then_with(|| a.from_port.cmp(&b.from_port))
            .then_with(|| a.to_node.cmp(&b.to_node))
            .then_with(|| a.to_port.cmp(&b.to_port))
    });
    edges.dedup();

    Some(WorkflowDag {
        id: format!("composed:{}", ctx.intent.id),
        nodes,
        edges,
        assumptions: Default::default(),
        source_template: None,
    })
}

/// Phase-1 proposal stub for a modality with no catalog satisfier. A
/// `GapReport` carries the unsatisfiable modality through the
/// `PartialDag` outcome with a `propose_hypothesized_node` suggestion;
/// Phase 2 upgrades this into a full `HypothesizedProposal`.
fn missing_modality_gap(modality: &str) -> GapReport {
    GapReport {
        id: format!("unsatisfiable_modality:{modality}"),
        statement: format!(
            "modality '{modality}' has no catalog satisfier (no archetype matched and \
             search produced no executable branch); surface a hypothesized-node proposal \
             instead of dropping it"
        ),
        missing_port: None,
        suggestions: vec![
            format!("Propose a hypothesized pipeline for '{modality}' via propose_hypothesized_node"),
            "Add an archetype or atoms covering this modality".into(),
        ],
    }
}

/// The assembled multi-branch DAG plus any unsatisfiable-modality gaps.
/// Scoring/classification of `dag` is done by the caller (`plan()`),
/// which holds the planner-private helpers.
pub(crate) struct MultiBranchComposition {
    pub dag: WorkflowDag,
    pub unresolved: Vec<GapReport>,
}

/// Plan each requested modality through the FULL single-modality planner
/// (recursive, guarded `plan()` call), prefix + strip + assemble. A
/// modality whose sub-plan yields no executable DAG becomes a
/// `GapReport` (never a silent drop). The caller guarantees >=2
/// modalities and no cross-omics archetype.
///
/// Branches are iterated in a fixed order (primary first, then
/// `additional_modalities` in stored order), which is load-bearing for
/// `modality_prefix` collision-suffix assignment — keep this collection
/// ordered so a future refactor does not feed an unordered set.
pub(crate) fn compose_branches(
    ctx: &crate::composer_v4::PlanningContext,
    goal: &GoalSpec,
    project_class: &str,
    atom_reg: &AtomRegistry,
    archetype_reg: &ArchetypeRegistry,
) -> MultiBranchComposition {
    let mut modalities: Vec<String> = Vec::new();
    if let Some(m) = ctx.intent.modality.as_deref() {
        modalities.push(m.to_string());
    }
    for m in &ctx.additional_modalities {
        if !modalities.contains(m) {
            modalities.push(m.clone());
        }
    }
    if modalities.len() > MAX_MODALITY_BRANCHES {
        tracing::warn!(
            requested = modalities.len(),
            cap = MAX_MODALITY_BRANCHES,
            "multi_branch: truncating modality list to cap"
        );
        modalities.truncate(MAX_MODALITY_BRANCHES);
    }

    let mut used_prefixes = BTreeSet::new();
    let mut branch_dags: Vec<WorkflowDag> = Vec::new();
    let mut terminals_per_branch: Vec<Vec<String>> = Vec::new();
    let mut unresolved: Vec<GapReport> = Vec::new();

    for modality in &modalities {
        // Single-modality sub-goal + sub-context. Clear the n_way signal
        // so the branch sub-plan can't attempt a cross-omics seed, and
        // set the recursion guard so the top-of-`plan()` dispatch refuses
        // to re-enter.
        let mut sub_goal = goal.clone();
        sub_goal.modifiers.remove("n_way_intent");
        let mut sub = ctx.clone();
        sub.intent.modality = Some(modality.clone());
        sub.additional_modalities = Vec::new();
        sub.in_branch_subplan = true;

        let result = crate::composer_v4::planner::plan(
            &sub,
            &sub_goal,
            project_class,
            atom_reg,
            archetype_reg,
        );

        let branch_dag: Option<WorkflowDag> = match result.primary {
            ComposeOutcome::ValidatedExecutableDag { dag, .. }
            | ComposeOutcome::DraftDag { dag, .. } => Some(dag),
            ComposeOutcome::PartialDag { dag, .. } if !dag.nodes.is_empty() => Some(dag),
            _ => {
                // NovelNodeSpec / Refusal / empty PartialDag → proposal stub.
                unresolved.push(missing_modality_gap(modality));
                None
            }
        };

        if let Some(mut dag) = branch_dag {
            strip_branch_final_report(&mut dag);
            let prefix = modality_prefix(modality, &mut used_prefixes);
            prefix_branch(&mut dag, &prefix);
            let terms = branch_terminals(&dag);
            branch_dags.push(dag);
            terminals_per_branch.push(terms);
        }
    }

    let dag = assemble(branch_dags, terminals_per_branch, ctx, goal, atom_reg).unwrap_or_else(|| {
        WorkflowDag {
            id: format!("composed:{}", ctx.intent.id),
            nodes: Vec::new(),
            edges: Vec::new(),
            assumptions: Default::default(),
            source_template: None,
        }
    });

    MultiBranchComposition { dag, unresolved }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer_v4::planner::planning_context_for_goal_with_modalities;
    use crate::workflow_contracts::task_node::TaskNode;
    use std::path::Path;

    fn registries() -> (AtomRegistry, ArchetypeRegistry) {
        let atoms = AtomRegistry::load_from_dir(Path::new("../../config/stage-atoms")).expect("atoms");
        let archs =
            ArchetypeRegistry::load_from_dir(Path::new("../../config/archetypes")).expect("archetypes");
        (atoms, archs)
    }
    fn de_goal() -> GoalSpec {
        // Differential-expression goal. `data:0951` is the `goal_data`
        // the `bulk_rnaseq_de` PRIMARY archetype declares (so its seed
        // fires definitively) and is registry-producible, so the
        // `proteomics` branch — whose two archetypes (`proteomics_dda` /
        // `proteomics_dia`) tie on `modality_hint` and therefore yield no
        // archetype seed — still composes through the backward-search
        // fallback. Both modalities resolve; the assembled join is clean.
        GoalSpec {
            edam_data: "data:0951".to_string(),
            source_prose: Some("differential expression across groups".to_string()),
            ..Default::default()
        }
    }

    /// A differential-expression goal whose `edam_data` is a well-formed
    /// but registry-unproducible IRI. A modality with a single matching
    /// archetype (e.g. `bulk_rnaseq_de`) still resolves through the
    /// modality-aware archetype seed (`score_archetype_full` scores
    /// `modality_hint` + project class even when `goal_data` differs),
    /// while a modality with NO matching archetype falls through to the
    /// modality-blind backward-search fallback — which finds no producer
    /// for this IRI and yields an empty branch, surfacing the Phase-1
    /// unsatisfiable-modality `GapReport`. With a producible IRI the
    /// modality-blind fallback satisfies even an unknown modality
    /// (root-cause #1, addressed in Phase 3), so the gap path could not
    /// be exercised in Phase 1.
    fn unproducible_de_goal() -> GoalSpec {
        GoalSpec {
            edam_data: "ecaax:multi_branch_unproducible_de_goal".to_string(),
            source_prose: Some("differential expression across groups".to_string()),
            ..Default::default()
        }
    }

    fn node(id: &str, atom_id: &str) -> TaskNode {
        let mut n = TaskNode::skeleton(id, "test node");
        n.machine_name = atom_id.to_string();
        n.attributes
            .insert("atom_id".into(), serde_json::Value::String(atom_id.into()));
        n
    }
    fn edge(from: &str, to: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: String::new(),
            to_node: to.into(),
            to_port: String::new(),
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        }
    }

    #[test]
    fn modality_prefix_is_deterministic_and_dedupes() {
        let mut used = BTreeSet::new();
        assert_eq!(modality_prefix("bulk_rnaseq", &mut used), "bulk_rnaseq_");
        assert_eq!(modality_prefix("chip-seq", &mut used), "chip_seq_");
        assert_eq!(modality_prefix("bulk_rnaseq", &mut used), "bulk_rnaseq2_");
        let mut u2 = BTreeSet::new();
        assert_eq!(modality_prefix("...", &mut u2), "modality_");
    }

    #[test]
    fn prefix_branch_renames_ids_and_edges_but_not_machine_name() {
        let mut dag = WorkflowDag {
            id: "b".into(),
            nodes: vec![
                node("alignment", "alignment"),
                node("differential_expression", "differential_expression"),
            ],
            edges: vec![edge("alignment", "differential_expression")],
            assumptions: Default::default(),
            source_template: None,
        };
        prefix_branch(&mut dag, "bulk_rnaseq_");
        assert!(dag.nodes.iter().all(|n| n.id.starts_with("bulk_rnaseq_")));
        // machine_name stays the bare atom id (discover-companion lookup key).
        assert_eq!(dag.nodes[0].machine_name, "alignment");
        assert_eq!(dag.edges[0].from_node, "bulk_rnaseq_alignment");
        assert_eq!(dag.edges[0].to_node, "bulk_rnaseq_differential_expression");
    }

    #[test]
    fn strip_branch_final_report_removes_final_reporting_and_its_edges() {
        let mut dag = WorkflowDag {
            id: "b".into(),
            nodes: vec![
                node("reporting", "reporting"),
                node("final_reporting", "final_reporting"),
            ],
            edges: vec![edge("reporting", "final_reporting")],
            assumptions: Default::default(),
            source_template: None,
        };
        strip_branch_final_report(&mut dag);
        assert!(dag
            .nodes
            .iter()
            .all(|n| n.attributes.get("atom_id").and_then(|v| v.as_str()) != Some("final_reporting")));
        assert!(dag.edges.is_empty(), "edges touching final_reporting are dropped");
    }

    #[test]
    fn branch_terminals_are_nodes_with_no_outgoing_edge() {
        let dag = WorkflowDag {
            id: "b".into(),
            nodes: vec![node("a", "a"), node("b", "b"), node("c", "c")],
            edges: vec![edge("a", "b"), edge("a", "c")],
            assumptions: Default::default(),
            source_template: None,
        };
        assert_eq!(branch_terminals(&dag), vec!["b".to_string(), "c".to_string()]);
    }

    #[test]
    fn assemble_joins_two_prefixed_branches_at_real_reporting_atoms() {
        let (atoms, _archs) = registries();
        let goal = de_goal();
        let ctx = planning_context_for_goal_with_modalities(
            "test-assemble",
            &goal,
            Some("bulk_rnaseq"),
            &["proteomics"],
            Some("bioinformatics"),
            &[],
        );
        // Two trivial prefixed branches (one terminal each).
        let b1 = WorkflowDag {
            id: "b1".into(),
            nodes: vec![node(
                "bulk_rnaseq_differential_expression",
                "differential_expression",
            )],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let b2 = WorkflowDag {
            id: "b2".into(),
            nodes: vec![node(
                "proteomics_differential_abundance",
                "differential_expression",
            )],
            edges: vec![],
            assumptions: Default::default(),
            source_template: None,
        };
        let terms = vec![
            vec!["bulk_rnaseq_differential_expression".to_string()],
            vec!["proteomics_differential_abundance".to_string()],
        ];
        let dag = assemble(vec![b1, b2], terms, &ctx, &goal, &atoms)
            .expect("reporting + final_reporting atoms must exist in the registry");

        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("multi_modal_thematic_comparison"));
        assert!(ids.contains("final_reporting"));
        // The comparison node resolves to the real `reporting` atom.
        let cmp = dag
            .nodes
            .iter()
            .find(|n| n.id == "multi_modal_thematic_comparison")
            .unwrap();
        assert_eq!(
            cmp.attributes.get("atom_id").and_then(|v| v.as_str()),
            Some("reporting")
        );
        // Each branch terminal feeds the comparison; comparison feeds final.
        assert!(dag.edges.iter().any(|e| e.from_node
            == "bulk_rnaseq_differential_expression"
            && e.to_node == "multi_modal_thematic_comparison"));
        assert!(dag.edges.iter().any(|e| e.from_node
            == "proteomics_differential_abundance"
            && e.to_node == "multi_modal_thematic_comparison"));
        assert!(dag.edges.iter().any(|e| e.from_node
            == "multi_modal_thematic_comparison"
            && e.to_node == "final_reporting"));
        // Determinism: nodes sorted by id.
        let actual: Vec<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut sorted = actual.clone();
        sorted.sort();
        assert_eq!(actual, sorted);
    }

    #[test]
    fn compose_branches_produces_prefixed_branches_and_join_for_known_modalities() {
        let (atoms, archs) = registries();
        let goal = de_goal();
        let ctx = planning_context_for_goal_with_modalities(
            "test-cb",
            &goal,
            Some("bulk_rnaseq"),
            &["proteomics"],
            Some("bioinformatics"),
            &[],
        );
        let comp = compose_branches(&ctx, &goal, "bioinformatics", &atoms, &archs);
        assert!(
            comp.unresolved.is_empty(),
            "both modalities have archetypes: {:?}",
            comp.unresolved
        );
        let ids: BTreeSet<&str> = comp.dag.nodes.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains("multi_modal_thematic_comparison"));
        assert!(ids.contains("final_reporting"));
        // Structural invariant: every node is a join id or carries a known modality prefix.
        for n in &comp.dag.nodes {
            let ok = n.id == "multi_modal_thematic_comparison"
                || n.id == "final_reporting"
                || n.id.starts_with("bulk_rnaseq_")
                || n.id.starts_with("proteomics_");
            assert!(ok, "unexpected/off-modality node id: {}", n.id);
        }
    }

    #[test]
    fn compose_branches_surfaces_a_gap_for_an_unsatisfiable_modality_without_dropping_the_rest() {
        let (atoms, archs) = registries();
        // Unproducible goal IRI so the modality-blind backward-search
        // fallback cannot rescue the unknown modality (see
        // `unproducible_de_goal` for the rationale). `bulk_rnaseq` still
        // resolves via its single matching archetype seed.
        let goal = unproducible_de_goal();
        let ctx = planning_context_for_goal_with_modalities(
            "test-cb-miss",
            &goal,
            Some("bulk_rnaseq"),
            &["totally_unknown_modality"],
            Some("bioinformatics"),
            &[],
        );
        let comp = compose_branches(&ctx, &goal, "bioinformatics", &atoms, &archs);
        // The resolvable branch is still composed...
        assert!(comp.dag.nodes.iter().any(|n| n.id.starts_with("bulk_rnaseq_")));
        // ...and the unsatisfiable modality is surfaced, not dropped.
        assert!(comp
            .unresolved
            .iter()
            .any(|g| g.id == "unsatisfiable_modality:totally_unknown_modality"));
    }

    /// End-to-end through the FULL `plan()` path: `bulk_rnaseq +
    /// single_cell_rnaseq` has no cross-omics archetype, so the dispatch
    /// routes to multi-branch synthesis; both modalities archetype-seed
    /// at the DE goal, so both branches are grounded with zero
    /// off-modality leakage. Proves Pillar A end-to-end for fully-grounded
    /// multi-modality requests. (The proteomics-wandering case — a
    /// modality with no archetype seed — is Pillar B / Phase 3.)
    #[test]
    fn full_plan_multi_branch_two_grounded_rna_modalities() {
        let (atoms, archs) = registries();
        let goal = de_goal();
        let ctx = planning_context_for_goal_with_modalities(
            "test-mb-2rna",
            &goal,
            Some("bulk_rnaseq"),
            &["single_cell_rnaseq"],
            Some("bioinformatics"),
            &[],
        );
        let result =
            crate::composer_v4::planner::plan(&ctx, &goal, "bioinformatics", &atoms, &archs);
        let dag = match &result.primary {
            crate::workflow_contracts::outcome::ComposeOutcome::ValidatedExecutableDag {
                dag,
                ..
            }
            | crate::workflow_contracts::outcome::ComposeOutcome::DraftDag { dag, .. }
            | crate::workflow_contracts::outcome::ComposeOutcome::PartialDag { dag, .. } => dag,
            other => panic!("unexpected outcome: {other:?}"),
        };
        let ids: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();
        // Multi-branch fired (our synthesized join), NOT a cross-omics archetype.
        assert!(
            ids.contains("multi_modal_thematic_comparison"),
            "multi-branch must fire (join node present): {ids:?}"
        );
        assert!(
            !ids.contains("cross_omics_thematic_comparison"),
            "must be multi-branch synthesis, not a cross-omics archetype"
        );
        // Both modality branches present + namespace-prefixed.
        assert!(ids.iter().any(|i| i.starts_with("bulk_rnaseq_")));
        assert!(ids.iter().any(|i| i.starts_with("single_cell_rnaseq_")));
        // Structural invariant: every node is a bare join terminal or
        // carries a requested-modality prefix — zero off-modality leakage.
        for n in &dag.nodes {
            let ok = n.id == "multi_modal_thematic_comparison"
                || n.id == "final_reporting"
                || n.id.starts_with("bulk_rnaseq_")
                || n.id.starts_with("single_cell_rnaseq_");
            assert!(ok, "off-modality node leaked into multi-branch DAG: {}", n.id);
        }
        // No wrong-modality / cross-modality-pollution atoms.
        assert!(
            !ids.iter().any(|i| i.contains("translation_efficiency")
                || i.contains("vdj")
                || i.starts_with("proteomics")),
            "no off-modality atoms expected: {ids:?}"
        );
    }
}
