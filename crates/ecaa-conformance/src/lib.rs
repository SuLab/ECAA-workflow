//! ECAA v0.1 conformance suite.
//!
//! This crate is NOT an independent reimplementation of the ECAA primitive.
//! It re-exports `ecaa-workflow-core`'s public conformance API (audit-proof
//! runner + WRROC validator) and bundles the integration tests under `tests/`
//! that constitute the machine-checkable conformance contract. A second
//! implementer claims ECAA v0.1 conformance by running this harness against
//! THEIR OWN emitted packages and passing every test.

pub use ecaa_workflow_core::audit_proof::{
    run_audit_proof, AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict,
};
pub use ecaa_workflow_core::wrroc_validator::{NoopWrrocValidator, WrrocOutcome, WrrocValidator};
