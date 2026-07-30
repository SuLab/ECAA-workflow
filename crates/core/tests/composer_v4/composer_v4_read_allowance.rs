//! RCA I-1 — systemic undeclared cross-stage reads (Task 13).
//!
//! Verified against a real `bulk_rnaseq` execution
//! (`docs/rca-de-raw-vs-normalized-counts-provenance.md` §10 I-1):
//! `normalisation` reads `data_acquisition`'s sample metadata for its
//! design formula, and `final_reporting` (+ its synthesized
//! `validate_final_reporting` companion) reads `differential_expression`
//! / `pathway_enrichment` / `contextualize_findings_with_literature`
//! results directly, beyond each atom's single declared producer edge.
//! Observed-provenance reconciliation (`core::provenance::reconcile` +
//! `core::ro_crate::reconcile_ro_crate_edges_with_allowances`) would
//! flag every one of these as `Divergent` without this task's fix:
//!
//! - `normalisation` gets a real, typed `sample_metadata` input port
//!   (matching `data_acquisition.cohort_manifest`'s own type) so the
//!   composed DAG carries a `data_acquisition -> normalisation` edge
//!   reconciliation can match the read against.
//! - `final_reporting` declares a `read_allowance` facet (its real read
//!   set is DAG-shape/archetype dependent, not a fixed producer set —
//!   not statically enumerable as ports). `validate_final_reporting`
//!   is a synthesized companion (no authored atom) that inherits the
//!   same allowance from its target via `companion_synthesis`.
//!
//! This file exercises the fix end-to-end against the REAL composed
//! `bulk_rnaseq` DAG (not a hand-built edge fixture), so a regression
//! in either the archetype wiring or the read-allowance propagation
//! fails here even if the lower-level `ro_crate.rs` unit tests still
//! pass on their own hand-built fixtures.

use std::collections::BTreeMap;
use std::path::Path;

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom::ReadAllowance;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::{plan as v4_plan, PlanningContext};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::provenance::ObservedRead;
use ecaa_workflow_core::ro_crate::{
    parameter_connection_entity, reconcile_ro_crate_edges_with_allowances,
};
use ecaa_workflow_core::workflow_contracts::data_product::DataProductContract;
use ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome;
use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;
use ecaa_workflow_core::workflow_contracts::workflow_intent::{DesiredOutput, WorkflowIntent};
use serde_json::{json, Value};

const ATOMS_DIR: &str = "../../config/stage-atoms";
const ARCHETYPES_DIR: &str = "../../config/archetypes";

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

/// Drive the v4 planner exactly like `composer_v4_validate_companions.rs`
/// and return the PRIMARY alternative's `WorkflowDag` — the one the
/// real compiler actually emits for a canonical bulk-RNA-seq intent
/// (the archetype seed wins primary over search on a definitive match).
fn run_v4_planner(modality: &str, goal: &GoalSpec) -> WorkflowDag {
    run_v4_planner_with_data(
        modality,
        goal,
        vec![DataProductContract::sample_paired_fastq()],
    )
}

fn run_v4_planner_with_data(
    modality: &str,
    goal: &GoalSpec,
    available_data: Vec<DataProductContract>,
) -> WorkflowDag {
    let atom_reg = AtomRegistry::load_from_dir(Path::new(ATOMS_DIR))
        .expect("AtomRegistry must load from config/stage-atoms");
    let archetype_reg = ArchetypeRegistry::load_from_dir(Path::new(ARCHETYPES_DIR))
        .expect("ArchetypeRegistry must load from config/archetypes");
    let intent = WorkflowIntent {
        id: format!("read_allowance_{modality}"),
        schema_version: semver::Version::new(1, 0, 0),
        goal: goal
            .source_prose
            .clone()
            .unwrap_or_else(|| goal.edam_data.clone()),
        modality: Some(modality.into()),
        project_class: Some("bioinformatics".into()),
        available_data,
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

/// `normalisation` must carry a typed edge from `data_acquisition`
/// into its new `sample_metadata` port (not just from
/// `qc_preprocessing` into `count_matrix`) — the composer-wiring half
/// of the fix. Pins the exact `to_port` too: the archetype-seed port
/// picker tries a consumer's declared inputs in order and would
/// otherwise let `count_matrix` shadow `sample_metadata` against
/// `data_acquisition`'s own `raw_count_matrix` output (see
/// `normalisation.yaml`'s port-ordering comment) — a regression there
/// would silently rewire this edge back onto `count_matrix`.
#[test]
fn bulk_rnaseq_normalisation_gets_a_typed_data_acquisition_metadata_edge() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());

    let metadata_edge = dag
        .edges
        .iter()
        .find(|e| e.from_node == "data_acquisition" && e.to_node == "normalisation");
    let metadata_edge = metadata_edge.unwrap_or_else(|| {
        panic!(
            "expected a data_acquisition -> normalisation edge; got {:?}",
            dag.edges
                .iter()
                .filter(|e| e.to_node == "normalisation")
                .map(|e| (
                    e.from_node.as_str(),
                    e.from_port.as_str(),
                    e.to_port.as_str()
                ))
                .collect::<Vec<_>>()
        )
    });
    assert_eq!(
        metadata_edge.to_port, "sample_metadata",
        "data_acquisition -> normalisation edge bound to the wrong port \
         (count_matrix shadowed sample_metadata): {metadata_edge:?}"
    );

    let count_edge = dag
        .edges
        .iter()
        .find(|e| e.from_node == "qc_preprocessing" && e.to_node == "normalisation")
        .expect("qc_preprocessing -> normalisation edge must survive unchanged");
    assert_eq!(
        count_edge.from_port, "filtered_count_matrix",
        "normalisation must consume the filtered matrix, not QC metrics: {count_edge:?}"
    );
    assert_eq!(count_edge.to_port, "count_matrix");
    assert_eq!(count_edge.proof.producer_type, "data:3917");
    assert_eq!(count_edge.proof.consumer_type, "data:3917");
}

#[test]
fn himes_counts_first_edges_use_authored_ports_and_reconcile_realistic_reads() {
    let dag = run_v4_planner_with_data(
        "bulk_rnaseq",
        &bulk_rnaseq_de_goal(),
        vec![DataProductContract::gene_count_matrix()],
    );

    for (from, to, expected_port) in [
        (
            "differential_expression",
            "pathway_enrichment",
            "ranked_de_results",
        ),
        (
            "differential_expression",
            "contextualize_findings_with_literature",
            "analysis_findings",
        ),
    ] {
        let edge = dag
            .edges
            .iter()
            .find(|edge| edge.from_node == from && edge.to_node == to)
            .unwrap_or_else(|| panic!("missing {from} -> {to} edge"));
        assert_eq!(
            edge.to_port, expected_port,
            "{from} -> {to} must bind the authored semantic port: {edge:?}"
        );
        assert!(
            !edge.to_port.starts_with("residual_in_"),
            "{from} -> {to} must not depend on a synthetic residual port"
        );
    }

    let survey_edges: Vec<_> = dag
        .edges
        .iter()
        .filter(|edge| {
            edge.from_node == "data_acquisition" && edge.to_node == "survey_method_landscape"
        })
        .collect();
    for (from_port, to_port) in [
        ("cohort_manifest", "cohort_manifest"),
        ("raw_count_matrix", "count_matrix"),
    ] {
        assert!(
            survey_edges.iter().any(|edge| {
                edge.from_port == from_port
                    && edge.to_port == to_port
                    && matches!(
                        edge.kind,
                        ecaa_workflow_core::workflow_contracts::edge::EdgeKind::TypedDataFlow
                            | ecaa_workflow_core::workflow_contracts::edge::EdgeKind::AdapterMediated
                    )
            }),
            "data acquisition must bind {from_port} -> {to_port}: {survey_edges:?}"
        );
    }

    let observed_reads = vec![
        ObservedRead {
            task_id: "survey_method_landscape".into(),
            declared_port: Some("cohort_manifest".into()),
            path: "runtime/outputs/data_acquisition/data/himes-inputs/samples.csv".into(),
        },
        ObservedRead {
            task_id: "survey_method_landscape".into(),
            declared_port: Some("count_matrix".into()),
            path: "runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv".into(),
        },
        ObservedRead {
            task_id: "contextualize_findings_with_literature".into(),
            declared_port: Some("analysis_findings".into()),
            path: "runtime/outputs/differential_expression/de_results.tsv".into(),
        },
        ObservedRead {
            task_id: "pathway_enrichment".into(),
            declared_port: Some("ranked_de_results".into()),
            path: "runtime/outputs/differential_expression/de_results.tsv".into(),
        },
    ];
    let mut metadata = graph_with_parameter_connections(&dag.edges);
    let divergences = reconcile_ro_crate_edges_with_allowances(
        &mut metadata,
        &dag.edges,
        &observed_reads,
        &node_read_allowances(&dag),
    );
    assert!(
        divergences.is_empty(),
        "authored-port reads must reconcile without allowances: {divergences:#?}"
    );
    let root = metadata["@graph"]
        .as_array()
        .and_then(|graph| graph.iter().find(|entry| entry["@id"] == "./"))
        .expect("root Dataset node present");
    assert!(
        root.get("ecaax:provenanceDivergence").is_none(),
        "realistic Himes reads must not create provenance divergence: {root:#?}"
    );
}

/// `final_reporting` declares a `read_allowance` facet (its own atom
/// YAML) and the synthesized `validate_final_reporting` companion
/// inherits it (`companion_synthesis::synthesize_validate_companions`).
/// Without inheritance the validator — which independently
/// cross-checks the same upstream numbers `final_reporting` restates —
/// would flag the identical reads `final_reporting` itself is
/// sanctioned for.
#[test]
fn final_reporting_and_its_synthesized_validator_both_carry_a_read_allowance() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());

    for id in ["final_reporting", "validate_final_reporting"] {
        let node = dag
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("{id} node missing from composed bulk_rnaseq DAG"));
        let allowance = node
            .attributes
            .get("read_allowance")
            .unwrap_or_else(|| panic!("{id} node has no read_allowance attribute"));
        let parsed: Vec<ReadAllowance> = serde_json::from_value(allowance.clone())
            .unwrap_or_else(|e| panic!("{id} read_allowance didn't deserialize: {e}"));
        assert!(!parsed.is_empty(), "{id} read_allowance array is empty");
        assert!(
            !parsed[0].rationale.is_empty(),
            "{id} read_allowance rationale is empty"
        );
    }
}

/// `reporting` — the intermediate narrative aggregator, sibling of
/// `final_reporting` — must ALSO declare a `read_allowance`, and its
/// synthesized `validate_reporting` companion must inherit it. Confirmed
/// against a real bulk_rnaseq execution: the reporting agent reads
/// `normalisation/result.json` directly (for the `qc_preprocessing`
/// section — normalisation declares no `result_schema`, so its numbers are
/// absent from the aggregated `report-data.json`). Without the allowance,
/// that read is recorded as a genuine `provenance_divergence` and blocks the
/// `reporting` task. `final_reporting` carried the allowance but `reporting`
/// did not — this guards that asymmetry from regressing.
#[test]
fn reporting_and_its_synthesized_validator_both_carry_a_read_allowance() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());

    for id in ["reporting", "validate_reporting"] {
        let node = dag
            .nodes
            .iter()
            .find(|n| n.id == id)
            .unwrap_or_else(|| panic!("{id} node missing from composed bulk_rnaseq DAG"));
        let allowance = node
            .attributes
            .get("read_allowance")
            .unwrap_or_else(|| panic!("{id} node has no read_allowance attribute"));
        let parsed: Vec<ReadAllowance> = serde_json::from_value(allowance.clone())
            .unwrap_or_else(|e| panic!("{id} read_allowance didn't deserialize: {e}"));
        assert!(!parsed.is_empty(), "{id} read_allowance array is empty");
        assert!(
            !parsed[0].rationale.is_empty(),
            "{id} read_allowance rationale is empty"
        );
    }
}

/// Regression (§G-B2 deposit gate): EVERY synthesized validator in a real
/// composed DAG must carry a read-allowance — not only those whose validated
/// stage happens to declare one. A validator without an allowance flags its
/// intrinsic cross-stage re-reads as GENUINE observed-read divergences, which
/// flips `deposit_ready` to false. In the himes run this blocked the deposit
/// via `validate_normalisation` / `validate_reporting` /
/// `validate_pathway_enrichment` / `validate_contextualize_findings_with_literature`,
/// whose validated stages declare no allowance to inherit.
#[test]
fn every_synthesized_validator_carries_an_upstream_read_allowance() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let allowances = node_read_allowances(&dag);
    let validators: Vec<String> = dag
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .filter(|id| id.starts_with("validate_"))
        .collect();
    assert!(
        !validators.is_empty(),
        "composed bulk_rnaseq DAG carries no validate_* companions to check"
    );
    for v in &validators {
        let a = allowances.get(v).unwrap_or_else(|| {
            panic!("validator {v} has no read_allowance — its cross-stage re-reads would false-divergence and block the deposit")
        });
        assert!(
            a.iter()
                .any(|x| x.scope == ecaa_workflow_core::atom::ReadAllowanceScope::AnyUpstreamStage),
            "validator {v} allowance must include AnyUpstreamStage: {a:?}"
        );
    }
}

fn node_read_allowances(dag: &WorkflowDag) -> BTreeMap<String, Vec<ReadAllowance>> {
    let mut map = BTreeMap::new();
    for node in &dag.nodes {
        if let Some(v) = node.attributes.get("read_allowance") {
            if let Ok(parsed) = serde_json::from_value::<Vec<ReadAllowance>>(v.clone()) {
                map.insert(node.id.clone(), parsed);
            }
        }
    }
    map
}

/// Minimal RO-Crate-shaped graph carrying a root Dataset plus one
/// `ParameterConnection` node per declared edge — enough for
/// `reconcile_ro_crate_edges_with_allowances` to stamp against, mirroring
/// `crate::ro_crate`'s own `graph_with_parameter_connections` test helper.
fn graph_with_parameter_connections(
    edges: &[ecaa_workflow_core::workflow_contracts::edge::EdgeContract],
) -> Value {
    let mut graph: Vec<Value> = vec![json!({"@id": "./", "@type": "Dataset", "hasPart": []})];
    for e in edges {
        graph.push(parameter_connection_entity(
            &format!("{}__to__{}", e.from_node, e.to_node),
            &format!("#step-{}", e.from_node),
            &e.from_port,
            &format!("#step-{}", e.to_node),
            &e.to_port,
        ));
    }
    json!({"@graph": graph})
}

/// The end-to-end acceptance test for Task 13 / RCA I-1: reconciling a
/// REAL composed `bulk_rnaseq` DAG's declared edges + per-node
/// read-allowances against the exact reads a real bulk_rnaseq
/// execution was verified to perform (RCA I-1 background doc, §10)
/// must surface ZERO unresolved `Divergent` verdicts for
/// `normalisation`, `final_reporting`, or `validate_final_reporting`.
#[test]
fn bulk_rnaseq_reconciliation_has_no_unresolved_divergence() {
    let dag = run_v4_planner("bulk_rnaseq", &bulk_rnaseq_de_goal());
    let declared_edges = dag.edges.clone();
    let read_allowances = node_read_allowances(&dag);

    let mut metadata = graph_with_parameter_connections(&declared_edges);

    let observed_reads = vec![
        // normalisation: the count matrix from qc_preprocessing (its
        // long-declared port) AND the sample metadata from
        // data_acquisition (the new port this task adds).
        ObservedRead {
            task_id: "normalisation".into(),
            declared_port: Some("count_matrix".into()),
            path: "runtime/outputs/qc_preprocessing/intermediates/filtered_counts.tsv".into(),
        },
        ObservedRead {
            task_id: "normalisation".into(),
            declared_port: Some("sample_metadata".into()),
            path: "runtime/outputs/data_acquisition/data/himes-inputs/samples.csv".into(),
        },
        // final_reporting: its declared reporting-bundle read, plus
        // the three verified direct upstream reads (RCA I-1) — none
        // of these three has a declared edge onto final_reporting, so
        // absent the read_allowance facet each would be Divergent.
        ObservedRead {
            task_id: "final_reporting".into(),
            declared_port: Some("analysis_bundle".into()),
            path: "runtime/outputs/reporting/report.md".into(),
        },
        ObservedRead {
            task_id: "final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/differential_expression/result.json".into(),
        },
        ObservedRead {
            task_id: "final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/pathway_enrichment/result.json".into(),
        },
        ObservedRead {
            task_id: "final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/contextualize_findings_with_literature/result.json".into(),
        },
        // validate_final_reporting: its own declared read of
        // final_reporting's output, plus the SAME three cross-stage
        // reads final_reporting performs (the validator independently
        // cross-checks the same upstream numbers).
        ObservedRead {
            task_id: "validate_final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/final_reporting/final_report.md".into(),
        },
        ObservedRead {
            task_id: "validate_final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/differential_expression/result.json".into(),
        },
        ObservedRead {
            task_id: "validate_final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/pathway_enrichment/result.json".into(),
        },
        ObservedRead {
            task_id: "validate_final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/contextualize_findings_with_literature/result.json".into(),
        },
    ];

    reconcile_ro_crate_edges_with_allowances(
        &mut metadata,
        &declared_edges,
        &observed_reads,
        &read_allowances,
    );

    let graph = metadata["@graph"].as_array().unwrap();
    let root = graph
        .iter()
        .find(|e| e["@id"] == "./")
        .expect("root Dataset node present");

    assert!(
        root.get("ecaax:provenanceDivergence").is_none(),
        "unresolved provenance divergence remains after the RCA I-1 fix: {:#?}",
        root.get("ecaax:provenanceDivergence")
    );

    let allowed = root
        .get("ecaax:provenanceReadAllowance")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("expected ecaax:provenanceReadAllowance to record the sanctioned reads")
        });
    // 3 cross-stage reads each for final_reporting + validate_final_reporting.
    assert_eq!(
        allowed.len(),
        6,
        "expected 6 sanctioned reads (3 per task x 2 tasks); got {allowed:#?}"
    );
    // Each root element is an `{@id}` reference (the RO-Crate/runcrate `@id`
    // fix) resolving to a first-class @graph node carrying the fields.
    for r in allowed {
        let entry = graph
            .iter()
            .find(|e| e["@id"] == r["@id"])
            .unwrap_or_else(|| panic!("read-allowance reference resolves to a @graph node: {r:?}"));
        let rationale = entry["rationale"].as_str().unwrap_or_default();
        assert!(
            !rationale.is_empty(),
            "sanctioned read is missing its rationale: {entry:?}"
        );
    }
}
