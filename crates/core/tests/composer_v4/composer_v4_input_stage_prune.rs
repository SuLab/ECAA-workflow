//! Counts-first input-stage pruning: the rewired producer port must be a port
//! the staging anchor actually declares.
//!
//! `input_stage_prune::prune_supplied_upstream` drops the producing chain for
//! an SME-supplied processed product and rewires the surviving consumers onto
//! `data_acquisition`. It reassigned `from_node` but left `from_port` naming
//! the PRUNED producer's own output port. On the counts-first `bulk_rnaseq`
//! path the anchor already produces `data:3917` (as its OPTIONAL
//! `raw_count_matrix` output), so the pass's "copy the producing port onto the
//! anchor" branch is suppressed — and the rewired edges were left pointing at
//! `quantification.count_matrix` / `qc_preprocessing.filtered_count_matrix`,
//! neither of which `config/stage-atoms/data_acquisition.yaml` declares.
//!
//! Downstream that is not cosmetic: `core::ro_crate` builds its
//! `ecaax:PortAlias` map by copying each declared edge's `from_port` verbatim,
//! so an unresolvable name means NO alias in the emitted crate resolves to the
//! anchor's canonical port and a reviewer cannot join the edge back to a real
//! atom contract.
//!
//! These tests drive the REAL v4 planner over the REAL atom/archetype
//! registries (not a hand-built edge fixture), so a regression in either the
//! prune rewire or the `data_acquisition` port contract fails here even if the
//! module's own unit tests still pass on their synthetic DAGs.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::input_stage_prune::{port_aliases, PortAlias};
use ecaa_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

/// The data-staging anchor every supplied-product rewire targets.
const ANCHOR: &str = "data_acquisition";

/// Producer-port names the composer synthesizes for WORKFLOW-ORDERING edges
/// rather than port-typed data flows. They are deliberately not members of any
/// producer's declared `outputs:`, so the port-resolution this file pins does
/// not apply to them and they are exempt from the "must be declared" sweep.
///
/// Enumerated from their construction sites: the empty string
/// (`discover_companion_synthesis`, `survey_method_landscape_synthesis`,
/// `multi_branch_synthesis`, `coherence_gate`), `report` / `literature` /
/// `report_data` (`report_data_synthesis::ordering_edge` +
/// `interpretation_synthesis::ordering_edge`), `_excluded_rewire`
/// (`prune_unsourced`), and `splice` (`EdgeContract::synthetic_splice`).
const ORDERING_SENTINEL_PORTS: &[&str] = &[
    "",
    "report",
    "literature",
    "report_data",
    "interpretation",
    "_excluded_rewire",
    "splice",
];

fn bulk_rnaseq_de_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: BTreeMap::new(),
        source_prose: Some(
            "Bulk RNA-seq differential expression analysis on an IBD cohort, \
             responder vs non-responder."
                .into(),
        ),
        confidence: 0.9,
    }
}

/// Drive the v4 planner the way `composer_v4_read_allowance.rs` does, but seed
/// `available_data` with a supplied gene count matrix (`data:3917`) — the
/// counts-first intake shape `probe_dataset` surfaces for a GEO Series that
/// ships processed counts and no raw reads. Returns the PRIMARY alternative's
/// `WorkflowDag`, i.e. what the compiler actually emits.
fn run_v4_planner_counts_first(modality: &str, goal: &GoalSpec) -> WorkflowDag {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("input_stage_prune_{modality}"),
        schema_version: semver::Version::new(1, 0, 0),
        goal: goal
            .source_prose
            .clone()
            .unwrap_or_else(|| goal.edam_data.clone()),
        modality: Some(modality.into()),
        project_class: Some("bioinformatics".into()),
        // The load-bearing difference from the raw-reads fixtures: the SME
        // already has counts, so the read-processing chain is pruned and its
        // consumers are rewired onto the staging anchor.
        available_data: vec![DataProductContract::gene_count_matrix()],
        desired_outputs: vec![DesiredOutput {
            label: goal
                .source_prose
                .clone()
                .unwrap_or_else(|| goal.edam_data.clone()),
            edam_data: Some(goal.edam_data.clone()),
            edam_format: goal.edam_format.clone(),
            human_readable: false,
        }],
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.max_branches = 64;
    ctx.max_depth = 12;
    ctx.max_alternatives = 5;

    let result = v4_plan(&ctx, goal, "bioinformatics", &atom_reg, &archetype_reg);
    match result.primary {
        ComposeOutcome::ValidatedExecutableDag { dag, .. }
        | ComposeOutcome::DraftDag { dag, .. } => dag,
        ComposeOutcome::PartialDag { dag, .. } if !dag.nodes.is_empty() => dag,
        other => panic!("[{modality}] v4 planner produced non-DAG outcome: {other:?}"),
    }
}

/// Output-port names the composed `data_acquisition` node declares.
fn anchor_declared_output_ports(dag: &WorkflowDag) -> BTreeSet<String> {
    dag.nodes
        .iter()
        .find(|n| n.id == ANCHOR)
        .unwrap_or_else(|| {
            panic!(
                "{ANCHOR} must survive the prune (it is a SUPPLY_ANCHOR); nodes={:?}",
                dag.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>()
            )
        })
        .outputs
        .iter()
        .map(|o| o.name.clone())
        .collect()
}

/// Every edge the prune rewired onto the anchor must name a port the anchor
/// actually declares. Before the fix a counts-first `bulk_rnaseq` DAG carried
/// three offenders — one `count_matrix` (from the pruned `quantification`) and
/// two `filtered_count_matrix` (from the pruned `qc_preprocessing`) — none of
/// which appear in `data_acquisition`'s `outputs:`.
#[test]
fn prune_rewrites_from_port_to_anchor_canonical_port() {
    let dag = run_v4_planner_counts_first("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let declared = anchor_declared_output_ports(&dag);

    // Sanity: the counts-first path really engaged. `quantification` is the
    // most-upstream `data:3917` producer, so a supplied count matrix must have
    // dropped it (otherwise the assertions below would pass vacuously).
    assert!(
        !dag.nodes.iter().any(|n| n.id == "quantification"),
        "counts-first prune did not engage — `quantification` survived; nodes={:?}",
        dag.nodes.iter().map(|n| n.id.as_str()).collect::<Vec<_>>()
    );

    let offenders: Vec<(String, String, String)> = dag
        .edges
        .iter()
        .filter(|e| e.from_node == ANCHOR)
        .filter(|e| !ORDERING_SENTINEL_PORTS.contains(&e.from_port.as_str()))
        .filter(|e| !declared.contains(&e.from_port))
        .map(|e| (e.from_node.clone(), e.from_port.clone(), e.to_node.clone()))
        .collect();
    assert!(
        offenders.is_empty(),
        "every rewired {ANCHOR} edge must name a declared output port. \
         declared={declared:?}; offending (from_node, from_port, to_node)={offenders:?}"
    );

    // Positive half: at least one edge actually resolved onto the anchor's own
    // counts port. Without the rewrite no `raw_count_matrix` edge exists at
    // all — the archetype binds `raw_count_matrix` to nothing, so this only
    // appears once the prune resolves the pruned producer's port.
    assert!(
        dag.edges
            .iter()
            .any(|e| e.from_node == ANCHOR && e.from_port == "raw_count_matrix"),
        "expected at least one {ANCHOR} edge on the canonical `raw_count_matrix` port; \
         {ANCHOR} edges={:?}",
        dag.edges
            .iter()
            .filter(|e| e.from_node == ANCHOR)
            .map(|e| (e.from_port.as_str(), e.to_node.as_str()))
            .collect::<Vec<_>>()
    );
}

/// The rewrite must not silently erase the pre-rewire name: each renamed edge
/// retains a `PortAlias` recording BOTH the original (pruned-producer) port and
/// the resolved canonical anchor port, so `ecaax:PortAlias` consumers and human
/// reviewers can still join the edge back to the atom contract it came from.
#[test]
fn port_alias_maps_noncanonical_to_canonical() {
    let dag = run_v4_planner_counts_first("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let declared = anchor_declared_output_ports(&dag);
    let surviving: BTreeSet<&str> = dag.nodes.iter().map(|n| n.id.as_str()).collect();

    let aliases: BTreeSet<PortAlias> = dag
        .edges
        .iter()
        .filter(|e| e.from_node == ANCHOR)
        .flat_map(port_aliases)
        .collect();

    assert!(
        !aliases.is_empty(),
        "a counts-first rewire must retain at least one port alias; {ANCHOR} edges={:?}",
        dag.edges
            .iter()
            .filter(|e| e.from_node == ANCHOR)
            .map(|e| (e.from_port.as_str(), e.to_node.as_str()))
            .collect::<Vec<_>>()
    );

    for alias in &aliases {
        assert_ne!(
            alias.original_port, alias.canonical_port,
            "an alias is only recorded for a genuine rename: {alias:?}"
        );
        assert!(
            declared.contains(&alias.canonical_port),
            "the resolved canonical port must be declared by {ANCHOR}: {alias:?}; declared={declared:?}"
        );
        assert!(
            !declared.contains(&alias.original_port),
            "the retained original port is precisely the one {ANCHOR} does NOT declare: {alias:?}"
        );
        assert!(
            !surviving.contains(alias.pruned_producer.as_str()),
            "an alias must name the PRUNED producer, which is gone from the DAG: {alias:?}"
        );
    }

    // The concrete counts-first mapping: `quantification`'s only output port
    // (`count_matrix`, `data:3917`) resolves onto `data_acquisition`'s own
    // `raw_count_matrix`. `bulk_rnaseq_de.yaml` documents this rewire on
    // `differential_expression`'s raw-counts one-of member.
    assert!(
        aliases.contains(&PortAlias {
            pruned_producer: "quantification".into(),
            original_port: "count_matrix".into(),
            canonical_port: "raw_count_matrix".into(),
        }),
        "expected the quantification.count_matrix -> {ANCHOR}.raw_count_matrix alias; got {aliases:?}"
    );
}

/// Positional ports synthesized while typing companion edges must retain a
/// real incoming edge. The counts-first Himes shape previously left
/// `survey_method_landscape.companion_in_3` behind after its edge disappeared;
/// execution then advertised the raw `samples.csv` through that orphan port,
/// and end-of-run provenance reconciliation blocked the completed workflow.
#[test]
fn counts_first_survey_has_no_orphan_synthetic_inputs() {
    let dag = run_v4_planner_counts_first("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let survey = dag
        .nodes
        .iter()
        .find(|node| node.id == "survey_method_landscape")
        .expect("method-landscape survey must be present");

    let orphaned: Vec<&str> = survey
        .inputs
        .iter()
        .filter(|input| {
            input.name.starts_with("companion_in_") || input.name.starts_with("residual_in_")
        })
        .filter(|input| {
            !dag.edges
                .iter()
                .any(|edge| edge.to_node == survey.id && edge.to_port == input.name)
        })
        .map(|input| input.name.as_str())
        .collect();

    assert!(
        orphaned.is_empty(),
        "every synthesized survey input must be backed by a surviving edge; \
         orphaned={orphaned:?}; inputs={:?}; incoming={:?}",
        survey
            .inputs
            .iter()
            .map(|input| input.name.as_str())
            .collect::<Vec<_>>(),
        dag.edges
            .iter()
            .filter(|edge| edge.to_node == survey.id)
            .map(|edge| (
                edge.from_node.as_str(),
                edge.from_port.as_str(),
                edge.to_port.as_str()
            ))
            .collect::<Vec<_>>()
    );
}
