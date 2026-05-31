//! Invariant 6: substrate-validity.
//! Delegates to the WRROC v0.5 Tier-3 validator already in core.

use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use crate::wrroc_validator::{WrrocOutcome, WrrocValidator};
use std::path::Path;

/// Check substrate validity.
///
/// Delegates to the injected validator's three-valued
/// [`WrrocValidator::validate_outcome`]. The mapping is:
/// - `Pass`        → `InvariantStatus::Pass`
/// - `Fail(msgs)`  → `InvariantStatus::Fail` (one violation per package run)
/// - `Unverified`  → `InvariantStatus::Unverified` — including the case
///   where the injected validator is the no-op adapter (runcrate not run).
///   A non-run must NOT be recorded as a substrate-validity pass.
pub fn check_substrate_validity(root: &Path, validator: &dyn WrrocValidator) -> InvariantVerdict {
    let descriptor = root.join("ro-crate-metadata.json");
    if !descriptor.exists() {
        return InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Unverified,
            detail: Some("ro-crate-metadata.json absent".into()),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    match validator.validate_outcome(&[root]) {
        WrrocOutcome::Pass => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Pass,
            detail: None,
            n_inspected: 1,
            n_violations: 0,
        },
        WrrocOutcome::Fail(msgs) => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Fail,
            detail: Some(msgs.join("; ")),
            n_inspected: 1,
            n_violations: msgs.len(),
        },
        WrrocOutcome::Unverified(reason) => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            // The descriptor IS present (we got past the early return), but
            // no real validation ran — count it as inspected-but-unverified.
            status: InvariantStatus::Unverified,
            detail: Some(reason),
            n_inspected: 1,
            n_violations: 0,
        },
    }
}
