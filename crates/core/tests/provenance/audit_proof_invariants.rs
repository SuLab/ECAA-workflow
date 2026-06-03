use ecaa_workflow_core::audit_proof::{
    invariants::claim_completeness::check_claim_completeness, loader::LoadedPackage,
    InvariantStatus,
};
use serde_json::json;

fn fixture_loaded(claims: serde_json::Value) -> LoadedPackage {
    LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![],
        proofs: vec![],
        claims: Some(claims),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    }
}

#[test]
fn claim_completeness_passes_on_fully_supported_claims() {
    let pkg = fixture_loaded(json!({
        "n_checked": 2, "n_verified": 2, "n_mismatch": 0, "n_unverifiable": 0,
        "verdicts": [
            {"claim_id":"c-001","status":"verified","supported_by":["runtime/tables/x.csv#r1"]},
            {"claim_id":"c-002","status":"verified","supported_by":["runtime/tables/x.csv#r2"]}
        ]
    }));
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
    assert_eq!(v.n_inspected, 2);
    assert_eq!(v.n_violations, 0);
}

#[test]
fn claim_completeness_fails_when_support_missing() {
    let pkg = fixture_loaded(json!({
        "n_checked": 2, "n_verified": 1, "n_mismatch": 0, "n_unverifiable": 1,
        "verdicts": [
            {"claim_id":"c-001","status":"verified","supported_by":[]},
            {"claim_id":"c-002","status":"verified","supported_by":["runtime/tables/x.csv#r2"]}
        ]
    }));
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn);
    assert_eq!(v.n_violations, 1);
    assert!(v.detail.unwrap().contains("c-001"));
}

#[test]
fn claim_completeness_passes_when_pending() {
    let pkg = fixture_loaded(json!({
        "n_checked": 1, "n_verified": 0, "n_mismatch": 0, "n_unverifiable": 0,
        "verdicts": [
            {"claim_id":"c-001","status":"pending","supported_by":[]}
        ]
    }));
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn claim_completeness_unverified_when_verdicts_empty_and_no_coverage() {
    // B1: an empty `verdicts` array with NO `coverage` block is a vacuous
    // ∀-over-empty-set. There is nothing to inspect and no signed verdict
    // sink, so the honest verdict is Unverified — NOT a coerced Pass.
    let pkg = fixture_loaded(json!({
        "n_checked": 0, "n_verified": 0, "n_mismatch": 0, "n_unverifiable": 0,
        "verdicts": []
    }));
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.n_violations, 0);
}

#[test]
fn claim_completeness_fails_when_coverage_required_absent() {
    // F5 recall-gate preserved: even with an empty `verdicts` array, a
    // `coverage` block reporting a Required entry as absent is a hard Fail.
    let pkg = fixture_loaded(json!({
        "n_checked": 0, "n_verified": 0, "n_mismatch": 0, "n_unverifiable": 0,
        "verdicts": [],
        "coverage": {"required_absent": 1, "required_unverifiable": 0}
    }));
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
    assert_eq!(v.n_violations, 1);
    assert!(v.detail.unwrap().contains("recall gap"));
}

#[test]
fn claim_completeness_unverified_when_no_claim_file() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![],
        proofs: vec![],
        claims: None,
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_claim_completeness(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

use ecaa_workflow_core::audit_proof::invariants::decision_justification::check_decision_justification;

fn fixture_with_decisions(decisions: Vec<serde_json::Value>) -> LoadedPackage {
    LoadedPackage {
        intake: vec![],
        decisions,
        validation_reports: vec![],
        proofs: vec![],
        claims: None,
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    }
}

#[test]
fn decision_justification_inspects_nested_method_choice_records() {
    // Real on-disk shape: the discriminator (`kind`) is nested under
    // `decision`, not at the top level.
    let pkg = fixture_with_decisions(vec![json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "session_id": "s-1",
        "decision": {"kind":"set_intake_method","stage":"differential_expression",
                     "method_prose":"DESeq2 chosen per protocol; meets 30-char minimum."},
        "actor": "sme"
    })]);
    let v = check_decision_justification(&pkg);
    assert_eq!(
        v.n_inspected, 1,
        "must count nested set_intake_method records"
    );
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn decision_justification_inspects_amend_stage_records() {
    // amend_stage is the second method-choice variant.
    let pkg = fixture_with_decisions(vec![json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "session_id": "s-1",
        "decision": {"kind":"amend_stage","stage":"integration",
                     "method_prose":"Switched batch correction to Harmony per reviewer request."},
        "actor": "sme"
    })]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.n_inspected, 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn decision_justification_warns_on_short_method_prose() {
    let pkg = fixture_with_decisions(vec![json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "session_id": "s-1",
        "decision": {"kind":"amend_stage","stage":"integration","method_prose":"use Harmony"},
        "actor": "sme"
    })]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.n_inspected, 1);
    assert_eq!(v.status, InvariantStatus::Warn);
    assert_eq!(v.n_violations, 1);
}

#[test]
fn decision_justification_passes_on_long_record_rationale() {
    // method_prose absent/short but record-level rationale is long enough.
    let pkg = fixture_with_decisions(vec![json!({
        "timestamp": "2026-01-01T00:00:00Z",
        "session_id": "s-1",
        "decision": {"kind":"set_intake_method","stage":"de","method_prose":"x"},
        "rationale": "This is a thirty-plus character justification for the method.",
        "actor": "sme"
    })]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.n_inspected, 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn decision_justification_ignores_non_method_decisions() {
    let pkg = fixture_with_decisions(vec![
        json!({"timestamp":"2026-01-01T00:00:00Z","session_id":"s-1",
               "decision":{"kind":"confirm"},"actor":"sme"}),
        json!({"timestamp":"2026-01-01T00:00:00Z","session_id":"s-1",
               "decision":{"kind":"reject"},"actor":"sme"}),
    ]);
    let v = check_decision_justification(&pkg);
    // n_inspected counts only method-choice (set_intake_method / amend_stage).
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

use ecaa_workflow_core::audit_proof::invariants::evidence_coverage::check_evidence_coverage;

fn pkg_with(
    claims: Option<serde_json::Value>,
    proofs: Vec<serde_json::Value>,
    assumptions: Vec<serde_json::Value>,
) -> LoadedPackage {
    LoadedPackage {
        // Real harness shape: obligation outcomes, no `outputs` field.
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        proofs,
        claims,
        assumptions,
        ..Default::default()
    }
}

/// Build a `LoadedPackage` whose analytical outputs come from the RO-Crate
/// `@graph` output entities (the real-output source for V + Inv 3), with the
/// given claims + assumptions. `output_entities` carries one `{@id, @type}`
/// per produced/declared artifact.
fn pkg_with_outputs(
    claims: Option<serde_json::Value>,
    output_entities: Vec<serde_json::Value>,
    assumptions: Vec<serde_json::Value>,
) -> LoadedPackage {
    LoadedPackage {
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        claims,
        assumptions,
        output_entities,
        ..Default::default()
    }
}

#[test]
fn evidence_coverage_ranges_over_rocrate_output_entities() {
    // The real-output source is the RO-Crate `@graph` ImageObject / output
    // File entities — NOT proofs.jsonl dependency edges. A declared figure
    // referenced by a claim's `supported_by` is covered → Pass.
    let pkg = pkg_with_outputs(
        Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/outputs/de/figures/volcano.png"]}]})),
        vec![json!({"@id":"runtime/outputs/de/figures/volcano.png",
            "@type":["File","ImageObject"]})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(
        v.n_inspected, 1,
        "output set must derive from @graph entities"
    );
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_warns_on_unreferenced_rocrate_figure() {
    // A declared figure obligation with empty claims (pre-execution / freshly
    // emitted) is uncovered → Warn (the honest pre-execution signal when there
    // ARE declared output entities to range over).
    let pkg = pkg_with_outputs(
        Some(json!({"verdicts":[]})),
        vec![json!({"@id":"runtime/outputs/de/figures/volcano.png",
            "@type":["File","ImageObject"]})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn);
    assert_eq!(v.n_violations, 1);
}

#[test]
fn evidence_coverage_unverified_when_no_output_entities() {
    // No RO-Crate output entities and no real-path proofs outputs => nothing to
    // range over => Unverified (honest), not a coerced Pass/Warn.
    let pkg = pkg_with_outputs(Some(json!({"verdicts":[]})), vec![], vec![]);
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn evidence_coverage_ignores_bogus_workflow_dependency_edges() {
    // The CLI emit path writes proofs.jsonl rows whose `computed_from` is a DAG
    // dependency NODE name (`workflow:<dep>`), NOT a produced file. These are E
    // dependency edges, not V outputs, and must NOT be counted as outputs.
    let pkg = pkg_with_outputs(
        Some(json!({"verdicts":[]})),
        // No RO-Crate output entities; only a bogus dep edge in proofs.
        vec![],
        vec![],
    );
    let mut pkg = pkg;
    pkg.proofs = vec![json!({"id":"workflow:de","computed_from":"workflow:data_acquisition"})];
    let v = check_evidence_coverage(&pkg);
    assert_eq!(
        v.n_inspected, 0,
        "workflow:* dependency edges are not analytical outputs"
    );
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn evidence_coverage_passes_when_rocrate_output_marked_unused() {
    let pkg = pkg_with_outputs(
        Some(json!({"verdicts":[]})),
        vec![json!({"@id":"runtime/outputs/de/figures/volcano.png",
            "@type":["File","ImageObject"]})],
        vec![json!({"kind":"output_unused","detail":"runtime/outputs/de/figures/volcano.png"})],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_reads_outputs_from_proofs_jsonl() {
    // The output set must derive from proofs (`computed_from`/`produces`),
    // NOT from validation_reports (which carry no `outputs` field).
    let pkg = pkg_with(
        Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de_results.csv#row_TP53"]}]})),
        vec![json!({"id":"workflow:de","type":"WorkflowStep",
                    "computed_from":"runtime/tables/de_results.csv"})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(
        v.n_inspected, 1,
        "output set must derive from proofs, not validation_reports"
    );
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_reads_produces_edge() {
    // `produces` is the alternate edge name accepted by the reader.
    let pkg = pkg_with(
        Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de_results.csv"]}]})),
        vec![json!({"id":"workflow:de","produces":"runtime/tables/de_results.csv"})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.n_inspected, 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_unverified_when_no_proofs() {
    // No proofs => no derivable outputs => Unverified, not Fail.
    let pkg = pkg_with(Some(json!({"verdicts":[]})), vec![], vec![]);
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn evidence_coverage_warns_when_output_orphan() {
    // Spec §3: an uncovered output is `Warn` (default), never `Fail`.
    let pkg = pkg_with(
        Some(json!({"verdicts":[]})),
        vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de_results.csv"})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn);
    assert_eq!(v.n_violations, 1);
}

#[test]
fn evidence_coverage_passes_when_orphan_marked_unused() {
    let pkg = pkg_with(
        Some(json!({"verdicts":[]})),
        vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de_results.csv"})],
        vec![json!({"kind":"output_unused","detail":"runtime/tables/de_results.csv"})],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_warns_when_no_claims_file() {
    let pkg = pkg_with(
        None,
        vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de_results.csv"})],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn);
}

#[test]
fn evidence_coverage_source_is_proofs_not_validation_reports() {
    // Regression guard (F6): outputs MUST derive from proofs.jsonl
    // (computed_from/produces), never from a validation_reports[].outputs
    // field — that field does not exist on real harness rows
    // ({task_id, obligation_id, outcome}). A package whose validation_reports
    // fabricate an `outputs` array must NOT cause it to be inspected.
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![
            json!({"task_id":"de","obligation_id":"o1","outcome":"passed",
            "outputs":["SHOULD_BE_IGNORED.csv"]}),
        ],
        proofs: vec![json!({"id":"workflow:de","type":"WorkflowStep",
            "computed_from":"runtime/tables/de.csv"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de.csv#row_TP53"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_evidence_coverage(&pkg);
    // Exactly the one proofs-derived output is inspected; the fabricated
    // validation_reports `outputs` entry is invisible.
    assert_eq!(v.n_inspected, 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;

/// Build a `LoadedPackage` whose:
///   - `verifier_decisions` holds the compile-time port-unification trace
///     (`event:"prove"` rows) — the ONLY thing Inv 4 still reads from there;
///   - `reexecution` holds the five-class `RerunOutcome` rows (the real
///     `runtime/reexecution.json` shape: `{schema_version, bucket_counts,
///     per_artifact:[{artifact_path, bucket}]}`). Pass `None` for an absent
///     `reexecution.json` (no re-execution performed → Q absent).
fn pkg_with_q(
    verifier: Vec<serde_json::Value>,
    reexecution: Option<serde_json::Value>,
    assumptions: Vec<serde_json::Value>,
) -> LoadedPackage {
    LoadedPackage {
        verifier_decisions: verifier,
        reexecution,
        assumptions,
        ..Default::default()
    }
}

/// Wrap a list of `RerunOutcome` rows into the real `reexecution.json`
/// document shape so the loader-equivalent in-memory `pkg.reexecution`
/// matches what `write_reexecution_sidecar` serializes.
fn reexecution_doc(per_artifact: Vec<serde_json::Value>) -> serde_json::Value {
    json!({
        "schema_version": "0.1",
        "bucket_counts": {},
        "per_artifact": per_artifact,
    })
}

#[test]
fn equivalence_failure_unverified_when_no_reexecution() {
    // Only compile-time prove rows, no RerunOutcomes (re-execution not performed):
    // spec §4 verdict table → Unverified, never a coerced Pass.
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"succeeded","edge_id":"e-1"})],
        None,
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn equivalence_failure_unverified_when_reexecution_present_but_empty() {
    // A present-but-empty `reexecution.json` (the first-emit shape) means no
    // re-execution was performed: Q is empty → Unverified (spec §4).
    let pkg = pkg_with_q(vec![], Some(reexecution_doc(vec![])), vec![]);
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn equivalence_failure_fails_when_failure_unacknowledged() {
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        None,
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn equivalence_failure_acked_prove_failure_no_reexecution_is_unverified() {
    // An acknowledged compile-time prove-failure is NOT a Fail, but with no
    // re-execution the equivalence verdict is Unverified (spec §4).
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        None,
        vec![json!({"kind":"unprovable_edge","edge_id":"e-2"})],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

#[test]
fn equivalence_failure_ack_via_detail_containment_no_reexecution_is_unverified() {
    // Real v0.1 assumptions carry `detail`, not `edge_id`. An ack whose
    // free-text detail mentions the failed edge prevents Fail; with no
    // re-execution the verdict is Unverified (spec §4).
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        None,
        vec![json!({"assumption_id":"a-1","kind":"policy_exception",
                    "detail":"edge e-2 left unproved per reviewer-approved policy exception",
                    "stage_id":"de"})],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

// --- spec §4 predicate over Q.RerunOutcomes, sourced from reexecution.json ---

#[test]
fn equivalence_failure_fails_on_unacked_acknowledged_non_determinism() {
    // Spec §4: ∀ r ∈ Q.RerunOutcomes : r.class ∉ {"failed",
    // "acknowledged_non_determinism"} ∨ ∃ F.Blocker acknowledging r.id.
    // A re-execution that diverged as acknowledged-non-deterministic with NO
    // Blocker is the silent-corruption case the invariant must catch. The
    // five-class typing now lives in reexecution.json::per_artifact[].bucket.
    let pkg = pkg_with_q(
        vec![],
        Some(reexecution_doc(vec![json!({
            "artifact_path": "results/tables/de.tsv",
            "bucket": "acknowledged_non_determinism"
        })])),
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn equivalence_failure_fails_on_unacked_failed_rerun_class() {
    let pkg = pkg_with_q(
        vec![],
        Some(reexecution_doc(vec![json!({
            "artifact_path": "results/tables/de.tsv",
            "bucket": "failed"
        })])),
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn equivalence_failure_passes_when_rerun_divergence_acknowledged() {
    // The acknowledging F.Blocker keys on the artifact_path (the RerunOutcome id).
    let pkg = pkg_with_q(
        vec![],
        Some(reexecution_doc(vec![json!({
            "artifact_path": "results/tables/de.tsv",
            "bucket": "acknowledged_non_determinism"
        })])),
        vec![json!({"kind":"policy_exception","edge_id":"results/tables/de.tsv"})],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn equivalence_failure_ignores_non_divergent_rerun_classes() {
    // byte_identical / semantic_equivalent / unavailable are not in the
    // diverged set and need no acknowledgement. Their presence still means
    // re-execution was performed, so the verdict is Pass (not Unverified).
    let pkg = pkg_with_q(
        vec![],
        Some(reexecution_doc(vec![
            json!({"artifact_path":"a.tsv","bucket":"byte_identical"}),
            json!({"artifact_path":"b.tsv","bucket":"semantic_equivalent"}),
            json!({"artifact_path":"c.tsv","bucket":"unavailable"}),
        ])),
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn equivalence_failure_prove_failed_in_verifier_decisions_still_fails() {
    // The compile-time port-unification trace stays in verifier-decisions.jsonl.
    // An unacknowledged prove/failed row is still a Fail even though no
    // re-execution was performed (the spec's two silent-corruption shapes).
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"edge-x"})],
        Some(reexecution_doc(vec![])),
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn equivalence_failure_ignores_class_rows_in_verifier_decisions() {
    // Class-bearing rows must NO LONGER be read from verifier-decisions.jsonl;
    // a stray `class` row there is part of the compile-time trace and must not
    // be mistaken for a re-execution divergence (regression guard for the source
    // migration). With no reexecution.json and only this stray row → Unverified.
    let pkg = pkg_with_q(vec![json!({"class":"failed","id":"stray-1"})], None, vec![]);
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

use ecaa_workflow_core::audit_proof::invariants::cross_graph_integrity::check_cross_graph_integrity;

#[test]
fn cross_graph_passes_when_all_refs_resolve() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        // known_outputs now derives from proofs `computed_from`.
        proofs: vec![json!({"edge_id":"e-1","from":"counts","to":"de",
                            "computed_from":"runtime/tables/de.csv"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de.csv#row_TP53"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![json!({"kind":"llm_inferred_default","edge_id":"e-1"})],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn cross_graph_resolves_supported_by_against_proofs_outputs() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        proofs: vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de.csv"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de.csv#row_TP53"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert!(v.n_inspected >= 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn cross_graph_fails_when_supported_by_dangling() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        proofs: vec![],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/nonexistent.csv#row_X"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
    assert!(v.n_violations >= 1);
}

#[test]
fn cross_graph_fails_when_assumption_dangling() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![],
        proofs: vec![json!({"edge_id":"e-1"})],
        claims: None,
        verifier_decisions: vec![],
        assumptions: vec![json!({"kind":"x","edge_id":"e-2"})],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn cross_graph_resolves_prefixed_supported_by_into_v() {
    // Spec §5 general form: a prefix-tagged `V:<id>` supported_by reference
    // resolves against the Evidence node-id set (output basename, sanitized).
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![],
        proofs: vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de.csv"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["V:de_csv"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert!(v.n_inspected >= 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn cross_graph_general_resolves_prefixed_ref_against_named_subgraph() {
    // Spec §5 general predicate: a `D:<id>` reference resolves against the
    // Decision node-id set (here a child decision derived from its parent).
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![
            json!({"id":"parent"}),
            json!({"id":"child","prov:wasDerivedFrom":"D:parent"}),
        ],
        validation_reports: vec![],
        proofs: vec![],
        claims: None,
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert!(v.n_inspected >= 1);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn cross_graph_general_fails_on_dangling_prefixed_ref() {
    // A `D:ghost` reference with no matching Decision node dangles → Fail.
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![json!({"id":"child","prov:wasDerivedFrom":"D:ghost"})],
        validation_reports: vec![],
        proofs: vec![],
        claims: None,
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
    assert!(v.n_violations >= 1);
}

#[test]
fn cross_graph_unverified_when_no_references_present() {
    // B1: a freshly emitted package carries no cross-graph references — no
    // claim `supported_by`, no assumption `edge_id`, no prefix-tagged refs.
    // A ∀-over-empty-set is vacuous, so the honest verdict is Unverified, NOT
    // a coerced Pass.
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        // Proofs declare outputs (computed_from) but those are not cross-graph
        // references — nothing inspects them in this invariant.
        proofs: vec![json!({"id":"workflow:de","computed_from":"runtime/tables/de.csv"})],
        claims: Some(json!({"verdicts":[]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![],
        claims_tampered: false,
        reexecution: None,
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.status, InvariantStatus::Unverified);
    assert_eq!(v.n_violations, 0);
}

#[test]
fn cross_graph_resolves_supported_by_against_rocrate_output_entities() {
    // B4b: post-execution / production shape. `proofs.jsonl` is a BARE
    // `EdgeContract` (no `computed_from`/`produces`), while the produced
    // evidence is registered in the RO-Crate `@graph` as an output entity and a
    // verified claim's `supported_by` references its real path. Inv 5 must
    // resolve the C→V reference against the SAME `output_source::analytical_outputs`
    // derivation Inv 3 uses — so both agree (Pass), not contradict.
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        // Bare EdgeContract: producer→consumer with NO computed_from/produces.
        proofs: vec![json!({"edge_id":"e-1","from_node":"counts","to_node":"de"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/outputs/de/figures/volcano.png"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        // The produced figure is carried as an RO-Crate @graph output entity.
        output_entities: vec![json!({"@id":"runtime/outputs/de/figures/volcano.png",
            "@type":["File","ImageObject"]})],
        claims_tampered: false,
        reexecution: None,
    };
    // Inv 5: the C→V reference resolves against the @graph-derived output set.
    let v5 = check_cross_graph_integrity(&pkg);
    assert!(
        v5.n_inspected >= 1,
        "the supported_by ref must be inspected"
    );
    assert_eq!(
        v5.status,
        InvariantStatus::Pass,
        "C→V ref to a real @graph output must resolve (detail: {:?})",
        v5.detail
    );
    // Inv 3: the same output is covered by the same claim — no contradiction.
    let v3 = check_evidence_coverage(&pkg);
    assert_eq!(
        v3.status,
        InvariantStatus::Pass,
        "evidence_coverage must agree the output is covered (detail: {:?})",
        v3.detail
    );
}

#[test]
fn cross_graph_resolves_supported_by_against_real_path_table_output() {
    // Companion shape: a `results/tables/de.csv` Dataset output entity (the
    // claim_sink.rs real-path form) referenced by a claim's `supported_by`.
    // Under the old proofs-keyed V registry this dangled; under output_source
    // it resolves.
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        proofs: vec![json!({"edge_id":"e-1","from_node":"counts","to_node":"de"})],
        claims: Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/outputs/de/de.csv#row_TP53"]}]})),
        verifier_decisions: vec![],
        assumptions: vec![],
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
        output_entities: vec![json!({"@id":"runtime/outputs/de/de.csv",
            "@type":["File","Dataset"]})],
        claims_tampered: false,
        reexecution: None,
    };
    let v5 = check_cross_graph_integrity(&pkg);
    assert!(v5.n_inspected >= 1);
    assert_eq!(
        v5.status,
        InvariantStatus::Pass,
        "C→V ref (with #fragment) to a real Dataset output must resolve (detail: {:?})",
        v5.detail
    );
}

use ecaa_workflow_core::audit_proof::invariants::substrate_validity::check_substrate_validity;
use ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;

#[test]
fn substrate_validity_with_noop_validator_is_unverified_not_pass() {
    // The no-op validator does not actually run a WRROC conformance
    // check, so a present descriptor must yield Unverified — NOT a
    // spurious Pass. A genuine pass requires a real (runcrate-backed)
    // validator on the conformance path.
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("ro-crate-metadata.json"),
        r#"{"@context":"https://w3id.org/ro/crate/1.1/context","@graph":[]}"#,
    )
    .unwrap();
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert_eq!(v.status, InvariantStatus::Unverified);
    assert_eq!(v.n_inspected, 1, "descriptor present ⇒ inspected");
    assert!(
        v.detail.as_deref().unwrap_or("").contains("runcrate"),
        "detail should explain runcrate did not run: {:?}",
        v.detail
    );
}

#[test]
fn substrate_validity_unverified_when_descriptor_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

// --- emit-driven integration coverage ---------------------------------
//
// The invariant readers above are exercised against hand-built
// `LoadedPackage`s shaped to the REAL on-disk sidecar contracts. This
// test goes one step further: it drives the public `emit_package` path
// end-to-end and runs all six invariants over the actually-emitted
// package root, asserting that at least one invariant SUBSTANTIVELY
// inspects content (`n_inspected` stops being ~0).

use ecaa_workflow_core::audit_proof::run_audit_proof;
use ecaa_workflow_core::classify::ClassificationResult;
use ecaa_workflow_core::clock::FrozenClock;
use ecaa_workflow_core::emitter::{emit_package, EmitConfig};

fn config_dir(rel: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join(rel)
}

fn emit_minimal_classification() -> ClassificationResult {
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "audit-proof emit-driven invariant smoke test".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "audit-proof emit-driven invariant smoke test".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

/// Drive emit_package through the v4 composer + emitter so all ECAA
/// sidecars are written into `out`.
fn emit_minimal_package(out: &std::path::Path) {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use ecaa_workflow_core::composer::compose_with_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use std::collections::BTreeMap;

    let atoms = AtomRegistry::load_from_dir(&config_dir("config/stage-atoms")).expect("atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config_dir("config/archetypes")).expect("archetypes");
    let goal = GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("audit-proof emit fixture".into()),
        confidence: 0.0,
    };
    let out_compose = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose");
    let dag = if let Some(wf) = out_compose.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(wf, "audit-proof-emit-fixture").expect("lower")
    } else {
        build_dag_from_composition(
            &out_compose.composition,
            "audit-proof-emit-fixture",
            &BTreeMap::new(),
            &[],
        )
        .expect("compose lower")
    };
    let clf = emit_minimal_classification();
    let policies_dir = config_dir("config/downstream-policy");
    emit_package(&EmitConfig {
        output_dir: out,
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        experimental_archetype: false,
    })
    .expect("emit");
}

#[test]
fn emitted_package_invariants_inspect_real_content() {
    let tmp = tempfile::TempDir::new().unwrap();
    emit_minimal_package(tmp.path());
    let report = run_audit_proof(tmp.path(), &NoopWrrocValidator, &FrozenClock::default()).unwrap();
    let total_inspected: usize = report.verdicts.iter().map(|v| v.n_inspected).sum();
    assert!(
        total_inspected > 1,
        "at least one invariant must substantively inspect content; got {total_inspected}: {:?}",
        report.verdicts
    );
    // A freshly emitted (un-executed) package carries proofs but no
    // claims/decisions yet — no invariant should hard-`Fail` at emit time.
    let any_fail = report
        .verdicts
        .iter()
        .any(|v| v.status == InvariantStatus::Fail);
    assert!(
        !any_fail,
        "no invariant should Fail on a freshly emitted package: {:?}",
        report.verdicts
    );
}
