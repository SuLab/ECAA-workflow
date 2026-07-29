//! DE raw|normalized counts one-of: a satisfied one-of `InputGroup`
//! must not mark `required_contract_unsatisfied` as `Reject`, even
//! though the unbound sibling member still carries a weak
//! (`Unproven`) placeholder edge.
//!
//! Exercises `composer_v4::rescore_dag` — the public wrapper around
//! the v4 planner's private `score_dag` — directly against a
//! hand-built `WorkflowDag`. The fixture stands in for the shape a
//! composed DAG would carry once `differential_expression` declares a
//! `counts` one-of group over `raw_counts` / `normalized_counts`
//! (planned separately); this test exercises the scoring exemption in
//! isolation from the search pipeline that would eventually produce
//! such a DAG.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom::InputGroup;
use ecaa_workflow_core::composer_v4::{rescore_dag, PlanningContext, ScoringTuple, ScoringValue};
use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
use ecaa_workflow_core::workflow_contracts::evidence::AssumptionLedger;
use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

const CONSUMER_ID: &str = "differential_expression";
const PRODUCER_ID: &str = "data_acquisition";

/// A `counts` one-of group over `raw_counts` / `normalized_counts`,
/// satisfied by a single bound member (mirrors the planned
/// `differential_expression` atom's method-neutral substrate choice:
/// count-GLM tools want raw, rank-based tools want normalized).
///
/// `InputGroup` is `#[non_exhaustive]`, so an external test crate
/// can't use struct-literal syntax; built via JSON deserialization
/// instead (round-trips through the same `Deserialize` impl the atom
/// YAML loader uses).
fn counts_one_of_group() -> InputGroup {
    serde_json::from_value(serde_json::json!({
        "name": "counts",
        "kind": "one_of",
        "members": ["raw_counts", "normalized_counts"],
        "min_bound": 1,
    }))
    .unwrap()
}

/// DE-like consumer node declaring the one-of group via the
/// `input_groups` attribute (`TaskNode::from_atom`'s preservation
/// convention — see `workflow_contracts::from_atom`).
fn de_consumer_node() -> TaskNode {
    let mut node = TaskNode::skeleton(CONSUMER_ID, "differential expression by condition");
    node.attributes.insert(
        "input_groups".into(),
        serde_json::to_value(vec![counts_one_of_group()]).unwrap(),
    );
    node
}

fn edge(to_port: &str, kind: EdgeKind) -> EdgeContract {
    EdgeContract {
        from_node: PRODUCER_ID.into(),
        from_port: "".into(),
        to_node: CONSUMER_ID.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof::default(),
        kind,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

fn score(dag: &WorkflowDag) -> ScoringTuple {
    rescore_dag(
        dag,
        &PlanningContext::default(),
        &ArchetypeRegistry::default(),
    )
}

/// DE binds `raw_counts` (proven `TypedDataFlow`); `normalized_counts`
/// carries an `Unproven` edge — the weak-match placeholder shape an
/// unbound one-of sibling leaves behind. The group's `min_bound` (1)
/// is already satisfied by `raw_counts` alone.
fn dag_with_bound_members(bound: &[&str]) -> WorkflowDag {
    let mut edges = vec![edge("normalized_counts", EdgeKind::Unproven)];
    if bound.contains(&"raw_counts") {
        edges.push(edge("raw_counts", EdgeKind::TypedDataFlow));
    }
    WorkflowDag {
        id: "test".into(),
        nodes: vec![
            TaskNode::skeleton(PRODUCER_ID, "producer"),
            de_consumer_node(),
        ],
        edges,
        assumptions: AssumptionLedger::default(),
        source_template: None,
    }
}

#[test]
fn satisfied_one_of_does_not_mark_required_contract_unsatisfied() {
    let dag = dag_with_bound_members(&["raw_counts"]);
    let score = score(&dag);
    assert_ne!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "a satisfied one-of group must not Reject the candidate"
    );
}

/// Regression guard: with ZERO members bound, the one-of group is not
/// satisfied — the `Unproven` edge into `normalized_counts` must still
/// Reject exactly as it would have before the exemption existed.
#[test]
fn unsatisfied_one_of_group_still_rejects() {
    let dag = dag_with_bound_members(&[]);
    let score = score(&dag);
    assert_eq!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "zero bound members means the group is unsatisfied; must still Reject"
    );
}

/// An `Unproven` edge into a port that belongs to NO declared group
/// must never be exempted, satisfied sibling elsewhere or not.
#[test]
fn unproven_edge_outside_any_group_still_rejects() {
    let mut dag = dag_with_bound_members(&["raw_counts"]);
    dag.edges
        .push(edge("experimental_design", EdgeKind::Unproven));
    let score = score(&dag);
    assert_eq!(
        score.required_contract_unsatisfied,
        ScoringValue::Reject,
        "an Unproven edge on a non-grouped port must still Reject"
    );
}

/// Archetype-emit assertions for the DE `counts` one-of across the
/// count-GLM archetypes: the raw member wires to a raw producer, the
/// normalized member to `normalisation`, and BOTH bound member edges
/// carry `mutually_exclusive_group = Some("counts")` (so neither is an
/// untagged authoritative count edge). Drives the REAL compose pipeline
/// (`compose_with_modalities_full`) end-to-end, not a hand-built DAG.
mod archetype_emit {
    use std::collections::BTreeMap;
    use std::path::Path;

    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::composer::compose_with_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use ecaa_workflow_core::workflow_contracts::edge::EdgeContract;
    use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
    use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;

    const ATOMS_DIR: &str = "../../config/stage-atoms";
    const ARCHETYPES_DIR: &str = "../../config/archetypes";
    /// The count-matrix OntologyTerm IRI the classifier stamps into
    /// `available_input_stage` for a counts-first (no-raw-reads) intake.
    const COUNTS_IRI: &str = "data:3917";

    fn regs() -> (AtomRegistry, ArchetypeRegistry) {
        (
            AtomRegistry::load_from_dir(Path::new(ATOMS_DIR)).expect("atoms load"),
            ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR)).expect("archetypes load"),
        )
    }

    /// Compose one archetype through the production dispatch. `counts_first`
    /// seeds `available_input_stage = data:3917` so the reads-processing
    /// chain is pruned and the DE raw edge rewires onto `data_acquisition`.
    /// Returns the validated `WorkflowDag` — panics unless the outcome is a
    /// `ValidatedExecutableDag` (i.e. the graph plans with no blocking gap).
    fn emit_validated(arch_id: &str, counts_first: bool) -> WorkflowDag {
        let (atoms, archetypes) = regs();
        let arch = archetypes.get(arch_id).expect("archetype registered");
        let mut modifiers = BTreeMap::new();
        if counts_first {
            modifiers.insert("available_input_stage".into(), COUNTS_IRI.into());
        }
        let goal = GoalSpec {
            edam_data: arch.goal_data.clone(),
            edam_format: arch.goal_format.clone(),
            modifiers,
            source_prose: None,
            confidence: 0.9,
        };
        let mods: Vec<&str> = if !arch.cross_omics_modalities.is_empty() {
            arch.cross_omics_modalities
                .iter()
                .map(String::as_str)
                .collect()
        } else {
            match arch.modality_hint.as_deref() {
                Some(m) => vec![m],
                None => vec!["generic_omics"],
            }
        };
        let out = compose_with_modalities_full(
            &goal,
            &arch.project_class,
            &atoms,
            &archetypes,
            &mods,
            None,
            None,
            None,
        )
        .unwrap_or_else(|e| panic!("{arch_id} (counts_first={counts_first}) must compose: {e:?}"));
        assert!(
            matches!(
                out.compose_outcome,
                Some(ComposeOutcome::ValidatedExecutableDag { .. })
            ),
            "{arch_id} (counts_first={counts_first}) must plan as a ValidatedExecutableDag; \
             got {:?}",
            out.compose_outcome.as_ref().map(std::mem::discriminant)
        );
        out.workflow_dag
            .expect("a validated executable dag carries a WorkflowDag")
    }

    /// The single DE-family stage id (atom_id == differential_expression).
    /// Every count-GLM single-modality archetype has exactly one.
    fn sole_de_stage(dag: &WorkflowDag) -> String {
        let stages: Vec<String> = dag
            .nodes
            .iter()
            .filter(|n| {
                n.attributes
                    .get("atom_id")
                    .and_then(|v| v.as_str())
                    .map(|a| a == "differential_expression")
                    .unwrap_or(false)
            })
            .map(|n| n.id.clone())
            .collect();
        assert_eq!(
            stages.len(),
            1,
            "expected exactly one differential_expression stage, got {stages:?}"
        );
        stages.into_iter().next().unwrap()
    }

    fn count_edge<'a>(dag: &'a WorkflowDag, de: &str, port: &str) -> Option<&'a EdgeContract> {
        dag.edges
            .iter()
            .find(|e| e.to_node == de && e.to_port == port)
    }

    /// AC1 — reads-first bulk: `raw_counts` binds `quantification`, tagged
    /// `counts`; the `normalisation → normalized_counts` edge is also
    /// tagged `counts` (so neither authoritative count edge is untagged).
    #[test]
    fn ac1_reads_first_bulk_binds_raw_to_quantification_tagged() {
        let dag = emit_validated("bulk_rnaseq_de", false);
        let de = sole_de_stage(&dag);

        let raw =
            count_edge(&dag, &de, "raw_counts").expect("reads-first bulk must bind DE.raw_counts");
        assert_eq!(
            raw.from_node, "quantification",
            "reads-first raw_counts must come from quantification"
        );
        assert_eq!(
            raw.mutually_exclusive_group.as_deref(),
            Some("counts"),
            "the raw_counts edge must be tagged mutually_exclusive_group=counts"
        );

        let norm = count_edge(&dag, &de, "normalized_counts")
            .expect("bulk must also bind DE.normalized_counts");
        assert_eq!(norm.from_node, "normalisation");
        assert_eq!(
            norm.mutually_exclusive_group.as_deref(),
            Some("counts"),
            "the normalisation→DE.normalized_counts edge must also be tagged — \
             no untagged authoritative count edge may survive"
        );
    }

    /// AC2 — counts-first bulk: the reads chain is pruned and `raw_counts`
    /// rewires onto `data_acquisition`; the graph still plans (the
    /// `emit_validated` helper asserts ValidatedExecutableDag = no residual
    /// blocking gap). The raw edge stays tagged through the rewire.
    #[test]
    fn ac2_counts_first_bulk_binds_raw_to_data_acquisition_and_plans() {
        let dag = emit_validated("bulk_rnaseq_de", true);
        let de = sole_de_stage(&dag);
        let raw =
            count_edge(&dag, &de, "raw_counts").expect("counts-first bulk must bind DE.raw_counts");
        assert_eq!(
            raw.from_node, "data_acquisition",
            "counts-first raw_counts must rewire onto the data_acquisition anchor"
        );
        assert_eq!(
            raw.mutually_exclusive_group.as_deref(),
            Some("counts"),
            "the rewired raw_counts edge must keep its mutually_exclusive_group tag"
        );
        // The quantification chain is gone — no edge may reference it.
        assert!(
            !dag.edges.iter().any(|e| e.from_node == "quantification"),
            "counts-first must prune the quantification producer"
        );
    }

    /// AC3 — single-cell binds `normalized_counts` (its `raw_counts` member
    /// has no producer on the single-cell path — benign) and plans.
    #[test]
    fn ac3_single_cell_binds_normalized_and_plans() {
        let dag = emit_validated("single_cell_de", false);
        let de = sole_de_stage(&dag);
        let norm = count_edge(&dag, &de, "normalized_counts")
            .expect("single-cell DE must bind normalized_counts");
        assert_eq!(norm.from_node, "normalisation");
        assert_eq!(
            norm.mutually_exclusive_group.as_deref(),
            Some("counts"),
            "single-cell normalized_counts edge must be tagged counts"
        );

        // Regression guard (adversarial finding I2): the single-cell path must
        // NOT assert a non-count stage as a raw-count candidate. An
        // OrderingOnly dep from a non-count stage (`cell_type_annotation`)
        // that fell through `pick_best_port_pair` onto the first input port
        // (`raw_counts`) must be neither a genuine producer nor
        // mutually_exclusive_group-tagged — otherwise the declared graph
        // claims a non-count stage produces raw counts (the exact
        // false-provenance class this branch removes).
        for e in dag
            .edges
            .iter()
            .filter(|e| e.to_node == de && e.to_port == "raw_counts")
        {
            assert!(
                !matches!(
                    e.kind,
                    ecaa_workflow_core::workflow_contracts::edge::EdgeKind::TypedDataFlow
                        | ecaa_workflow_core::workflow_contracts::edge::EdgeKind::AdapterMediated
                ),
                "single-cell DE.raw_counts must have no genuine count producer; got {:?} from {}",
                e.kind,
                e.from_node
            );
            assert_ne!(
                e.mutually_exclusive_group.as_deref(),
                Some("counts"),
                "single-cell DE.raw_counts edge from {} must not be tagged a count candidate",
                e.from_node
            );
        }
    }

    /// Every count-GLM archetype still plans (no regression) on the
    /// reads-first / native intake path.
    #[test]
    fn all_count_glm_archetypes_still_plan() {
        for arch in [
            "bulk_rnaseq_de",
            "methylation_de",
            "spatial_transcriptomics",
            "single_cell_de",
            "proteomics_dda",
            "proteomics_dia",
            "cross_omics_rnaseq_atac",
            "cross_omics_rnaseq_atac_chip",
            "cross_omics_rnaseq_methylation",
            "cross_omics_rnaseq_proteomics",
        ] {
            let _ = emit_validated(arch, false);
        }
    }

    /// methylation reads-first also binds a raw edge from its
    /// `quantification` (extraction) stage, tagged — the count-GLM
    /// generalization beyond bulk.
    #[test]
    fn methylation_reads_first_binds_raw_tagged() {
        let dag = emit_validated("methylation_de", false);
        let de = sole_de_stage(&dag);
        let raw = count_edge(&dag, &de, "raw_counts")
            .expect("methylation reads-first must bind DE.raw_counts");
        assert_eq!(raw.from_node, "quantification");
        assert_eq!(raw.mutually_exclusive_group.as_deref(), Some("counts"));
    }
}
