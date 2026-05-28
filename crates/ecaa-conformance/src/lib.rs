//! ECAA v0.1 conformance suite.
//!
//! Re-exports the public API from scripps-workflow-core that any
//! second implementation needs to claim ECAA conformance, plus the
//! integration tests under `tests/`.

pub use scripps_workflow_core::audit_proof::{
    run_audit_proof, AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict,
};
pub use scripps_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocValidator};
