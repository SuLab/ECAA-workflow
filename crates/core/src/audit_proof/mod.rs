//! D8 audit-proof invariant checker — Aim 1 deliverable.
//! Consumes already-emitted ECAA subgraph sidecars and produces
//! `runtime/audit-proof-report.json` with per-invariant verdicts.
//!
//! Invariants are warn-only at emission time: `Fail` is preserved
//! in the report but never blocks `emit_package`.
//!
//! Canonical types live in `ecaa-workflow-types::invariants`.
//! Re-exported below for backward compatibility with existing call sites.

pub mod bench_readiness;
pub mod invariants;
pub mod loader;
pub mod output_source;

pub use ecaa_workflow_types::{
    AuditProofReport, EvaluatorInfo, InvariantId, InvariantStatus, InvariantVerdict,
};

use crate::audit_proof::invariants::{
    claim_completeness::check_claim_completeness,
    cross_graph_integrity::check_cross_graph_integrity,
    decision_justification::check_decision_justification,
    equivalence_failure::check_equivalence_failure, evidence_coverage::check_evidence_coverage,
    substrate_validity::check_substrate_validity,
};
use crate::audit_proof::loader::LoadedPackage;
use crate::wrroc_validator::WrrocValidator;
use anyhow::Result;
use std::path::Path;

/// Compose the 6 invariant checks into a single `AuditProofReport`.
/// Public entry point consumed by the emitter after all sidecars
/// have been written.
pub fn run_audit_proof(
    root: &Path,
    validator: &dyn WrrocValidator,
    clock: &dyn crate::clock::Clock,
) -> Result<AuditProofReport> {
    run_audit_proof_with_verifier(root, validator, clock, None)
}

/// As [`run_audit_proof`], but a `Some(verifier)` lets the loader read and
/// HMAC-verify the signed verdict sink (de-vacuifying Inv 1/5). `None`
/// preserves the legacy stub-only behavior.
pub fn run_audit_proof_with_verifier(
    root: &Path,
    validator: &dyn WrrocValidator,
    clock: &dyn crate::clock::Clock,
    verifier: Option<&crate::audit_writer::AuditWriter>,
) -> Result<AuditProofReport> {
    let pkg = LoadedPackage::from_root_with_verifier(root, verifier)?;
    Ok(assemble_report(&pkg, root, validator, clock))
}

/// Run the six invariant checks over an already-loaded package and assemble
/// the report. Separated from the loader step so a verifier-aware load can
/// populate the signed C-graph sink before grading.
fn assemble_report(
    pkg: &LoadedPackage,
    root: &Path,
    validator: &dyn WrrocValidator,
    clock: &dyn crate::clock::Clock,
) -> AuditProofReport {
    let verdicts = vec![
        check_claim_completeness(pkg),
        check_decision_justification(pkg),
        check_evidence_coverage(pkg),
        check_equivalence_failure(pkg),
        check_cross_graph_integrity(pkg),
        check_substrate_validity(root, validator),
    ];
    AuditProofReport {
        schema_version: "0.1".to_string(),
        ecaa_version: ecaa_workflow_types::consts::ECAA_VERSION.to_string(),
        min_reader_version: ecaa_workflow_types::consts::MIN_READER_VERSION.to_string(),
        max_reader_version: None,
        package_iri: root
            .join("ro-crate-metadata.json")
            .exists()
            .then(|| "ro-crate-metadata.json".to_string()),
        evaluated_at: Some(clock.now_rfc3339()),
        evaluator: ecaa_workflow_types::EvaluatorInfo::reference(),
        verdicts,
    }
}
