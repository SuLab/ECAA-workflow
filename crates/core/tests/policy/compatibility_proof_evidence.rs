//! Asserts that every compatible edge's `CompatibilityProof.evidence` is
//! non-empty and carries at least one `RegistrySnapshot` or `ValidatorRun`
//! entry.
//!
//! Closes the grant PAR-26-040 "registry-snapshot ids, validator outputs"
//! claim (E30): `CompatibilityProof.evidence` must be populated at
//! construction time by the engine, not left empty in production.

use ecaa_workflow_core::compatibility::engine::{
    AdapterPolicy, CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine,
    PlanningContext,
};
use ecaa_workflow_core::workflow_contracts::edge::ProofEvidence;
use ecaa_workflow_core::workflow_contracts::port::PortContract;
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;

fn port(iri: &str) -> PortContract {
    PortContract {
        name: "port".into(),
        semantic_type: SemanticType::edam(iri, ""),
        ..Default::default()
    }
}

fn engine() -> DeterministicCompatibilityEngine {
    DeterministicCompatibilityEngine::new()
}

// ── helpers ────────────────────────────────────────────────────────────────

fn extract_evidence(result: CompatibilityResult) -> Vec<ProofEvidence> {
    match result {
        CompatibilityResult::Compatible(proof) => proof.evidence,
        CompatibilityResult::CompatibleWithAdapters { proof, .. } => proof.evidence,
        other => panic!("expected Compatible result, got {other:?}"),
    }
}

// ── baseline: edge_compatibility ValidatorRun always present ───────────────

#[test]
fn compatible_proof_evidence_non_empty() {
    let engine = engine();
    let result = engine.prove(
        &port("data:0863"),
        &port("data:0863"),
        &PlanningContext::default(),
    );
    let evidence = extract_evidence(result);
    assert!(
        !evidence.is_empty(),
        "evidence must not be empty for a compatible edge"
    );
}

#[test]
fn compatible_proof_contains_validator_run() {
    let engine = engine();
    let result = engine.prove(
        &port("data:0863"),
        &port("data:0863"),
        &PlanningContext::default(),
    );
    let evidence = extract_evidence(result);
    let has_validator = evidence.iter().any(|e| {
        matches!(e, ProofEvidence::ValidatorRun { validator_id, .. } if validator_id == "edge_compatibility")
    });
    assert!(
        has_validator,
        "expected edge_compatibility ValidatorRun in evidence; got: {evidence:?}"
    );
}

// ── registry snapshots populated from PlanningContext ──────────────────────

#[test]
fn atom_snapshot_id_in_planning_context_surfaces_as_registry_snapshot() {
    let engine = engine();
    let ctx = PlanningContext {
        atom_snapshot_id: Some("atoms-v89-20260515T1200Z".into()),
        ..Default::default()
    };
    let result = engine.prove(&port("data:0863"), &port("data:0863"), &ctx);
    let evidence = extract_evidence(result);
    let has_atom_snap = evidence.iter().any(|e| {
        matches!(e, ProofEvidence::RegistrySnapshot { registry, snapshot_id }
            if registry == "atom_registry" && snapshot_id == "atoms-v89-20260515T1200Z")
    });
    assert!(
        has_atom_snap,
        "atom_snapshot_id should produce RegistrySnapshot{{registry: atom_registry, ...}}; got: {evidence:?}"
    );
}

#[test]
fn ontology_snapshot_id_in_planning_context_surfaces_as_registry_snapshot() {
    let engine = engine();
    let ctx = PlanningContext {
        ontology_snapshot_id: Some("edam-1.25-20251112T1620Z".into()),
        ..Default::default()
    };
    let result = engine.prove(&port("data:0863"), &port("data:0863"), &ctx);
    let evidence = extract_evidence(result);
    let has_onto_snap = evidence.iter().any(|e| {
        matches!(e, ProofEvidence::RegistrySnapshot { registry, snapshot_id }
            if registry == "ontology" && snapshot_id == "edam-1.25-20251112T1620Z")
    });
    assert!(
        has_onto_snap,
        "ontology_snapshot_id should produce RegistrySnapshot{{registry: ontology, ...}}; got: {evidence:?}"
    );
}

#[test]
fn both_snapshots_plus_validator_run_when_both_ids_supplied() {
    let engine = engine();
    let ctx = PlanningContext {
        atom_snapshot_id: Some("atoms-v89-20260515T1200Z".into()),
        ontology_snapshot_id: Some("edam-1.25-20251112T1620Z".into()),
        ..Default::default()
    };
    let result = engine.prove(&port("data:0863"), &port("data:0863"), &ctx);
    let evidence = extract_evidence(result);

    let registry_snaps: Vec<_> = evidence
        .iter()
        .filter(|e| matches!(e, ProofEvidence::RegistrySnapshot { .. }))
        .collect();
    assert_eq!(
        registry_snaps.len(),
        2,
        "expected 2 RegistrySnapshot entries (atom_registry + ontology); got: {evidence:?}"
    );

    let validator_runs: Vec<_> = evidence
        .iter()
        .filter(|e| matches!(e, ProofEvidence::ValidatorRun { .. }))
        .collect();
    assert!(
        !validator_runs.is_empty(),
        "expected at least one ValidatorRun entry; got: {evidence:?}"
    );
}

// ── evidence is preserved through adapters ─────────────────────────────────

#[test]
fn compatible_with_adapters_proof_evidence_non_empty() {
    let engine = engine();
    let mut producer = port("data:0863");
    producer.genome_build = Some("GRCh37".into());
    let mut consumer = port("data:0863");
    consumer.genome_build = Some("GRCh38".into());
    let ctx = PlanningContext {
        adapter_policy: AdapterPolicy {
            allow_lossless: true,
            allow_lossy_with_assumption: true,
            allow_risky_with_confirmation: true,
        },
        atom_snapshot_id: Some("atoms-v89".into()),
        ..Default::default()
    };
    let result = engine.prove(&producer, &consumer, &ctx);
    match result {
        CompatibilityResult::CompatibleWithAdapters { proof, .. } => {
            assert!(
                !proof.evidence.is_empty(),
                "CompatibleWithAdapters proof must carry evidence; got: {:?}",
                proof.evidence
            );
            let has_snap = proof.evidence.iter().any(|e| {
                matches!(e, ProofEvidence::RegistrySnapshot { registry, .. } if registry == "atom_registry")
            });
            assert!(
                has_snap,
                "CompatibleWithAdapters proof must have atom_registry RegistrySnapshot; got: {:?}",
                proof.evidence
            );
        }
        other => panic!("expected CompatibleWithAdapters, got {other:?}"),
    }
}

// ── evidence is stable across replays ──────────────────────────────────────

#[test]
fn evidence_is_deterministic_across_replays() {
    let engine = engine();
    let ctx = PlanningContext {
        atom_snapshot_id: Some("atoms-v89".into()),
        ontology_snapshot_id: Some("edam-1.25".into()),
        ..Default::default()
    };
    let mut prev: Option<Vec<ProofEvidence>> = None;
    for _ in 0..20 {
        let result = engine.prove(&port("data:0863"), &port("data:0863"), &ctx);
        let evidence = extract_evidence(result);
        if let Some(ref p) = prev {
            assert_eq!(
                p, &evidence,
                "evidence must be deterministic across replays"
            );
        }
        prev = Some(evidence);
    }
}
