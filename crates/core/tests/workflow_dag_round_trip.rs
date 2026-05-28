//! Round-trip integration test: a `WorkflowDag`
//! whose edges carry `CompatibilityProof`s and whose ledger carries
//! `Assumption` entries must survive a `lower_to_workflow_json` →
//! `workflow_dag_from_artifact` round-trip with the sidecar-only
//! fields preserved.
//!
//! Pre-Phase-4 baseline: `dag_to_workflow_dag` alone dropped
//! sidecars (its docstring acknowledged this). v3 P4 added the
//! `workflow_dag_from_artifact` helper that re-attaches proofs +
//! assumptions from the artifact's JSONL strings; this test pins
//! that contract.

use ecaa_workflow_core::backend_emitters::{
    lower_to_workflow_json, workflow_dag_from_artifact, EmitContext,
};
use ecaa_workflow_core::workflow_contracts::edge::{
    CompatibilityProof, EdgeContract, FacetMatch, FacetMatchKind, ProofEvidence,
};
use ecaa_workflow_core::workflow_contracts::evidence::{
    Assumption, AssumptionLedger, AssumptionResolution, AssumptionSource, RiskClass,
};
use ecaa_workflow_core::workflow_contracts::implementation::{Implementation, OciImageRef};
use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};

fn align_node() -> TaskNode {
    let mut n = TaskNode::skeleton("align_reads", "Align reads to GRCh38");
    n.implementation = Implementation::ContainerCommand {
        image: OciImageRef {
            image: "ghcr.io/scripps/bio-base".into(),
            tag: "v0.4.0".into(),
            digest: "sha256:abc".into(),
            arch: vec!["amd64".into()],
            gpu: false,
        },
        command_template: vec![],
    };
    n.attributes
        .insert("role".into(), serde_json::Value::String("operation".into()));
    n.attributes
        .insert("assignee".into(), serde_json::Value::String("agent".into()));
    n
}

fn quantify_node() -> TaskNode {
    let mut n = TaskNode::skeleton("quantify_features", "Count features per gene");
    n.attributes
        .insert("role".into(), serde_json::Value::String("operation".into()));
    n.attributes
        .insert("assignee".into(), serde_json::Value::String("agent".into()));
    n
}

fn make_dag_with_sidecars() -> WorkflowDag {
    let mut ledger = AssumptionLedger::default();
    ledger.entries.push(Assumption {
        id: "a_unstranded".into(),
        statement: "Reads are unstranded (Illumina TruSeq default)".into(),
        source: AssumptionSource::LlmInferred {
            confidence: "moderate".into(),
        },
        affects_nodes: vec!["quantify_features".into()],
        risk: RiskClass::Moderate,
        resolution: AssumptionResolution::Unresolved,
        chain_of_custody: None,
    });
    ledger.entries.push(Assumption {
        id: "a_genome".into(),
        statement: "Reference is GRCh38 ENSEMBL primary assembly".into(),
        source: AssumptionSource::SmeAccepted {
            rationale: "SME confirmed GRCh38 in the intake transcript".into(),
        },
        affects_nodes: vec!["align_reads".into()],
        risk: RiskClass::Low,
        resolution: AssumptionResolution::Unresolved,
        chain_of_custody: None,
    });

    let proof = CompatibilityProof {
        producer_type: "data:0863".into(),
        consumer_type: "data:0863".into(),
        ontology_subsumption_path: vec!["data:0006".into()],
        facet_matches: vec![FacetMatch {
            facet: "genome_build".into(),
            producer: "GRCh38".into(),
            consumer: "GRCh38".into(),
            kind: FacetMatchKind::Exact,
            rationale: None,
        }],
        format_conversions: vec![],
        inserted_adapter_node_ids: vec![],
        required_validators: vec![],
        warnings: vec![],
        assumptions: vec![],
        policy_decisions: vec!["phi_propagation_allowed".into()],
        rationale: Some("BAM → BAM, both GRCh38, no conversion".into()),
        evidence: vec![ProofEvidence::RegistrySnapshot {
            registry: "edam".into(),
            snapshot_id: "1.25-20251112T1620Z".into(),
        }],
    };

    WorkflowDag {
        id: "test_dag".into(),
        nodes: vec![align_node(), quantify_node()],
        edges: vec![EdgeContract {
            from_node: "align_reads".into(),
            from_port: "bam".into(),
            to_node: "quantify_features".into(),
            to_port: "bam".into(),
            proof,
            chain_of_custody: None,
        }],
        assumptions: ledger,
        source_template: None,
    }
}

/// §9.4 — `workflow_dag_from_artifact` recovers the
/// `CompatibilityProof` on each edge and the `Assumption` entries on
/// the ledger. The pre-P4 `dag_to_workflow_dag` alone discarded both.
#[test]
fn workflow_template_to_dag_round_trips_sidecars() {
    let original = make_dag_with_sidecars();
    let ctx = EmitContext::defaults();
    let artifact =
        lower_to_workflow_json(&original, &ctx).expect("lowering well-typed DAG must succeed");

    // Sanity — sidecars are non-empty after lowering.
    assert!(
        !artifact.proofs_jsonl.is_empty(),
        "proofs_jsonl must be populated for a DAG with edges"
    );
    assert!(
        !artifact.assumptions_jsonl.is_empty(),
        "assumptions_jsonl must be populated for a DAG with ledger entries"
    );

    // Round-trip back into a WorkflowDag.
    let reconstructed = workflow_dag_from_artifact(&artifact);

    // Edge count + proof content preserved.
    assert_eq!(
        reconstructed.edges.len(),
        original.edges.len(),
        "edge count drifted across round-trip"
    );
    let recon_edge = &reconstructed.edges[0];
    let orig_edge = &original.edges[0];
    assert_eq!(recon_edge.from_node, orig_edge.from_node);
    assert_eq!(recon_edge.to_node, orig_edge.to_node);
    assert_eq!(
        recon_edge.proof.producer_type, orig_edge.proof.producer_type,
        "proof.producer_type dropped during round-trip"
    );
    assert_eq!(
        recon_edge.proof.ontology_subsumption_path, orig_edge.proof.ontology_subsumption_path,
        "proof.ontology_subsumption_path dropped during round-trip"
    );
    assert_eq!(
        recon_edge.proof.facet_matches.len(),
        1,
        "facet_matches dropped during round-trip"
    );
    assert_eq!(
        recon_edge.proof.policy_decisions, orig_edge.proof.policy_decisions,
        "policy_decisions dropped during round-trip"
    );
    assert_eq!(
        recon_edge.proof.evidence.len(),
        1,
        "proof.evidence dropped during round-trip"
    );

    // Assumption ledger preserved (entry count + content equality).
    assert_eq!(
        reconstructed.assumptions.entries.len(),
        original.assumptions.entries.len(),
        "assumption ledger entry count drifted"
    );
    // Ledger order is preserved by lowering (no sort); pre + post must match by id.
    let mut orig_ids: Vec<&str> = original
        .assumptions
        .entries
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    let mut recon_ids: Vec<&str> = reconstructed
        .assumptions
        .entries
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    orig_ids.sort();
    recon_ids.sort();
    assert_eq!(
        orig_ids, recon_ids,
        "assumption ids dropped during round-trip"
    );
    // Pick the moderate-risk LlmInferred entry and check it survived intact.
    let recon_unstranded = reconstructed
        .assumptions
        .entries
        .iter()
        .find(|a| a.id == "a_unstranded")
        .expect("a_unstranded ledger entry missing after round-trip");
    assert_eq!(recon_unstranded.risk, RiskClass::Moderate);
    assert_eq!(
        recon_unstranded.statement,
        "Reads are unstranded (Illumina TruSeq default)"
    );
    match &recon_unstranded.source {
        AssumptionSource::LlmInferred { confidence } => {
            assert_eq!(confidence, "moderate");
        }
        other => panic!("source variant drifted: {other:?}"),
    }
}
