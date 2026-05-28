//! Invariant 4: equivalence-failure.
//! Every verifier-decisions.jsonl `prove failed` must have a
//! corresponding `unprovable_edge` or `policy_exception` assumption.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Check equivalence failure.
pub fn check_equivalence_failure(pkg: &LoadedPackage) -> InvariantVerdict {
    let failed_edges: Vec<String> = pkg
        .verifier_decisions
        .iter()
        .filter(|v| {
            v.get("event").and_then(|s| s.as_str()) == Some("prove")
                && v.get("outcome").and_then(|s| s.as_str()) == Some("failed")
        })
        .filter_map(|v| v.get("edge_id").and_then(|s| s.as_str()).map(String::from))
        .collect();
    if failed_edges.is_empty() {
        return InvariantVerdict {
            id: InvariantId::EquivalenceFailure,
            status: InvariantStatus::Pass,
            detail: None,
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let ack: BTreeSet<String> = pkg
        .assumptions
        .iter()
        .filter(|a| {
            matches!(
                a.get("kind").and_then(|s| s.as_str()),
                Some("unprovable_edge" | "policy_exception")
            )
        })
        .filter_map(|a| a.get("edge_id").and_then(|s| s.as_str()).map(String::from))
        .collect();
    let mut violators = Vec::new();
    for e in &failed_edges {
        if !ack.contains(e) {
            violators.push(e.clone());
        }
    }
    let n_inspected = failed_edges.len();
    let n_violations = violators.len();
    let status = if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Fail
    };
    let detail = if n_violations == 0 {
        None
    } else {
        Some(format!(
            "{} prove-failed edge(s) without ack: {}",
            n_violations,
            violators.join(", ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::EquivalenceFailure,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
