//! Invariant 3: evidence-coverage.
//! Every output node from an Execution task must be referenced
//! in claim-verification.json::verdicts[].supported_by OR carry
//! an output_unused assumption.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Check evidence coverage.
pub fn check_evidence_coverage(pkg: &LoadedPackage) -> InvariantVerdict {
    let outputs: Vec<String> = pkg
        .validation_reports
        .iter()
        .filter_map(|r| r.get("outputs").and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    if outputs.is_empty() {
        return InvariantVerdict {
            id: InvariantId::EvidenceCoverage,
            status: InvariantStatus::Unverified,
            detail: Some("no execution outputs declared".into()),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let supported: BTreeSet<String> = pkg
        .claims
        .as_ref()
        .and_then(|c| c.get("verdicts").and_then(|v| v.as_array()))
        .map(|verdicts| {
            verdicts
                .iter()
                .filter_map(|v| v.get("supported_by").and_then(|s| s.as_array()))
                .flatten()
                .filter_map(|v| {
                    v.as_str().map(|s| {
                        // Strip any `#fragment` suffix to match output paths
                        s.split('#').next().unwrap_or(s).to_string()
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let unused: BTreeSet<String> = pkg
        .assumptions
        .iter()
        .filter(|a| a.get("kind").and_then(|s| s.as_str()) == Some("output_unused"))
        .filter_map(|a| a.get("detail").and_then(|s| s.as_str()).map(String::from))
        .collect();
    let mut violators = Vec::new();
    for o in &outputs {
        if !supported.contains(o) && !unused.contains(o) {
            violators.push(o.clone());
        }
    }
    let n_inspected = outputs.len();
    let n_violations = violators.len();
    let status = if pkg.claims.is_none() {
        InvariantStatus::Warn
    } else if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Fail
    };
    let detail = if n_violations == 0 && pkg.claims.is_some() {
        None
    } else if pkg.claims.is_none() {
        Some(format!(
            "no claim-verification.json; {} outputs uncovered by default",
            n_inspected
        ))
    } else {
        Some(format!(
            "{} output(s) not referenced and not marked unused: {}",
            n_violations,
            violators.join(", ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::EvidenceCoverage,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
