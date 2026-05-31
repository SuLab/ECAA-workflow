//! Invariant 3: evidence-coverage.
//! Every output produced by the execution graph is either referenced
//! as Evidence (`claim-verification.json::verdicts[].supported_by`)
//! or explicitly marked unused (an `output_unused` assumption).
//!
//! Outputs are derived from the Evidence (E) graph (`proofs.jsonl`),
//! which the emitter populates with `produces` / `computed_from`
//! edges. The harness `validation-reports.jsonl` rows are obligation
//! outcomes (`{task_id, obligation_id, outcome}`) and carry no
//! `outputs` field, so they cannot be the source here.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Strip any `#fragment` suffix so evidence references resolve against
/// the bare output identifier.
fn strip_fragment(s: &str) -> String {
    s.split('#').next().unwrap_or(s).to_string()
}

/// Check evidence coverage.
pub fn check_evidence_coverage(pkg: &LoadedPackage) -> InvariantVerdict {
    let outputs: Vec<String> = pkg
        .proofs
        .iter()
        .filter_map(|p| {
            p.get("computed_from")
                .or_else(|| p.get("produces"))
                .and_then(|v| v.as_str())
                .map(strip_fragment)
        })
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
                .filter_map(|v| v.as_str().map(strip_fragment))
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
    // Spec §3: the default verdict on a violation (or on absent claim
    // graph) is `Warn`, never `Fail`. Outputs that exist but are not yet
    // referenced are a soft signal (e.g. a freshly emitted, un-executed
    // package has no claims yet), not a hard-block condition.
    let status = if pkg.claims.is_none() {
        InvariantStatus::Warn
    } else if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Warn
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
