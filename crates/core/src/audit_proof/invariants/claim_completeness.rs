//! Invariant 1: claim-completeness.
//! Every adjudicated Claim in claim-verification.json must retain the evidence
//! route and explanation appropriate to its verdict.

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
    let has_refs = |v: &serde_json::Value, field: &str| {
        v.get(field)
            .and_then(|value| value.as_array())
            .is_some_and(|refs| !refs.is_empty())
    };
    let has_detail = |v: &serde_json::Value| {
        v.get("verdict_detail")
            .and_then(|value| value.as_str())
            .is_some_and(|detail| !detail.trim().is_empty())
    };
    let mut violators = Vec::new();
    for v in verdicts {
        let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("");
        let complete = match status {
            "verified" => has_refs(v, "supported_by") && has_refs(v, "checked_against"),
            "mismatch" | "contradicted" => {
                has_refs(v, "supported_by")
                    && has_refs(v, "checked_against")
                    && has_refs(v, "contradicts")
                    && has_detail(v)
            }
            // A loaded table can still yield no resolvable quantity. Its
            // reason is mandatory; `checked_against` is retained when a table
            // was opened, while a missing route can legitimately have none.
            "unverifiable" => has_detail(v),
            // No adjudication ran, so only the explicit reason is mandatory.
            // A declared route, when present, lives in `attempted_sources`.
            "pending" => has_detail(v),
            // The verifier loaded evidence but asks for human review.
            "suspicious" => {
                has_refs(v, "supported_by") && has_refs(v, "checked_against") && has_detail(v)
            }
            _ => false,
        };
        if !complete {
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
                "{} claim(s) missing verdict-appropriate evidence links or explanation: {}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn package(verdicts: Vec<serde_json::Value>) -> LoadedPackage {
        LoadedPackage {
            claims: Some(json!({"verdicts": verdicts})),
            ..LoadedPackage::default()
        }
    }

    #[test]
    fn verdict_specific_evidence_routes_are_complete() {
        let pkg = package(vec![
            json!({
                "claim_id": "verified",
                "status": "verified",
                "supported_by": ["runtime/outputs/de/de.tsv"],
                "checked_against": ["runtime/outputs/de/de.tsv"]
            }),
            json!({
                "claim_id": "mismatch",
                "status": "mismatch",
                "supported_by": ["runtime/outputs/de/de.tsv"],
                "checked_against": ["runtime/outputs/de/de.tsv"],
                "contradicts": ["runtime/outputs/de/de.tsv"],
                "verdict_detail": "claimed 2.0, observed -2.0"
            }),
            json!({
                "claim_id": "unverifiable",
                "status": "unverifiable",
                "checked_against": ["runtime/outputs/de/de.tsv"],
                "verdict_detail": "measurement column absent"
            }),
            json!({
                "claim_id": "pending",
                "status": "pending",
                "attempted_sources": ["runtime/outputs/de/missing.tsv"],
                "verdict_detail": "source table was not produced"
            }),
            json!({
                "claim_id": "suspicious",
                "status": "suspicious",
                "supported_by": ["runtime/outputs/de/de.tsv"],
                "checked_against": ["runtime/outputs/de/de.tsv"],
                "verdict_detail": "entity absent from a compatible table"
            }),
        ]);
        let verdict = check_claim_completeness(&pkg);
        assert_eq!(verdict.status, InvariantStatus::Pass, "{verdict:?}");
        assert_eq!(verdict.n_violations, 0);
    }

    #[test]
    fn mismatch_without_comparison_route_is_reported() {
        let pkg = package(vec![json!({
            "claim_id": "mismatch",
            "status": "mismatch",
            "supported_by": [],
            "checked_against": [],
            "contradicts": [],
            "verdict_detail": "sign mismatch"
        })]);
        let verdict = check_claim_completeness(&pkg);
        assert_eq!(verdict.status, InvariantStatus::Warn, "{verdict:?}");
        assert_eq!(verdict.n_violations, 1);
        assert!(verdict
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("mismatch")));
    }
}
