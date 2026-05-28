//! Invariant 5: cross-graph-integrity.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Check cross graph integrity.
pub fn check_cross_graph_integrity(pkg: &LoadedPackage) -> InvariantVerdict {
    let known_outputs: BTreeSet<String> = pkg
        .validation_reports
        .iter()
        .filter_map(|r| r.get("outputs").and_then(|v| v.as_array()))
        .flatten()
        .filter_map(|v| v.as_str().map(String::from))
        .collect();
    let known_edges: BTreeSet<String> = pkg
        .proofs
        .iter()
        .filter_map(|p| p.get("edge_id").and_then(|s| s.as_str()).map(String::from))
        .collect();
    let mut violators = Vec::new();
    let mut n_inspected = 0;
    if let Some(claims) = &pkg.claims {
        if let Some(verdicts) = claims.get("verdicts").and_then(|v| v.as_array()) {
            for v in verdicts {
                if let Some(refs) = v.get("supported_by").and_then(|s| s.as_array()) {
                    for r in refs {
                        if let Some(s) = r.as_str() {
                            n_inspected += 1;
                            let path = s.split('#').next().unwrap_or(s);
                            if !known_outputs.contains(path) {
                                violators.push(format!("supported_by: {}", s));
                            }
                        }
                    }
                }
            }
        }
    }
    for a in &pkg.assumptions {
        if let Some(eid) = a.get("edge_id").and_then(|s| s.as_str()) {
            n_inspected += 1;
            if !known_edges.contains(eid) {
                violators.push(format!("assumption edge_id: {}", eid));
            }
        }
    }
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
            "{} dangling cross-graph reference(s): {}",
            n_violations,
            violators.join("; ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::CrossGraphIntegrity,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
