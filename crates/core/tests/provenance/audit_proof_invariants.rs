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
        intake: vec![],
        decisions: vec![],
        // Real harness shape: obligation outcomes, no `outputs` field.
        validation_reports: vec![json!({"task_id":"de","obligation_id":"o1","outcome":"passed"})],
        proofs,
        claims,
        verifier_decisions: vec![],
        assumptions,
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
    }
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

use ecaa_workflow_core::audit_proof::invariants::equivalence_failure::check_equivalence_failure;

fn pkg_with_q(
    verifier: Vec<serde_json::Value>,
    assumptions: Vec<serde_json::Value>,
) -> LoadedPackage {
    LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![],
        proofs: vec![],
        claims: None,
        verifier_decisions: verifier,
        assumptions,
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
    }
}

#[test]
fn equivalence_failure_passes_when_no_failures() {
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"succeeded","edge_id":"e-1"})],
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn equivalence_failure_fails_when_failure_unacknowledged() {
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        vec![],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
}

#[test]
fn equivalence_failure_passes_when_acknowledged_by_edge_id() {
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        vec![json!({"kind":"unprovable_edge","edge_id":"e-2"})],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn equivalence_failure_passes_when_acknowledged_via_detail_containment() {
    // Real v0.1 assumptions carry `detail`, not `edge_id`. An ack whose
    // free-text detail mentions the failed edge satisfies the predicate.
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        vec![json!({"assumption_id":"a-1","kind":"policy_exception",
                    "detail":"edge e-2 left unproved per reviewer-approved policy exception",
                    "stage_id":"de"})],
    );
    let v = check_equivalence_failure(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
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
    };
    let v = check_cross_graph_integrity(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
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
    use ecaa_workflow_core::composer::compose_with_version_and_modalities_full;
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
    let out_compose = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
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
    })
    .expect("emit");
}

#[test]
fn emitted_package_invariants_inspect_real_content() {
    let tmp = tempfile::TempDir::new().unwrap();
    emit_minimal_package(tmp.path());
    let report =
        run_audit_proof(tmp.path(), &NoopWrrocValidator, &FrozenClock::default()).unwrap();
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
