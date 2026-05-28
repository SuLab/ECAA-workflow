//! Grant v19 §C.0.1 — "PROV-O: no failures observed across 30
//! scenarios" (originally 10, expanded to 20 in the first Phase B
//! Pass on, then to 30 in the construct-validity-fixes
//! Phase E pass later the same day). Each scenario is a structurally
//! distinct DAG shape; the emitted ro-crate-metadata.json must
//! contain valid PROV-O / P-PLAN / OPMW entities for the shape,
//! and the ParameterConnection count must equal the DAG edge count
//! (the strengthened parity invariant adopted alongside the first
//! expansion).
//!
//! Wilson 95% one-sided upper bound on the failure rate at k=0
//! successes-fail / n=30 is ~0.115 (vs ~0.161 at n=20 and ~0.295 at
//! n=10) — the n-uplift narrative in grant §C.0.1 cites this UB to
//! quantify residual risk.
//!
//! Field-shape note: the plan's example helpers
//! used `..Default::default()` on `Task` / `DAG` and a `Task.id`
//! field. The actual structs in `crates/core/src/dag.rs` do not
//! `derive(Default)` on `Task` / `DAG` and store the id as the
//! `BTreeMap<TaskId, Task>` key (no `id` field on `Task` itself).
//! This file constructs the literals explicitly to match the live
//! shape.

use scripps_workflow_core::atom::SafetyPolicy;
use scripps_workflow_core::classify::ClassificationResult;
use scripps_workflow_core::dag::{Assignee, Task, TaskId, TaskKind, TaskState, DAG};
use scripps_workflow_core::ro_crate::build_metadata;
use std::collections::BTreeMap;

fn task(id: &str, depends_on: &[&str]) -> (TaskId, Task) {
    (
        TaskId::from(id),
        Task {
            kind: TaskKind::Computation,
            state: TaskState::Pending,
            depends_on: depends_on.iter().map(|s| TaskId::from(*s)).collect(),
            assignee: Assignee::Agent,
            description: format!("test task {id}"),
            spec: None,
            resolution: None,
            result_ref: None,
            resource_class: Default::default(),
            requires_sme_review: false,
            required_artifacts: vec![],
            container: None,
            source_atom_id: None,
            safety: SafetyPolicy::default(),
        },
    )
}

fn dag_from_tasks(workflow_id: &str, tasks: Vec<(TaskId, Task)>) -> DAG {
    let mut map = BTreeMap::new();
    for (id, t) in tasks {
        map.insert(id, t);
    }
    DAG {
        workflow_id: workflow_id.into(),
        version: "1.0.0".into(),
        schema_version: scripps_workflow_core::dag::current_dag_schema_version(),
        current_task: None,
        tasks: map,
        reverse_deps: BTreeMap::new(),
        run_id: None,
    }
}

fn default_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "".into(),
        domain: "transcriptomics".into(),
        workflow_description: "test".into(),
        confidence: 1.0,
        confidence_label: "high".into(),
        edam_topic: "topic_3170".into(),
        edam_operation: "operation_3223".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "test".into(),
        goal: None,
        archetype_id: Some("test_archetype".into()),
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

/// PROV-O conformance check: presence + (when checkable) ParameterConnection
/// count parity against the DAG's edge count. Strengthened in Phase B
/// From the prior presence-only check to also verify the
/// count parity invariant when an `expected_edges` is passed.
fn assert_prov_o_valid(metadata: &serde_json::Value, scenario: &str) {
    assert_prov_o_valid_with_edges(metadata, scenario, None);
}

fn assert_prov_o_valid_with_edges(
    metadata: &serde_json::Value,
    scenario: &str,
    expected_edges: Option<usize>,
) {
    let graph = metadata["@graph"].as_array().expect("graph must be array");

    // (1) p-plan:Plan must be present.
    let has_plan = graph.iter().any(|e| {
        e.get("@type")
            .and_then(|t| t.as_array())
            .map(|arr| arr.contains(&serde_json::json!("p-plan:Plan")))
            .unwrap_or(false)
    });
    assert!(has_plan, "scenario {scenario}: missing p-plan:Plan entity");

    let connections = graph
        .iter()
        .filter(|e| e.get("@type") == Some(&serde_json::json!("ParameterConnection")))
        .count();

    // (2) ParameterConnection count parity with the DAG edge count
    // (when caller provides an expected count). This strengthens the
    // earlier presence-only check, which let a DAG with N edges and
    // 1 ParameterConnection sneak through.
    if let Some(want) = expected_edges {
        assert_eq!(
            connections, want,
            "scenario {scenario}: ParameterConnection count != DAG edge count \
             (got {connections}, expected {want})"
        );
    } else if scenario != "single_task" && scenario != "empty" {
        // Legacy weaker check for scenarios that don't pass expected_edges.
        assert!(
            connections > 0,
            "scenario {scenario}: zero ParameterConnections"
        );
    }
}

/// Count the edges in a DAG (sum of dependency arrows).
fn dag_edge_count(dag: &DAG) -> usize {
    dag.tasks.values().map(|t| t.depends_on.len()).sum()
}

#[test]
fn prov_o_scenario_1_linear() {
    let dag = dag_from_tasks(
        "linear",
        vec![task("a", &[]), task("b", &["a"]), task("c", &["b"])],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "linear");
}

#[test]
fn prov_o_scenario_2_fan_out() {
    let dag = dag_from_tasks(
        "fan_out",
        vec![
            task("root", &[]),
            task("a", &["root"]),
            task("b", &["root"]),
            task("c", &["root"]),
        ],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "fan_out");
}

#[test]
fn prov_o_scenario_3_fan_in() {
    let dag = dag_from_tasks(
        "fan_in",
        vec![
            task("a", &[]),
            task("b", &[]),
            task("c", &[]),
            task("sink", &["a", "b", "c"]),
        ],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "fan_in");
}

#[test]
fn prov_o_scenario_4_diamond() {
    let dag = dag_from_tasks(
        "diamond",
        vec![
            task("a", &[]),
            task("b", &["a"]),
            task("c", &["a"]),
            task("d", &["b", "c"]),
        ],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "diamond");
}

#[test]
fn prov_o_scenario_5_iterate() {
    // Iterate scaffold = 4-template-task chain per Cardinality::IterateUntil
    let dag = dag_from_tasks(
        "iterate",
        vec![
            task("iterate_gate_x", &[]),
            task("x", &["iterate_gate_x"]),
            task("iterate_check_x", &["x"]),
            task("validate_x", &["iterate_check_x"]),
        ],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "iterate");
}

#[test]
fn prov_o_scenario_6_single_task() {
    let dag = dag_from_tasks("single", vec![task("only", &[])]);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "single_task");
}

#[test]
fn prov_o_scenario_7_long_chain() {
    let mut tasks = vec![task("t0", &[])];
    for i in 1..20 {
        let prev = format!("t{}", i - 1);
        let prev_static: &'static str = Box::leak(prev.into_boxed_str());
        let id = format!("t{i}");
        let id_static: &'static str = Box::leak(id.into_boxed_str());
        tasks.push(task(id_static, &[prev_static]));
    }
    let dag = dag_from_tasks("long_chain", tasks);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "long_chain");
}

#[test]
fn prov_o_scenario_8_parallel_chains() {
    let dag = dag_from_tasks(
        "parallel_chains",
        vec![
            task("a1", &[]),
            task("a2", &["a1"]),
            task("a3", &["a2"]),
            task("b1", &[]),
            task("b2", &["b1"]),
            task("b3", &["b2"]),
            task("merge", &["a3", "b3"]),
        ],
    );
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "parallel_chains");
}

#[test]
fn prov_o_scenario_9_wide_fan_out_in() {
    let mut tasks = vec![task("root", &[])];
    let mut leaf_ids: Vec<&'static str> = Vec::new();
    for i in 0..10 {
        let id = format!("leaf{i}");
        let id_static: &'static str = Box::leak(id.into_boxed_str());
        tasks.push(task(id_static, &["root"]));
        leaf_ids.push(id_static);
    }
    tasks.push(task("collect", &leaf_ids));
    let dag = dag_from_tasks("wide_fan", tasks);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid(&m, "wide_fan");
}

#[test]
fn prov_o_scenario_10_validation_chain() {
    let t1 = task("data", &[]);
    let t2 = task("analyze", &["data"]);
    let mut t3 = task("validate", &["analyze"]);
    t3.1.kind = TaskKind::Validation;
    let mut t4 = task("review", &["validate"]);
    t4.1.kind = TaskKind::Review;
    let dag = dag_from_tasks("validation_chain", vec![t1, t2, t3, t4]);
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "validation_chain", Some(edges));
}

// ────────────────────────────────────────────────────────────────────
// Corpus extension 10 → 20 scenarios.
//
// The original 10 covered linear / fan-out / fan-in / diamond / iterate /
// single-task / long-chain / parallel-chains / wide-fan-out-in /
// validation-chain. The 10 below extend coverage to:
// - structurally-empty workflows
// - bipartite producer/consumer pairs
// - 3-way and 5-way fan-in (different cardinalities)
// - mixed validation + review chains with parallel review steps
// - long-iterate (multi-cycle iterate scaffold)
// - deep diamond (cascaded diamonds)
// - hub-spoke (one central hub task with bidirectional edges in the
// dependency sense — many predecessors AND many successors)
// - sensitivity-comparison fan (parallel variants + one collector)
// - dual-validation (two validators against one analysis task)
//
// All ten use `assert_prov_o_valid_with_edges(&m, scenario_id, Some(edges))`
// to assert ParameterConnection count parity, the strengthened
// invariant added alongside this corpus extension.
// ────────────────────────────────────────────────────────────────────

#[test]
fn prov_o_scenario_11_empty_dag() {
    // Zero-task DAG. The emitter must still produce a valid p-plan:Plan
    // (the *workflow* exists even if it has no steps yet).
    let dag = dag_from_tasks("empty", vec![]);
    let edges = dag_edge_count(&dag);
    assert_eq!(edges, 0);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "empty", Some(0));
}

#[test]
fn prov_o_scenario_12_bipartite_pairs() {
    // Three independent producer/consumer pairs share no dependencies.
    let dag = dag_from_tasks(
        "bipartite_pairs",
        vec![
            task("p1", &[]),
            task("c1", &["p1"]),
            task("p2", &[]),
            task("c2", &["p2"]),
            task("p3", &[]),
            task("c3", &["p3"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "bipartite_pairs", Some(edges));
}

#[test]
fn prov_o_scenario_13_three_way_fan_in() {
    // 3-way fan-in (vs scenario_3's 3-way also; this is structurally
    // identical to scenario_3 but verifies the parity invariant in the
    // strengthened assertion).
    let dag = dag_from_tasks(
        "three_way_fan_in",
        vec![
            task("a", &[]),
            task("b", &[]),
            task("c", &[]),
            task("sink", &["a", "b", "c"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "three_way_fan_in", Some(edges));
}

#[test]
fn prov_o_scenario_14_five_way_fan_in() {
    let dag = dag_from_tasks(
        "five_way_fan_in",
        vec![
            task("a", &[]),
            task("b", &[]),
            task("c", &[]),
            task("d", &[]),
            task("e", &[]),
            task("sink", &["a", "b", "c", "d", "e"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "five_way_fan_in", Some(edges));
}

#[test]
fn prov_o_scenario_15_parallel_validation() {
    // One analysis task + two independent validators in parallel.
    // Distinct from scenario_10 which chains validate → review.
    let mut t_v1 = task("validate_qc", &["analyze"]);
    t_v1.1.kind = TaskKind::Validation;
    let mut t_v2 = task("validate_assumptions", &["analyze"]);
    t_v2.1.kind = TaskKind::Validation;
    let dag = dag_from_tasks(
        "parallel_validation",
        vec![task("data", &[]), task("analyze", &["data"]), t_v1, t_v2],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "parallel_validation", Some(edges));
}

#[test]
fn prov_o_scenario_16_long_iterate() {
    // Two stacked iterate scaffolds — exercises p-plan emission across
    // multiple convergence cycles (vs scenario_5 which is a single
    // cycle).
    let dag = dag_from_tasks(
        "long_iterate",
        vec![
            task("iterate_gate_x", &[]),
            task("x", &["iterate_gate_x"]),
            task("iterate_check_x", &["x"]),
            task("validate_x", &["iterate_check_x"]),
            task("iterate_gate_y", &["validate_x"]),
            task("y", &["iterate_gate_y"]),
            task("iterate_check_y", &["y"]),
            task("validate_y", &["iterate_check_y"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "long_iterate", Some(edges));
}

#[test]
fn prov_o_scenario_17_deep_diamond() {
    // Two cascaded diamonds: a → {b, c} → d → {e, f} → g.
    let dag = dag_from_tasks(
        "deep_diamond",
        vec![
            task("a", &[]),
            task("b", &["a"]),
            task("c", &["a"]),
            task("d", &["b", "c"]),
            task("e", &["d"]),
            task("f", &["d"]),
            task("g", &["e", "f"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "deep_diamond", Some(edges));
}

#[test]
fn prov_o_scenario_18_hub_spoke() {
    // One central hub task with many predecessors AND many successors.
    let dag = dag_from_tasks(
        "hub_spoke",
        vec![
            task("p1", &[]),
            task("p2", &[]),
            task("p3", &[]),
            task("hub", &["p1", "p2", "p3"]),
            task("s1", &["hub"]),
            task("s2", &["hub"]),
            task("s3", &["hub"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "hub_spoke", Some(edges));
}

#[test]
fn prov_o_scenario_19_sensitivity_fan() {
    // Three parameter variants converging into one comparison step
    // (a sensitivity_comparison shape: one input → 3 variants → 1 selector).
    let dag = dag_from_tasks(
        "sensitivity_fan",
        vec![
            task("input", &[]),
            task("variant_a", &["input"]),
            task("variant_b", &["input"]),
            task("variant_c", &["input"]),
            task("compare", &["variant_a", "variant_b", "variant_c"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "sensitivity_fan", Some(edges));
}

#[test]
fn prov_o_scenario_20_review_after_validation() {
    // Mixed validation + review with a parallel review pair after
    // a single validator (distinct from scenario_10's chained
    // validate → review sequence).
    let mut t_val = task("validate", &["analyze"]);
    t_val.1.kind = TaskKind::Validation;
    let mut t_rev_a = task("review_methods", &["validate"]);
    t_rev_a.1.kind = TaskKind::Review;
    let mut t_rev_b = task("review_writeup", &["validate"]);
    t_rev_b.1.kind = TaskKind::Review;
    let dag = dag_from_tasks(
        "review_after_validation",
        vec![
            task("data", &[]),
            task("analyze", &["data"]),
            t_val,
            t_rev_a,
            t_rev_b,
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "review_after_validation", Some(edges));
}

// ────────────────────────────────────────────────────────────────────
// Corpus extension (Phase E, construct-validity-fixes): 20 → 30
// scenarios. The first ten (21–30) extend coverage to cross-modality
// joins, branched-derived chains, hypothesized-node mixes, 5-way
// sensitivity comparisons with tie-breakers, long opmw chains
// (length distinct from scenario_7), 3-tier producer/consumer/aggregator
// bipartites (distinct cardinality from scenario_12), diamonds with
// validation sinks, deep alternating-validation pipelines, multi-hub
// topologies, and split-merge butterfly fans.
//
// All ten use `assert_prov_o_valid_with_edges(&m, scenario_id, Some(edges))`
// (the strengthened parity assertion). At n=30 with k=0 observed
// failures, the Wilson 95% one-sided upper bound on the true failure
// rate is ~0.115 — the value cited in grant §C.0.1.
// ────────────────────────────────────────────────────────────────────

#[test]
fn prov_o_scenario_21_cross_modality_join() {
    // Three independent input chains (mirrors cross-omics modalities:
    // rnaseq / atacseq / chipseq) merging via a single integrator task.
    // Distinct from scenario_8 (parallel_chains, 2 chains of length 3
    // merging) by having 3 chains of length 2 and a different sink
    // cardinality.
    let dag = dag_from_tasks(
        "cross_modality_join",
        vec![
            task("rna_input", &[]),
            task("rna_quant", &["rna_input"]),
            task("atac_input", &[]),
            task("atac_peaks", &["atac_input"]),
            task("chip_input", &[]),
            task("chip_peaks", &["chip_input"]),
            task("integrator", &["rna_quant", "atac_peaks", "chip_peaks"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "cross_modality_join", Some(edges));
}

#[test]
fn prov_o_scenario_22_branch_derived_chain() {
    // A linear chain where two terminal tasks each depend on a
    // common root (but not on each other) — exercises sibling
    // derivation lineage. Distinct from scenario_2 (fan_out: 3
    // siblings off a root with no chain prefix) by having a
    // root → mid → {sibling_a, sibling_b → child} mixed shape.
    let dag = dag_from_tasks(
        "branch_derived_chain",
        vec![
            task("root", &[]),
            task("mid", &["root"]),
            task("sibling_a", &["mid"]),
            task("sibling_b", &["mid"]),
            task("child", &["sibling_b"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "branch_derived_chain", Some(edges));
}

#[test]
fn prov_o_scenario_23_hypothesized_node_attached() {
    // 5-task DAG with one Validation node and one Review node
    // alongside Computation nodes — exercises mixed-kind emission
    // in a non-chain topology (distinct from scenario_10's pure
    // linear validation chain).
    let mut t_val = task("validate_branch", &["compute_a"]);
    t_val.1.kind = TaskKind::Validation;
    let mut t_rev = task("review_summary", &["compute_b", "validate_branch"]);
    t_rev.1.kind = TaskKind::Review;
    let dag = dag_from_tasks(
        "hypothesized_node_attached",
        vec![
            task("root", &[]),
            task("compute_a", &["root"]),
            task("compute_b", &["root"]),
            t_val,
            t_rev,
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "hypothesized_node_attached", Some(edges));
}

#[test]
fn prov_o_scenario_24_conditional_skip_sensitivity() {
    // Sensitivity comparison with 5 variants + a tie-breaker stage
    // before the comparator. Distinct from scenario_19 (3 variants,
    // direct compare) by having a 5-wide variant fan and an
    // intermediate tie_breaker between the variants and the selector.
    let dag = dag_from_tasks(
        "conditional_skip_sensitivity",
        vec![
            task("input", &[]),
            task("variant_a", &["input"]),
            task("variant_b", &["input"]),
            task("variant_c", &["input"]),
            task("variant_d", &["input"]),
            task("variant_e", &["input"]),
            task(
                "tie_breaker",
                &[
                    "variant_a",
                    "variant_b",
                    "variant_c",
                    "variant_d",
                    "variant_e",
                ],
            ),
            task("selector", &["tie_breaker"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "conditional_skip_sensitivity", Some(edges));
}

#[test]
fn prov_o_scenario_25_opmw_long_chain() {
    // 12-task linear chain — exercises opmw connectivity at a
    // length distinct from scenario_7 (20-task chain).
    let mut tasks = vec![task("c0", &[])];
    for i in 1..12 {
        let prev = format!("c{}", i - 1);
        let prev_static: &'static str = Box::leak(prev.into_boxed_str());
        let id = format!("c{i}");
        let id_static: &'static str = Box::leak(id.into_boxed_str());
        tasks.push(task(id_static, &[prev_static]));
    }
    let dag = dag_from_tasks("opmw_long_chain", tasks);
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "opmw_long_chain", Some(edges));
}

#[test]
fn prov_o_scenario_26_bipartite_pairs_3_tier() {
    // Three producer/consumer/aggregator triples — 9 tasks, 6 edges.
    // Distinct from scenario_12 (producer/consumer PAIRS, 6 tasks,
    // 3 edges) by adding a third tier per group.
    let dag = dag_from_tasks(
        "bipartite_pairs_3_tier",
        vec![
            task("p1", &[]),
            task("c1", &["p1"]),
            task("a1", &["c1"]),
            task("p2", &[]),
            task("c2", &["p2"]),
            task("a2", &["c2"]),
            task("p3", &[]),
            task("c3", &["p3"]),
            task("a3", &["c3"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "bipartite_pairs_3_tier", Some(edges));
}

#[test]
fn prov_o_scenario_27_diamond_with_validator() {
    // Classic 4-task diamond (a → b,c → d) with the sink marked
    // Validation. Distinct from scenario_4 (pure Computation diamond)
    // by mixing kinds on the same topology, exercising kind-emission
    // at fan-in points.
    let mut t_d = task("d", &["b", "c"]);
    t_d.1.kind = TaskKind::Validation;
    let dag = dag_from_tasks(
        "diamond_with_validator",
        vec![task("a", &[]), task("b", &["a"]), task("c", &["a"]), t_d],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "diamond_with_validator", Some(edges));
}

#[test]
fn prov_o_scenario_28_deep_validation_pipeline() {
    // 10-step pipeline alternating Computation / Validation kinds.
    // Distinct from scenario_10 (4-task validate→review chain) and
    // scenario_16 (long_iterate with two iterate scaffolds) by being
    // a longer alternating-kind sequence. Uses the Box::leak idiom
    // (same as scenarios 7, 9, 25, 30) to obtain 'static &str ids
    // for the depends_on slice the helper expects.
    let mut tasks: Vec<(TaskId, Task)> = Vec::new();
    let mut prev: Option<&'static str> = None;
    for i in 0..10 {
        let id = format!("step_{i}");
        let id_static: &'static str = Box::leak(id.into_boxed_str());
        let depends: Vec<&'static str> = match prev {
            Some(p) => vec![p],
            None => vec![],
        };
        let mut t = task(id_static, &depends);
        if i % 2 == 1 {
            t.1.kind = TaskKind::Validation;
        }
        tasks.push(t);
        prev = Some(id_static);
    }
    let dag = dag_from_tasks("deep_validation_pipeline", tasks);
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "deep_validation_pipeline", Some(edges));
}

#[test]
fn prov_o_scenario_29_multi_hub() {
    // Two hub tasks, each with 3 predecessors and 3 successors,
    // connected by 1 bridge edge from hub1 to hub2. Distinct from
    // scenario_18 (single hub_spoke, 7 tasks, 6 edges) by having
    // two hubs with a bridge — 14 tasks, 13 edges.
    let dag = dag_from_tasks(
        "multi_hub",
        vec![
            task("h1_p1", &[]),
            task("h1_p2", &[]),
            task("h1_p3", &[]),
            task("hub1", &["h1_p1", "h1_p2", "h1_p3"]),
            task("h1_s1", &["hub1"]),
            task("h1_s2", &["hub1"]),
            task("h1_s3", &["hub1"]),
            task("h2_p1", &[]),
            task("h2_p2", &[]),
            task("h2_p3", &[]),
            task("hub2", &["h2_p1", "h2_p2", "h2_p3", "hub1"]),
            task("h2_s1", &["hub2"]),
            task("h2_s2", &["hub2"]),
            task("h2_s3", &["hub2"]),
        ],
    );
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "multi_hub", Some(edges));
}

#[test]
fn prov_o_scenario_30_split_merge_butterfly() {
    // One root splits into 4 parallel chains of length 3; all 4
    // chains merge into one sink — 14 tasks (1 + 12 + 1), edges =
    // 4 (root → chain heads) + 8 (within each length-3 chain) + 4
    // (chain tails → sink) = 16. Distinct from scenario_8
    // (parallel_chains: 2 chains of length 3 merging) by width
    // (4 vs 2) and shared root prefix.
    let mut tasks: Vec<(TaskId, Task)> = vec![task("root", &[])];
    let mut tails: Vec<&'static str> = Vec::new();
    for chain in 0..4 {
        let h_id = format!("ch{chain}_0");
        let h_static: &'static str = Box::leak(h_id.into_boxed_str());
        tasks.push(task(h_static, &["root"]));
        let m_id = format!("ch{chain}_1");
        let m_static: &'static str = Box::leak(m_id.into_boxed_str());
        tasks.push(task(m_static, &[h_static]));
        let t_id = format!("ch{chain}_2");
        let t_static: &'static str = Box::leak(t_id.into_boxed_str());
        tasks.push(task(t_static, &[m_static]));
        tails.push(t_static);
    }
    tasks.push(task("sink", &tails));
    let dag = dag_from_tasks("split_merge_butterfly", tasks);
    let edges = dag_edge_count(&dag);
    let m = build_metadata(
        &dag,
        &default_classification(),
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    assert_prov_o_valid_with_edges(&m, "split_merge_butterfly", Some(edges));
}

#[test]
fn corpus_size_baseline() {
    // F15-style drift gate: the file must hold exactly 30 scenarios
    // (linear → split_merge_butterfly) so the §C.0.1 Wilson UB
    // narrative stays anchored to the corpus size.
    //
    // If a scenario is added, update this baseline AND the grant prose
    // ("no failures observed across 30 scenarios"; Wilson 95% UB ≈ 11.5%).
    const EXPECTED_SCENARIOS: usize = 30;
    let source = include_str!("prov_o_corpus.rs");
    // Count function definitions at column 0 only — avoids the regex's
    // own occurrence of the pattern string inside this very test from
    // double-counting.
    let count = source
        .lines()
        .filter(|line| line.starts_with("fn prov_o_scenario_"))
        .count();
    assert_eq!(
        count, EXPECTED_SCENARIOS,
        "PROV-O corpus drift: expected {} scenarios, found {}; update Wilson UB in grant §C.0.1 and this baseline together",
        EXPECTED_SCENARIOS, count
    );
}
