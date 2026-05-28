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
fn decision_justification_passes_with_citation() {
    let pkg = fixture_with_decisions(vec![
        json!({"kind":"method_choice","rationale":"short","cites":["doi:10.1/x"]}),
    ]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn decision_justification_passes_with_long_rationale() {
    let pkg = fixture_with_decisions(vec![
        json!({"kind":"method_choice","rationale":"This is a thirty-plus character rationale string.","cites":[]}),
    ]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn decision_justification_fails_with_short_no_citation() {
    let pkg = fixture_with_decisions(vec![
        json!({"kind":"method_choice","rationale":"too short","cites":[]}),
    ]);
    let v = check_decision_justification(&pkg);
    assert_eq!(v.status, InvariantStatus::Warn);
    assert_eq!(v.n_violations, 1);
}

#[test]
fn decision_justification_ignores_non_method_decisions() {
    let pkg = fixture_with_decisions(vec![
        json!({"kind":"confirm","rationale":"","cites":[]}),
        json!({"kind":"reject","rationale":"","cites":[]}),
    ]);
    let v = check_decision_justification(&pkg);
    // n_inspected counts only method_choice entries
    assert_eq!(v.n_inspected, 0);
    assert_eq!(v.status, InvariantStatus::Unverified);
}

use ecaa_workflow_core::audit_proof::invariants::evidence_coverage::check_evidence_coverage;

fn pkg_with(
    claims: Option<serde_json::Value>,
    reports: Vec<serde_json::Value>,
    assumptions: Vec<serde_json::Value>,
) -> LoadedPackage {
    LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: reports,
        proofs: vec![],
        claims,
        verifier_decisions: vec![],
        assumptions,
        determinism_shim: None,
        security_policy: None,
        plot_affordances: None,
    }
}

#[test]
fn evidence_coverage_passes_when_outputs_referenced() {
    let pkg = pkg_with(
        Some(json!({"verdicts":[{"claim_id":"c-1","status":"verified",
            "supported_by":["runtime/tables/de_results.csv#row_TP53"]}]})),
        vec![
            json!({"task_id":"differential_expression","outputs":["runtime/tables/de_results.csv"]}),
        ],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_fails_when_output_orphan() {
    let pkg = pkg_with(
        Some(json!({"verdicts":[]})),
        vec![
            json!({"task_id":"differential_expression","outputs":["runtime/tables/de_results.csv"]}),
        ],
        vec![],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Fail);
    assert_eq!(v.n_violations, 1);
}

#[test]
fn evidence_coverage_passes_when_orphan_marked_unused() {
    let pkg = pkg_with(
        Some(json!({"verdicts":[]})),
        vec![
            json!({"task_id":"differential_expression","outputs":["runtime/tables/de_results.csv"]}),
        ],
        vec![json!({"kind":"output_unused","detail":"runtime/tables/de_results.csv"})],
    );
    let v = check_evidence_coverage(&pkg);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn evidence_coverage_warns_when_no_claims_file() {
    let pkg = pkg_with(
        None,
        vec![
            json!({"task_id":"differential_expression","outputs":["runtime/tables/de_results.csv"]}),
        ],
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
fn equivalence_failure_passes_when_acknowledged() {
    let pkg = pkg_with_q(
        vec![json!({"event":"prove","outcome":"failed","edge_id":"e-2"})],
        vec![json!({"kind":"unprovable_edge","edge_id":"e-2"})],
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
        validation_reports: vec![json!({"task_id":"de","outputs":["runtime/tables/de.csv"]})],
        proofs: vec![json!({"edge_id":"e-1","from":"counts","to":"de"})],
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
fn cross_graph_fails_when_supported_by_dangling() {
    let pkg = LoadedPackage {
        intake: vec![],
        decisions: vec![],
        validation_reports: vec![json!({"task_id":"de","outputs":["runtime/tables/de.csv"]})],
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
fn substrate_validity_with_noop_validator_passes_on_present_descriptor() {
    let tmp = tempfile::TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("ro-crate-metadata.json"),
        r#"{"@context":"https://w3id.org/ro/crate/1.1/context","@graph":[]}"#,
    )
    .unwrap();
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert_eq!(v.status, InvariantStatus::Pass);
}

#[test]
fn substrate_validity_unverified_when_descriptor_absent() {
    let tmp = tempfile::TempDir::new().unwrap();
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert_eq!(v.status, InvariantStatus::Unverified);
}
