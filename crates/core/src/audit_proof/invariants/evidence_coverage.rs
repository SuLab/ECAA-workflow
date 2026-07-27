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
//!
//! # The denominator is claim-ELIGIBLE outputs
//!
//! The ∀ ranges over the outputs a narrative claim could plausibly cite, not
//! over every byte the run wrote. Each task also emits execution machinery —
//! generated `scripts/`, `env.lock`/`env.explicit.lock`, `task-spec.json`,
//! `agent-code.json`, the literature `evidence/` snapshot store, logs and state
//! patches — that is required for re-execution and correctly registered in the
//! RO-Crate, but that no claim will ever reference. Counting it made a real
//! deposit report 295 outputs inspected and 293 "uncovered", burying the two
//! genuine gaps.
//!
//! The filter is applied HERE rather than inside
//! [`crate::audit_proof::output_source::analytical_outputs`] on purpose: that
//! derivation is shared with the V sub-graph projection and Invariant 5
//! (`cross_graph_integrity`), which must keep ranging over the package's FULL
//! output set (a `V:` node exists for every registered artifact, and the two
//! assign ids by enumeration order — narrowing the shared source would silently
//! delete V nodes and shift those ids). `analytical_outputs` therefore tags each
//! output with an [`crate::audit_proof::output_source::OutputRole`] and this
//! invariant partitions on it, so the excluded objects stay classified and
//! counted instead of vanishing.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::output_source::{analytical_outputs, same_task_basename_match, OutputRole};
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// Strip any `#fragment` suffix so evidence references resolve against
/// the bare output identifier.
fn strip_fragment(s: &str) -> String {
    s.split('#').next().unwrap_or(s).to_string()
}

/// The claim-eligible / administrative partition of a package's analytical
/// outputs. `administrative` is retained (not discarded) so the verdict can
/// report how much of the crate was excluded from the denominator and why.
pub struct CoverageScope {
    /// Outputs a claim could cite — the Invariant-3 denominator.
    pub claim_eligible: Vec<String>,
    /// Execution / provenance machinery, excluded from the denominator.
    pub administrative: Vec<String>,
}

/// Partition a package's analytical outputs into the claim-eligible
/// denominator and the administrative remainder. Both halves keep the
/// deterministic path ordering `analytical_outputs` produces.
pub fn coverage_scope(pkg: &LoadedPackage) -> CoverageScope {
    let mut claim_eligible = Vec::new();
    let mut administrative = Vec::new();
    for o in analytical_outputs(&pkg.output_entities, &pkg.proofs) {
        match o.role {
            OutputRole::ClaimEligible => claim_eligible.push(o.path),
            OutputRole::Administrative => administrative.push(o.path),
        }
    }
    CoverageScope {
        claim_eligible,
        administrative,
    }
}

/// Suffix appended to every detail string so the excluded objects are visible
/// in the report rather than silently missing from the denominator.
fn administrative_note(n_administrative: usize) -> String {
    if n_administrative == 0 {
        String::new()
    } else {
        format!(
            " ({n_administrative} administrative output(s) — generated scripts, environment \
             locks, task specs, agent telemetry, evidence snapshots — excluded from the \
             denominator)"
        )
    }
}

/// Check evidence coverage.
pub fn check_evidence_coverage(pkg: &LoadedPackage) -> InvariantVerdict {
    let CoverageScope {
        claim_eligible: outputs,
        administrative,
    } = coverage_scope(pkg);
    let n_administrative = administrative.len();
    if outputs.is_empty() {
        // ∀-over-empty-set is vacuous: when the package declares no
        // claim-eligible analytical outputs to range over (no figure
        // obligations, no produced result files — only machinery, or nothing at
        // all), there is nothing to certify. Report Unverified — not a coerced
        // Pass/Warn — so the preprint never claims coverage over an empty set.
        return InvariantVerdict {
            id: InvariantId::EvidenceCoverage,
            status: InvariantStatus::Unverified,
            detail: Some(format!(
                "no claim-eligible analytical outputs declared{}",
                administrative_note(n_administrative)
            )),
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
    // The administrative note rides on every detail (including the clean-pass
    // one, which previously carried `None`) so a reader can always reconcile the
    // denominator against the crate's full output count.
    let admin = administrative_note(n_administrative);
    let detail = if n_violations == 0 && pkg.claims.is_some() {
        (n_administrative > 0).then(|| {
            format!("{n_inspected} claim-eligible output(s) all referenced or marked unused{admin}")
        })
    } else if pkg.claims.is_none() {
        Some(format!(
            "no claim-verification.json; {n_inspected} outputs uncovered by default{admin}"
        ))
    } else {
        Some(format!(
            "{} output(s) not referenced and not marked unused: {}{}",
            n_violations,
            violators.join(", "),
            admin
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn file_entity(id: &str) -> serde_json::Value {
        json!({"@id": id, "@type": ["File", "Dataset"]})
    }

    /// The ∀ must range over claim-eligible outputs ONLY. A task that produced
    /// two result tables alongside the usual five machinery files must report
    /// two inspected and one uncovered — not seven inspected and six uncovered,
    /// the shape that made a real deposit read as 293/295 uncovered.
    #[test]
    fn evidence_coverage_denominator_is_claim_eligible_only() {
        let pkg = LoadedPackage {
            output_entities: vec![
                // 2 claim-eligible result tables.
                file_entity("runtime/outputs/de/de_results.tsv"),
                file_entity("runtime/outputs/de/normalized_counts.tsv"),
                // 5 administrative files.
                file_entity("runtime/outputs/de/scripts/01_deseq2_de.R"),
                file_entity("runtime/outputs/de/env.lock"),
                file_entity("runtime/outputs/de/task-spec.json"),
                file_entity("runtime/outputs/de/agent-code.json"),
                file_entity("runtime/outputs/de/evidence/manifest.json"),
            ],
            claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
                "supported_by":["runtime/outputs/de/de_results.tsv"]}]})),
            ..Default::default()
        };
        let v = check_evidence_coverage(&pkg);
        assert_eq!(
            v.n_inspected, 2,
            "only the 2 claim-eligible tables belong in the denominator: {:?}",
            v.detail
        );
        assert_eq!(
            v.n_violations, 1,
            "only the unreferenced table is a violation: {:?}",
            v.detail
        );
        assert_eq!(
            v.status,
            InvariantStatus::Warn,
            "one uncovered output is a Warn, never a Fail"
        );
        let detail = v.detail.expect("a violation must carry a detail");
        assert!(
            detail.contains("runtime/outputs/de/normalized_counts.tsv"),
            "the uncovered table must be named: {detail}"
        );
        assert!(
            !detail.contains("env.lock") && !detail.contains("scripts/"),
            "machinery must not be listed as uncovered: {detail}"
        );
        assert!(
            detail.contains("5 administrative output(s)"),
            "the excluded count must be surfaced, not silently dropped: {detail}"
        );
    }

    /// A package whose registered outputs are ALL machinery has nothing
    /// claim-bearing to range over: Unverified, and the excluded count still
    /// reported so the emptiness is explained rather than mysterious.
    #[test]
    fn evidence_coverage_all_administrative_is_unverified() {
        let pkg = LoadedPackage {
            output_entities: vec![
                file_entity("runtime/outputs/de/scripts/01_deseq2_de.R"),
                file_entity("runtime/outputs/de/env.lock"),
            ],
            claims: Some(json!({"verdicts": []})),
            ..Default::default()
        };
        let v = check_evidence_coverage(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Unverified,
            "machinery-only package cannot certify coverage"
        );
        assert_eq!(v.n_inspected, 0, "empty denominator");
        let detail = v.detail.expect("Unverified must explain itself");
        assert!(
            detail.contains("2 administrative output(s)"),
            "the excluded count must be surfaced: {detail}"
        );
    }

    /// Full coverage over the claim-eligible set is a Pass even though the
    /// crate also registers machinery — and the Pass says so.
    #[test]
    fn evidence_coverage_passes_when_every_eligible_output_referenced() {
        let pkg = LoadedPackage {
            output_entities: vec![
                file_entity("runtime/outputs/de/de_results.tsv"),
                file_entity("runtime/outputs/de/env.lock"),
                file_entity("runtime/outputs/de/scripts/01_deseq2_de.R"),
            ],
            claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
                "supported_by":["runtime/outputs/de/de_results.tsv"]}]})),
            ..Default::default()
        };
        let v = check_evidence_coverage(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Pass,
            "machinery must not block a Pass: {:?}",
            v.detail
        );
        assert_eq!(v.n_inspected, 1, "one claim-eligible output");
        assert_eq!(v.n_violations, 0, "it is referenced");
        let detail = v.detail.expect("a Pass alongside machinery must report it");
        assert!(
            detail.contains("2 administrative output(s)"),
            "the excluded count must be surfaced on a Pass too: {detail}"
        );
    }
}
