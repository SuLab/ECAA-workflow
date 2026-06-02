//! Invariant 1: claim-completeness.
//! Every Claim in claim-verification.json must have non-empty
//! `supported_by` OR be `status: pending`.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};

/// Check claim completeness.
pub fn check_claim_completeness(pkg: &LoadedPackage) -> InvariantVerdict {
    if pkg.claims_tampered {
        return InvariantVerdict {
            id: InvariantId::ClaimCompleteness,
            status: InvariantStatus::Fail,
            detail: Some(
                "claim-verification sink failed HMAC verification (tampered or unauthorized writer)"
                    .into(),
            ),
            n_inspected: 0,
            n_violations: 1,
        };
    }
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
    // Recall floor (folded into Inv 1, NOT a 7th invariant): when the
    // signed sink carries a `coverage` block, every Required entry must be
    // Addressed. Absent/Unverifiable Required entries are recall gaps that
    // FAIL the invariant — saying less is no longer the cheapest clean run.
    // The coverage block is structured-claims-only (deterministic), so the
    // predicate stays free of regex/narrative heuristic input.
    let mut coverage_gaps: usize = 0;
    if let Some(cov) = claims.get("coverage") {
        let unverifiable = cov
            .get("required_unverifiable")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        let absent = cov
            .get("required_absent")
            .and_then(|n| n.as_u64())
            .unwrap_or(0);
        coverage_gaps = (unverifiable + absent) as usize;
    }

    let n_inspected = verdicts.len();
    let support_violations = violators.len();
    let n_violations = support_violations + coverage_gaps;
    // A ∀-over-empty-set is vacuous: no verdicts to inspect AND no `coverage`
    // block (the signed verdict sink that would make the recall floor
    // meaningful) means there is genuinely nothing to certify. Report
    // Unverified rather than a coerced Pass — saying "Pass" over an empty set
    // is the vacuous-pass the preprint must not make.
    if verdicts.is_empty() && claims.get("coverage").is_none() {
        return InvariantVerdict {
            id: InvariantId::ClaimCompleteness,
            status: InvariantStatus::Unverified,
            detail: Some("no verified claims; signed verdict sink absent".into()),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let status = if coverage_gaps > 0 {
        // A Required recall gap is a hard FAIL (blocking-class), distinct
        // from a soft Warn on an empty supported_by.
        InvariantStatus::Fail
    } else if support_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Warn
    };
    let detail = if n_violations == 0 {
        None
    } else {
        let mut parts = Vec::new();
        if support_violations > 0 {
            parts.push(format!(
                "{} claim(s) with empty supported_by and not pending: {}",
                support_violations,
                violators.join(", ")
            ));
        }
        if coverage_gaps > 0 {
            parts.push(format!(
                "{} required expected-claim(s) absent or unverifiable (recall gap)",
                coverage_gaps
            ));
        }
        Some(parts.join("; "))
    };
    InvariantVerdict {
        id: InvariantId::ClaimCompleteness,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
