//! Invariant 1: claim-completeness.
//! Every Claim in claim-verification.json must have non-empty
//! `supported_by` OR be `status: pending`.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};

/// Check claim completeness.
pub fn check_claim_completeness(pkg: &LoadedPackage) -> InvariantVerdict {
    let claims = match &pkg.claims {
        Some(v) => v,
        None => {
            return InvariantVerdict {
                id: InvariantId::ClaimCompleteness,
                status: InvariantStatus::Unverified,
                detail: Some("runtime/claim-verification.json absent".into()),
                n_inspected: 0,
                n_violations: 0,
            }
        }
    };
    let verdicts = claims.get("verdicts").and_then(|v| v.as_array());
    let verdicts = match verdicts {
        Some(a) => a,
        None => {
            return InvariantVerdict {
                id: InvariantId::ClaimCompleteness,
                status: InvariantStatus::Unverified,
                detail: Some("claims file has no `verdicts` array".into()),
                n_inspected: 0,
                n_violations: 0,
            }
        }
    };
    let mut violators = Vec::new();
    for v in verdicts {
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
        if status == "pending" {
            continue;
        }
        let support = v.get("supported_by").and_then(|s| s.as_array());
        let supported = support.map(|a| !a.is_empty()).unwrap_or(false);
        if !supported {
            let id = v
                .get("claim_id")
                .and_then(|s| s.as_str())
                .unwrap_or("<unknown>");
            violators.push(id.to_string());
        }
    }
    let n_inspected = verdicts.len();
    let n_violations = violators.len();
    let status = if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Warn
    };
    let detail = if n_violations == 0 {
        None
    } else {
        Some(format!(
            "{} claim(s) with empty supported_by and not pending: {}",
            n_violations,
            violators.join(", ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::ClaimCompleteness,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
