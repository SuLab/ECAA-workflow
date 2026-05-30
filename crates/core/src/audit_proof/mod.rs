//! D8 audit-proof invariant checker — Aim 1 deliverable.
//! Consumes already-emitted ECAA subgraph sidecars and produces
//! `runtime/audit-proof-report.json` with per-invariant verdicts.
//!
//! Invariants are warn-only at emission time: `Fail` is preserved
//! in the report but never blocks `emit_package`.
//!
//! Canonical types live in `ecaa-workflow-types::invariants`.
//! Re-exported below for backward compatibility with existing call sites.

pub mod invariants;
pub mod loader;

pub use ecaa_workflow_types::{AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict};

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
pub fn run_audit_proof(root: &Path, validator: &dyn WrrocValidator) -> Result<AuditProofReport> {
    let pkg = LoadedPackage::from_root(root)?;
    let verdicts = vec![
        check_claim_completeness(&pkg),
        check_decision_justification(&pkg),
        check_evidence_coverage(&pkg),
        check_equivalence_failure(&pkg),
        check_cross_graph_integrity(&pkg),
        check_substrate_validity(root, validator),
    ];
    Ok(AuditProofReport {
        schema_version: "0.1".to_string(),
        verdicts,
    })
}
