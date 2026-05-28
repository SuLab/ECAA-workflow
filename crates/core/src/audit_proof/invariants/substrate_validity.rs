//! Invariant 6: substrate-validity.
//! Delegates to the WRROC v0.5 Tier-3 validator already in core.

use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use crate::wrroc_validator::WrrocValidator;
use std::path::Path;

/// Check substrate validity.
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
    match validator.validate_packages(&[root]) {
        Ok(report) => {
            let n_failures = report.summary.failed;
            let status = if n_failures == 0 {
                InvariantStatus::Pass
            } else {
                InvariantStatus::Fail
            };
            let detail = if n_failures == 0 {
                None
            } else {
                let msgs: Vec<String> = report
                    .validated
                    .iter()
                    .filter(|p| !p.ok)
                    .flat_map(|p| p.errors.iter().map(move |e| format!("{}: {}", p.path, e)))
                    .collect();
                Some(msgs.join("; "))
            };
            InvariantVerdict {
                id: InvariantId::SubstrateValidity,
                status,
                detail,
                n_inspected: 1,
                n_violations: n_failures,
            }
        }
        Err(e) => InvariantVerdict {
            id: InvariantId::SubstrateValidity,
            status: InvariantStatus::Unverified,
            detail: Some(format!("validator error: {}", e)),
            n_inspected: 1,
            n_violations: 0,
        },
    }
}
