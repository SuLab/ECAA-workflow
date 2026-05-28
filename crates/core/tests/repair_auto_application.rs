//! Auto-application of `LowAutoAttempt` repair proposals.
//!
//! Bridges the gap between `composer_v4::planner` emitting a
//! `RepairProposal` to substrate and actually mutating the in-flight
//! DAG: `composer_v4::dag_mutation::apply_dag_modification` splices
//! the `DagModification` payload so safe mechanical repairs (gzip
//! decompression, sort/index regeneration) flow end-to-end without
//! an SME click.
//!
//! These tests pin the contract at three layers:
//!
//! 1. `apply_dag_modification` correctly mutates a `WorkflowDag` for
//! `InsertConverter` (`LowAutoAttempt` shape) — node spliced, edges
//! rewired.
//! 2. The substrate-emission path the planner uses records a
//! `RepairAccepted { attempt_kind: Auto, applied_modification: Some(_) }`
//! row for the auto-applied `LowAutoAttempt` proposal.
//! 3. `MediumUserGated` (substitute_compatible_producer) +
//! `HighCredentialedReview` (insert_liftover) proposals are routed
//! only to `RepairProposed` — auto-application never fires.

use scripps_workflow_core::composer_v4::dag_mutation::{apply_dag_modification, ApplyError};
use scripps_workflow_core::composer_v4::PlanningContext;
use scripps_workflow_core::decision_substrate::{
    drain, record, stable_id, timestamp, AttemptKind, VerifierDecision,
};
use scripps_workflow_core::repair::proposal::{
    DagModification, FacetMismatch, RepairGap, RepairProposal, RepairRiskClass,
};
use scripps_workflow_core::repair::registry::RepairRegistry;
use scripps_workflow_core::repair::strategy::GapKind;
use scripps_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract};
use scripps_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};
use scripps_workflow_core::workflow_contracts::workflow_intent::WorkflowIntent;
use std::sync::Mutex;

/// Substrate guard — every test that drains the process-wide
/// `decision_substrate` buffer serializes against this so concurrent
/// tests don't steal one another's rows.
static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

fn sample_ctx() -> PlanningContext {
    PlanningContext::new(WorkflowIntent {
        id: "auto_apply_test".into(),
        schema_version: semver::Version::new(1, 0, 0),
        goal: "test".into(),
        modality: Some("bulk_rnaseq".into()),
        ..Default::default()
    })
}

/// Synthesize a `RepairGap` that exercises the gzip-decompression
/// LowAutoAttempt path. `producer_value=gzip` + `consumer_value=uncompressed`
/// triggers `InsertGzipDecompressionConverterRepair::propose`.
fn gzip_gap() -> RepairGap {
    RepairGap {
        id: "g_gzip".into(),
        statement: "compression mismatch on edge".into(),
        kind: GapKind::ContractGap,
        consumer_node: "downstream".into(),
        consumer_port: "in_data".into(),
        producer_node: Some("upstream".into()),
        producer_port: Some("out_data".into()),
        facet_mismatches: vec![FacetMismatch {
            facet: "compression".into(),
            producer_value: "gzip".into(),
            consumer_value: "uncompressed".into(),
        }],
    }
}

/// Synthesize a `RepairGap` that exercises the liftover
/// HighCredentialedReview path. The matching strategy is
/// `InsertLiftoverRepair`; it MUST NOT auto-apply.
fn liftover_gap() -> RepairGap {
    RepairGap {
        id: "g_lift".into(),
        statement: "genome build mismatch".into(),
        kind: GapKind::ReferenceMismatch,
        consumer_node: "downstream".into(),
        consumer_port: "in_bam".into(),
        producer_node: Some("upstream".into()),
        producer_port: Some("out_bam".into()),
        facet_mismatches: vec![FacetMismatch {
            facet: "genome_build".into(),
            producer_value: "GRCh37".into(),
            consumer_value: "GRCh38".into(),
        }],
    }
}

fn skeleton_dag_with_edge() -> WorkflowDag {
    let producer = TaskNode::skeleton("upstream", "upstream node");
    let consumer = TaskNode::skeleton("downstream", "downstream node");
    WorkflowDag {
        id: "test".into(),
        nodes: vec![producer, consumer],
        edges: vec![EdgeContract {
            from_node: "upstream".into(),
            from_port: "out_data".into(),
            to_node: "downstream".into(),
            to_port: "in_data".into(),
            proof: CompatibilityProof::default(),
            chain_of_custody: None,
        }],
        ..Default::default()
    }
}

/// Drive the auto-apply path the planner uses: registry.propose →
/// record `RepairProposed` → apply LowAutoAttempt + record
/// `RepairAccepted { Auto }`. Mirrors the planner wiring at
/// `composer_v4::planner::plan` so the test validates the same
/// substrate-emission shape.
fn run_planner_auto_apply(
    gap: &RepairGap,
    dag: &mut WorkflowDag,
    ctx: &PlanningContext,
    registry: &RepairRegistry,
) -> Vec<RepairProposal> {
    let proposals = registry.propose(gap, ctx);
    for proposal in &proposals {
        record(VerifierDecision::RepairProposed {
            id: stable_id("repair_proposed", &proposal.id, &proposal.gap_id),
            timestamp: timestamp(),
            gap_id: proposal.gap_id.clone(),
            strategy: proposal.strategy_id.clone(),
            risk_class: format!("{:?}", proposal.risk_class),
            proposal_payload: serde_json::to_string(proposal).unwrap_or_default(),
        });
        if proposal.risk_class <= ctx.auto_attempt_risk_threshold {
            let payload = serde_json::to_string(&proposal.modification).unwrap_or_default();
            match apply_dag_modification(dag, &proposal.modification) {
                Ok(()) => {
                    record(VerifierDecision::RepairAccepted {
                        id: stable_id("repair_accepted", &proposal.id, "auto"),
                        timestamp: timestamp(),
                        proposal_id: proposal.id.clone(),
                        acceptor: "planner_auto_apply".into(),
                        credentials: Vec::new(),
                        attempt_kind: AttemptKind::Auto,
                        applied_modification: Some(payload),
                    });
                }
                Err(e) => {
                    record(VerifierDecision::RepairRejected {
                        id: stable_id("repair_rejected", &proposal.id, "auto_apply_failed"),
                        timestamp: timestamp(),
                        proposal_id: proposal.id.clone(),
                        reason: format!("auto-apply failed: {e}"),
                    });
                }
            }
        }
    }
    proposals
}

#[test]
fn low_auto_attempt_proposals_splice_into_dag_and_emit_substrate() {
    let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _ = drain();
    let registry = RepairRegistry::with_builtin();
    let ctx = sample_ctx();
    let mut dag = skeleton_dag_with_edge();
    let baseline_node_count = dag.nodes.len();
    let baseline_edge_count = dag.edges.len();

    let proposals = run_planner_auto_apply(&gzip_gap(), &mut dag, &ctx, &registry);

    // Builtin registry includes the LowAutoAttempt gzip strategy. The
    // gap kind is `ContractGap` + facet `compression: gzip→uncompressed`,
    // which `InsertGzipDecompressionConverterRepair::propose` matches.
    let gzip_props: Vec<_> = proposals
        .iter()
        .filter(|p| p.strategy_id == "insert_gzip_decompression")
        .collect();
    assert_eq!(
        gzip_props.len(),
        1,
        "expected one gzip proposal; got {:?}",
        proposals.iter().map(|p| &p.strategy_id).collect::<Vec<_>>()
    );
    assert_eq!(gzip_props[0].risk_class, RepairRiskClass::LowAutoAttempt);

    // The DAG must show the splice: converter node added, edge rewired.
    assert!(
        dag.nodes.len() > baseline_node_count,
        "expected node count to grow after auto-apply"
    );
    assert!(
        dag.edges.len() > baseline_edge_count,
        "expected edge count to grow after auto-apply (original edge split into two)"
    );
    assert!(
        dag.nodes.iter().any(|n| n.id == "gunzip_downstream"),
        "expected the converter node `gunzip_downstream` to be spliced into the DAG"
    );

    // The substrate must carry exactly one `RepairAccepted { attempt_kind: Auto,.. }`
    // row for the gzip proposal.
    let events = drain();
    let auto_accepts: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                VerifierDecision::RepairAccepted {
                    attempt_kind: AttemptKind::Auto,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(
        auto_accepts.len(),
        1,
        "expected exactly one auto-accept row; got {} (events: {})",
        auto_accepts.len(),
        events.len(),
    );
    // The applied_modification must round-trip back to a DagModification::InsertConverter.
    match auto_accepts[0] {
        VerifierDecision::RepairAccepted {
            applied_modification: Some(payload),
            ..
        } => {
            let modification: DagModification =
                serde_json::from_str(payload).expect("applied_modification must round-trip");
            assert!(
                matches!(modification, DagModification::InsertConverter { .. }),
                "expected an InsertConverter modification in substrate"
            );
        }
        _ => panic!("auto-accept row missing applied_modification payload"),
    }
}

#[test]
fn medium_user_gated_proposals_do_not_auto_apply() {
    let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let _ = drain();
    let registry = RepairRegistry::with_builtin();
    let ctx = sample_ctx();
    let mut dag = skeleton_dag_with_edge();
    let baseline_node_count = dag.nodes.len();
    let baseline_edge_count = dag.edges.len();

    let proposals = run_planner_auto_apply(&liftover_gap(), &mut dag, &ctx, &registry);

    // Liftover proposal must be emitted as HighCredentialedReview.
    let liftover_props: Vec<_> = proposals
        .iter()
        .filter(|p| p.strategy_id == "insert_liftover")
        .collect();
    assert_eq!(liftover_props.len(), 1);
    assert_eq!(
        liftover_props[0].risk_class,
        RepairRiskClass::HighCredentialedReview
    );

    // The DAG must be UNCHANGED — no auto-apply for HighCredentialedReview.
    assert_eq!(dag.nodes.len(), baseline_node_count);
    assert_eq!(dag.edges.len(), baseline_edge_count);

    let events = drain();

    // The substrate must carry `RepairProposed` rows but ZERO
    // `RepairAccepted` rows.
    let proposed_count = events
        .iter()
        .filter(|e| matches!(e, VerifierDecision::RepairProposed { .. }))
        .count();
    assert!(
        proposed_count >= 1,
        "expected at least one RepairProposed row; got {proposed_count}"
    );
    let accepted_count = events
        .iter()
        .filter(|e| matches!(e, VerifierDecision::RepairAccepted { .. }))
        .count();
    assert_eq!(
        accepted_count, 0,
        "MediumUserGated / HighCredentialedReview proposals MUST NOT \
         emit RepairAccepted from the auto-apply path; got {accepted_count}"
    );
}

#[test]
fn apply_dag_modification_returns_unknown_node_for_stale_proposals() {
    // Pin the contract that a strategy proposing an InsertConverter
    // against a DAG that no longer contains the producer surfaces as
    // a typed ApplyError rather than silently mutating.
    let mut dag = WorkflowDag {
        id: "test".into(),
        nodes: vec![],
        edges: vec![],
        ..Default::default()
    };
    let modification = DagModification::InsertConverter {
        converter_node: TaskNode::skeleton("gunzip_x", "decompress"),
        source_port: scripps_workflow_core::repair::proposal::PortRef {
            node_id: "absent_producer".into(),
            port_name: "out".into(),
        },
        sink_port: scripps_workflow_core::repair::proposal::PortRef {
            node_id: "absent_consumer".into(),
            port_name: "in".into(),
        },
    };
    let err = apply_dag_modification(&mut dag, &modification).unwrap_err();
    assert_eq!(err, ApplyError::UnknownNode("absent_producer".into()));
}
