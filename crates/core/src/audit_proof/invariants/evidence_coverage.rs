//! Invariant 3: evidence-coverage.
//! Every analytical output produced by the analysis is either referenced
//! as Evidence (`claim-verification.json::verdicts[].supported_by`)
//! or explicitly marked unused (an `output_unused` assumption).
//!
//! Outputs are derived from the SAME real-output source the Evidence (V)
//! sub-graph projection uses — the RO-Crate `@graph` output entities (figure
//! obligations / produced `runtime/outputs/` artifacts), plus any real-path
//! `computed_from`/`produces` proofs row — via
//! [`crate::audit_proof::output_source::analytical_outputs`]. Reader and writer
//! therefore agree (closing the D.5.1 key-mismatch where this reader keyed on
//! `proofs.jsonl::computed_from`, a field the production conversation writer
//! never emits). The harness `validation-reports.jsonl` rows are obligation
//! outcomes (`{task_id, obligation_id, outcome}`) and carry no `outputs` field,
//! so they cannot be the source here.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::output_source::{analytical_outputs, same_task_basename_match};
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Strip any `#fragment` suffix so evidence references resolve against
/// the bare output identifier.
fn strip_fragment(s: &str) -> String {
    s.split('#').next().unwrap_or(s).to_string()
}

/// Check evidence coverage.
pub fn check_evidence_coverage(pkg: &LoadedPackage) -> InvariantVerdict {
    let outputs: Vec<String> = analytical_outputs(&pkg.output_entities, &pkg.proofs)
        .into_iter()
        .map(|o| o.path)
        .collect();
    if outputs.is_empty() {
        // ∀-over-empty-set is vacuous: when the package declares no analytical
        // outputs to range over (no figure obligations, no produced files),
        // there is nothing to certify. Report Unverified — not a coerced
        // Pass/Warn — so the preprint never claims coverage over an empty set.
        return InvariantVerdict {
            id: InvariantId::EvidenceCoverage,
            status: InvariantStatus::Unverified,
            detail: Some("no analytical outputs declared".into()),
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
    // A verified claim's `supported_by` is recorded by the runtime verifier as a
    // BASENAME then path-reconstructed by `claim_sink::evidence_ref_for`; a nested
    // table (`…/<task>/tables/de.tsv`) yields a reconstructed path that differs
    // from the registered output `@id` even though the SAME basename is
    // referenced UNDER THE SAME TASK. An output counts as covered if a claim ref
    // names it exactly OR resolves to it via `same_task_basename_match` (the
    // intra-task nested-table gap) — never via a cross-task basename collision,
    // so an output referenced only by a wrong-directory ref still counts as
    // uncovered. Inv 5 (`cross_graph_integrity`) applies the identical rule.
    let unused: BTreeSet<String> = pkg
        .assumptions
        .iter()
        .filter(|a| a.get("kind").and_then(|s| s.as_str()) == Some("output_unused"))
        .filter_map(|a| a.get("detail").and_then(|s| s.as_str()).map(String::from))
        .collect();
    let mut violators = Vec::new();
    for o in &outputs {
        let covered = supported.contains(o)
            || supported.iter().any(|r| same_task_basename_match(o, r));
        if !covered && !unused.contains(o) {
            violators.push(o.clone());
        }
    }
    let n_inspected = outputs.len();
    let n_violations = violators.len();
    // Spec §3: the default verdict on a violation (or on absent claim
    // graph) is `Warn`, never `Fail`. Outputs that exist but are not yet
    // referenced are a soft signal (e.g. a freshly emitted, un-executed
    // package has declared figure obligations but no claims yet), not a
    // hard-block condition.
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
