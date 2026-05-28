//! Acceptance tests — affordance provenance, sidecar writer, and
//! DecisionType round-trip.
//!
//! Tests that require a real harness state or filesystem writes are
//! marked `#[ignore]`.

use ecaa_workflow_core::backend_emitters::{
    lower_to_workflow_json, EmitContext, PlotAffordanceRecord,
};
use ecaa_workflow_core::decision_log::{DecisionActor, DecisionRecord, DecisionType};
use ecaa_workflow_core::plot_affordance::{AffordanceProof, GenericPrimitive, PlotAffordance};

// ── Helpers ──────────────────────────────────────────────────────────────────

fn proof() -> AffordanceProof {
    AffordanceProof {
        source_semantic_type: "EDAM:data_3134".into(),
        ontology_walk: vec![],
        registry_snapshot_id: "snap-2026-05-08-a".into(),
        theme_version: "theme.json:sha256:abc".into(),
        rationale: "exact semantic-type match in closed catalog".into(),
    }
}

fn registered_affordance() -> PlotAffordance {
    PlotAffordance::Registered {
        figure_ids: vec!["volcano".into(), "ma_plot".into()],
        renderer_module: "runtime.plotting.stages.differential_expression".into(),
        proof: proof(),
    }
}

fn fallback_affordance() -> PlotAffordance {
    PlotAffordance::StructuralFallback {
        primitive: GenericPrimitive::Distribution,
        figure_id: GenericPrimitive::Distribution.figure_id().to_string(),
        warning: "no registered renderer for semantic type data:9999".into(),
        proof: AffordanceProof {
            source_semantic_type: "data:9999".into(),
            rationale: "structural fallback — no catalog match".into(),
            ..proof()
        },
    }
}

// ── PlotAffordanceRecord round-trip ─────────────────────────────────────────

#[test]
fn plot_affordance_record_round_trips_registered() {
    let rec = PlotAffordanceRecord {
        task_id: "differential_expression".into(),
        port_name: "result_table".into(),
        affordance: registered_affordance(),
        provisional: false,
    };
    let json = serde_json::to_string(&rec).expect("serialize");
    let back: PlotAffordanceRecord = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.task_id, rec.task_id);
    assert_eq!(back.port_name, rec.port_name);
    assert_eq!(back.provisional, rec.provisional);
    assert_eq!(back.affordance, rec.affordance);
}

#[test]
fn plot_affordance_record_round_trips_provisional_fallback() {
    let rec = PlotAffordanceRecord {
        task_id: "qc_preprocessing".into(),
        port_name: "qc_metrics".into(),
        affordance: fallback_affordance(),
        provisional: true,
    };
    let json = serde_json::to_string(&rec).expect("serialize");
    let back: PlotAffordanceRecord = serde_json::from_str(&json).expect("deserialize");
    // is_provisional() must round-trip: StructuralFallback is always provisional.
    assert!(back.affordance.is_provisional());
    assert!(back.provisional);
}

#[test]
fn is_provisional_round_trips_through_json_for_registered() {
    let a = registered_affordance();
    assert!(!a.is_provisional(), "Registered must not be provisional");
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert!(!back.is_provisional());
}

#[test]
fn is_provisional_round_trips_through_json_for_fallback() {
    let a = fallback_affordance();
    assert!(a.is_provisional(), "StructuralFallback must be provisional");
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert!(back.is_provisional());
}

#[test]
fn is_provisional_round_trips_for_deferred() {
    let a = PlotAffordance::Deferred {
        data_artifact_relpath: "runtime/outputs/x/result.parquet".into(),
        recommendation: "trajectory plot via scvelo".into(),
        sme_check_required: true,
        proof: proof(),
    };
    assert!(a.is_provisional());
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert!(back.is_provisional());
}

// ── DecisionType new variant round-trips ────────────────────────────────────

#[test]
fn decision_type_plot_affordance_resolved_round_trips() {
    let d = DecisionType::PlotAffordanceResolved {
        task_id: "differential_expression".into(),
        port_name: "result_table".into(),
        affordance_variant: "registered".into(),
        figure_ids: vec!["volcano".into(), "ma_plot".into()],
        provisional: false,
        snapshot_id: "snap-2026-05-08-a".into(),
    };
    let record = DecisionRecord::new("session-aaa", d.clone(), DecisionActor::Llm, None);
    let json = serde_json::to_string(&record).expect("serialize");
    let back: DecisionRecord = serde_json::from_str(&json).expect("deserialize");
    // Internally-tagged shape: kind == "plot_affordance_resolved"
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["decision"]["kind"], "plot_affordance_resolved");
    assert_eq!(v["decision"]["task_id"], "differential_expression");
    assert_eq!(back.decision, record.decision);
}

#[test]
fn decision_type_plot_affordance_fallback_round_trips() {
    let d = DecisionType::PlotAffordanceFallback {
        task_id: "qc_preprocessing".into(),
        port_name: "qc_metrics".into(),
        primitive: "distribution".into(),
        semantic_type: "data:9999".into(),
        fallback_reason: "no registered renderer for this semantic type".into(),
    };
    let record = DecisionRecord::new("session-bbb", d.clone(), DecisionActor::Llm, None);
    let json = serde_json::to_string(&record).expect("serialize");
    let back: DecisionRecord = serde_json::from_str(&json).expect("deserialize");
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["decision"]["kind"], "plot_affordance_fallback");
    assert_eq!(v["decision"]["primitive"], "distribution");
    assert_eq!(back.decision, record.decision);
}

// ── Sidecar writer unit test ─────────────────────────────────────────────────

/// Constructs two PlotAffordanceRecords, passes them through
/// `lower_to_workflow_json` via `EmitContext::emit_affordances`,
/// and asserts the resulting `plot_affordances_jsonl` is parseable.
/// Uses a minimal skeleton WorkflowDag (no nodes, no edges).
///
/// Marked `#[ignore]` because the WorkflowDag constructors require
/// a real taxonomy config. Promote to live once the resolution wiring
/// lands and a fixture DAG is available.
#[test]
#[ignore = "requires real WorkflowDag fixture"]
fn sidecar_writer_emits_sorted_jsonl() {
    use ecaa_workflow_core::workflow_contracts::evidence::AssumptionLedger;
    use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;

    let records = vec![
        PlotAffordanceRecord {
            task_id: "qc_preprocessing".into(), // sorts before "differential_expression" — b
            port_name: "qc_metrics".into(),
            affordance: fallback_affordance(),
            provisional: true,
        },
        PlotAffordanceRecord {
            task_id: "differential_expression".into(), // sorts first — a
            port_name: "result_table".into(),
            affordance: registered_affordance(),
            provisional: false,
        },
    ];

    let dag = WorkflowDag {
        id: "test_sidecar".into(),
        nodes: vec![],
        edges: vec![],
        assumptions: AssumptionLedger::default(),
        source_template: None,
    };
    let mut ctx = EmitContext::defaults();
    ctx.emit_affordances = Some(records);

    let artifact = lower_to_workflow_json(&dag, &ctx).unwrap();
    let lines: Vec<&str> = artifact
        .plot_affordances_jsonl
        .lines()
        .filter(|l| !l.trim().is_empty())
        .collect();

    assert_eq!(lines.len(), 2, "expected 2 JSONL lines");

    // Sorted by task_id: "differential_expression" < "qc_preprocessing".
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["task_id"], "differential_expression");
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["task_id"], "qc_preprocessing");

    // Both are parseable as PlotAffordanceRecord.
    let _r0: PlotAffordanceRecord = serde_json::from_str(lines[0]).unwrap();
    let _r1: PlotAffordanceRecord = serde_json::from_str(lines[1]).unwrap();
}

/// Verifies the sidecar writer is a no-op when emit_affordances is None.
#[test]
fn sidecar_writer_noop_when_none() {
    use ecaa_workflow_core::workflow_contracts::evidence::AssumptionLedger;
    use ecaa_workflow_core::workflow_contracts::task_node::WorkflowDag;

    let dag = WorkflowDag {
        id: "test_noop".into(),
        nodes: vec![],
        edges: vec![],
        assumptions: AssumptionLedger::default(),
        source_template: None,
    };
    let ctx = EmitContext::defaults(); // emit_affordances = None

    let artifact = lower_to_workflow_json(&dag, &ctx).unwrap();
    assert!(
        artifact.plot_affordances_jsonl.is_empty(),
        "plot_affordances_jsonl must be empty when emit_affordances = None"
    );
}

/// Verifies that PlotAffordanceRecord written to a tempdir JSONL file
/// can be read back as valid JSON. Requires no external dependencies
/// beyond std + serde_json.
#[test]
fn sidecar_tempdir_write_and_parse() {
    use std::io::Write;

    let rec = PlotAffordanceRecord {
        task_id: "differential_expression".into(),
        port_name: "result_table".into(),
        affordance: registered_affordance(),
        provisional: false,
    };

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("plot_affordances.jsonl");
    {
        let mut f = std::fs::File::create(&path).unwrap();
        let line = serde_json::to_string(&rec).unwrap();
        writeln!(f, "{}", line).unwrap();
    }

    let content = std::fs::read_to_string(&path).unwrap();
    let back: PlotAffordanceRecord =
        serde_json::from_str(content.trim()).expect("JSONL line must be valid JSON");
    assert_eq!(back.task_id.as_str(), "differential_expression");
    assert!(!back.provisional);
    assert!(!back.affordance.is_provisional());
}
