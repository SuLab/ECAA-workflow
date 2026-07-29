//! Invariant 3: evidence-coverage.
//! Every output explicitly declared as claim evidence is either referenced by
//! an adjudication (`supported_by`, `checked_against`, or `contradicts`) or
//! explicitly marked unused (an `output_unused` assumption).
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
//! # The denominator is declared claim evidence
//!
//! The universal quantifier ranges over artifacts selected by
//! `result_schema.artifact` or `report_schemas.*.artifact`, plus artifacts that
//! a claim actually references. It does not range over every scientific file
//! merely because the file was retained. A workflow can retain normalized
//! matrices, alternate table views, summaries, copied inputs, plotting data,
//! figures, validation reports, and execution machinery without promising that
//! each file supports a narrative claim.
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

/// The accountability partition of a package's analytical outputs.
pub struct CoverageScope {
    /// Explicitly declared or actually referenced evidence. This is the
    /// Invariant-3 denominator.
    pub claim_evidence: Vec<String>,
    /// Other retained analytical results.
    pub analytical_results: Vec<String>,
    /// Human-facing reports and rendered figures.
    pub presentation: Vec<String>,
    /// Derived alternate views, summaries, indexes, and plotting data.
    pub intermediate: Vec<String>,
    /// Validator outputs.
    pub validation: Vec<String>,
    /// Copied inputs retained for inspection or replay.
    pub retained_inputs: Vec<String>,
    /// Outputs explicitly marked as superseded.
    pub superseded: Vec<String>,
    /// Execution and provenance machinery.
    pub administrative: Vec<String>,
}

/// Return every artifact reference recorded by a claim adjudication.
pub fn claim_references(pkg: &LoadedPackage) -> BTreeSet<String> {
    pkg.claims
        .as_ref()
        .and_then(|claims| claims.get("verdicts").and_then(|value| value.as_array()))
        .map(|verdicts| {
            verdicts
                .iter()
                .flat_map(|verdict| {
                    ["supported_by", "checked_against", "contradicts"]
                        .into_iter()
                        .filter_map(|field| verdict.get(field).and_then(|refs| refs.as_array()))
                        .flatten()
                })
                .filter_map(|value| value.as_str().map(strip_fragment))
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn reference_resolves_output(output: &str, reference: &str) -> bool {
    output == reference || same_task_basename_match(output, reference)
}

pub(crate) fn declaration_selects_output(output: &str, declaration: &str) -> bool {
    let declaration = declaration.split('#').next().unwrap_or(declaration);
    if declaration.contains('/') {
        reference_resolves_output(output, declaration)
    } else {
        output.rsplit('/').next() == Some(declaration)
    }
}

/// Partition all outputs without dropping any retained file. Explicit
/// declarations and actual claim links take precedence over the path-derived
/// supporting role.
pub fn coverage_scope(pkg: &LoadedPackage) -> CoverageScope {
    let referenced = claim_references(pkg);
    let mut scope = CoverageScope {
        claim_evidence: Vec::new(),
        analytical_results: Vec::new(),
        presentation: Vec::new(),
        intermediate: Vec::new(),
        validation: Vec::new(),
        retained_inputs: Vec::new(),
        superseded: Vec::new(),
        administrative: Vec::new(),
    };
    for o in analytical_outputs(&pkg.output_entities, &pkg.proofs) {
        let selected = referenced
            .iter()
            .any(|reference| reference_resolves_output(&o.path, reference))
            || pkg
                .declared_claim_evidence
                .iter()
                .any(|declaration| declaration_selects_output(&o.path, declaration));
        if selected {
            scope.claim_evidence.push(o.path);
            continue;
        }
        match o.role {
            OutputRole::ClaimEligible => scope.analytical_results.push(o.path),
            OutputRole::Presentation => scope.presentation.push(o.path),
            OutputRole::Intermediate => scope.intermediate.push(o.path),
            OutputRole::Validation => scope.validation.push(o.path),
            OutputRole::RetainedInput => scope.retained_inputs.push(o.path),
            OutputRole::Superseded => scope.superseded.push(o.path),
            OutputRole::Administrative => scope.administrative.push(o.path),
        }
    }
    scope
}

/// Suffix appended to every detail string so all retained output roles remain
/// visible even though only declared evidence belongs in the denominator.
fn accountability_note(scope: &CoverageScope) -> String {
    let counts = [
        ("other analytical", scope.analytical_results.len()),
        ("presentation", scope.presentation.len()),
        ("intermediate", scope.intermediate.len()),
        ("validation", scope.validation.len()),
        ("retained input", scope.retained_inputs.len()),
        ("superseded", scope.superseded.len()),
        ("administrative", scope.administrative.len()),
    ];
    if counts.iter().all(|(_, count)| *count == 0) {
        String::new()
    } else {
        let summary = counts
            .into_iter()
            .map(|(role, count)| format!("{role}={count}"))
            .collect::<Vec<_>>()
            .join(", ");
        format!(" (retained output roles outside denominator: {summary})")
    }
}

/// Check evidence coverage.
pub fn check_evidence_coverage(pkg: &LoadedPackage) -> InvariantVerdict {
    let scope = coverage_scope(pkg);
    let outputs = &scope.claim_evidence;
    if outputs.is_empty() {
        // A universal quantifier over an empty set is vacuous. When the package
        // declares no evidence artifacts and no claim resolves to an output,
        // there is nothing to certify.
        return InvariantVerdict {
            id: InvariantId::EvidenceCoverage,
            status: InvariantStatus::Unverified,
            detail: Some(format!(
                "no claim-evidence outputs declared or referenced{}",
                accountability_note(&scope)
            )),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let referenced = claim_references(pkg);
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
    for o in outputs {
        let covered =
            referenced.contains(o) || referenced.iter().any(|r| same_task_basename_match(o, r));
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
    let roles = accountability_note(&scope);
    let detail = if n_violations == 0 && pkg.claims.is_some() {
        Some(format!(
            "{n_inspected} declared claim-evidence output(s) all referenced or marked unused{roles}"
        ))
    } else if pkg.claims.is_none() {
        Some(format!(
            "no claim-verification.json; {n_inspected} declared evidence outputs uncovered by default{roles}"
        ))
    } else {
        Some(format!(
            "{} output(s) not referenced and not marked unused: {}{}",
            n_violations,
            violators.join(", "),
            roles
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
            declared_claim_evidence: BTreeSet::from([
                "de_results.tsv".to_string(),
                "normalized_counts.tsv".to_string(),
            ]),
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
            detail.contains("administrative=5"),
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
            detail.contains("administrative=2"),
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
            declared_claim_evidence: BTreeSet::from(["de_results.tsv".to_string()]),
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
            detail.contains("administrative=2"),
            "the excluded count must be surfaced on a Pass too: {detail}"
        );
    }

    #[test]
    fn comparison_and_contradiction_links_account_for_used_evidence() {
        let pkg = LoadedPackage {
            output_entities: vec![file_entity("runtime/outputs/de/de_results.tsv")],
            claims: Some(json!({"verdicts":[{
                "claim_id":"c-1",
                "status":"mismatch",
                "supported_by":[],
                "checked_against":["runtime/outputs/de/de_results.tsv"],
                "contradicts":["runtime/outputs/de/de_results.tsv"]
            }]})),
            ..Default::default()
        };
        let verdict = check_evidence_coverage(&pkg);
        assert_eq!(verdict.status, InvariantStatus::Pass, "{verdict:?}");
        assert_eq!(verdict.n_inspected, 1);
        assert_eq!(verdict.n_violations, 0);
    }
}
