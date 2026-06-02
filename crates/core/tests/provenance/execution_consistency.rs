//! Inv 6 sub-check: every WRROC `@graph` HowToStep has a matching E
//! WorkflowStep (`proofs.jsonl`), and vice versa. Referential agreement only
//! (F11) — NOT step-ordering or numeric equivalence.
use ecaa_workflow_core::audit_proof::invariants::execution_consistency::check_execution_consistency;
use serde_json::json;

#[test]
fn agreeing_graph_and_e_sidecar_pass() {
    // @graph has HowToStep #step/de; proofs.jsonl has workflow:de.
    let graph = vec![json!({"@id":"#step/de","@type":"HowToStep"})];
    let e_steps = vec![json!({"id":"workflow:de","type":"WorkflowStep"})];
    let (n, viol) = check_execution_consistency(&graph, &e_steps);
    assert_eq!(n, 1);
    assert!(viol.is_empty(), "agreeing materializations must not drift");
}

#[test]
fn graph_step_missing_from_e_is_drift() {
    let graph = vec![
        json!({"@id":"#step/de","@type":"HowToStep"}),
        json!({"@id":"#step/qc","@type":"HowToStep"}),
    ];
    let e_steps = vec![json!({"id":"workflow:de","type":"WorkflowStep"})];
    let (_n, viol) = check_execution_consistency(&graph, &e_steps);
    assert_eq!(
        viol.len(),
        1,
        "qc present in @graph but absent from E must drift"
    );
}

#[test]
fn e_step_missing_from_graph_is_drift() {
    // Symmetric: a WorkflowStep with no matching HowToStep is also drift.
    let graph = vec![json!({"@id":"#step/de","@type":"HowToStep"})];
    let e_steps = vec![
        json!({"id":"workflow:de","type":"WorkflowStep"}),
        json!({"id":"workflow:report","type":"WorkflowStep"}),
    ];
    let (_n, viol) = check_execution_consistency(&graph, &e_steps);
    assert_eq!(
        viol.len(),
        1,
        "report present in E but absent from @graph must drift"
    );
}

// --- Inv 6 fold: substrate_validity surfaces @graph↔E drift -------------

use ecaa_workflow_core::audit_proof::invariants::substrate_validity::check_substrate_validity;
use ecaa_workflow_core::audit_proof::InvariantStatus;
use ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;

/// Write a `ro-crate-metadata.json` whose `@graph` carries the given
/// `#step-<id>` HowToSteps, plus a `proofs.jsonl` of `workflow:<to>` edges.
fn write_pkg(dir: &std::path::Path, graph_steps: &[&str], proof_edges: &[(&str, &str)]) {
    let steps: Vec<serde_json::Value> = graph_steps
        .iter()
        .map(|s| json!({"@id": format!("#step-{s}"), "@type": "HowToStep"}))
        .collect();
    let meta = json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": steps,
    });
    std::fs::write(
        dir.join("ro-crate-metadata.json"),
        serde_json::to_string(&meta).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("runtime")).unwrap();
    let mut proofs = String::new();
    for (from, to) in proof_edges {
        proofs.push_str(&format!(
            "{}\n",
            json!({"id": format!("workflow:{to}"), "type":"WorkflowStep",
                   "computed_from": format!("workflow:{from}")})
        ));
    }
    std::fs::write(dir.join("runtime/proofs.jsonl"), proofs).unwrap();
}

#[test]
fn substrate_validity_folds_execution_drift_into_inv6() {
    // @graph has steps {qc, de, report}; proofs cover only qc->de
    // (so `report` is in @graph but absent from the E endpoint set) → drift.
    let tmp = tempfile::TempDir::new().unwrap();
    write_pkg(tmp.path(), &["qc", "de", "report"], &[("qc", "de")]);
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert!(
        v.n_violations >= 1,
        "drift (report missing from E) must add a violation: {v:?}"
    );
    assert!(
        v.detail.as_deref().unwrap_or("").contains("report"),
        "drift detail must name the drifted step: {:?}",
        v.detail
    );
}

#[test]
fn substrate_validity_no_drift_when_graph_and_e_agree() {
    // @graph {qc, de}; proofs qc->de covers BOTH endpoints (qc as
    // computed_from, de as id) → no drift; base verdict is preserved
    // (Unverified for the no-op validator, NOT a drift Fail).
    let tmp = tempfile::TempDir::new().unwrap();
    write_pkg(tmp.path(), &["qc", "de"], &[("qc", "de")]);
    let v = check_substrate_validity(tmp.path(), &NoopWrrocValidator);
    assert_eq!(
        v.status,
        InvariantStatus::Unverified,
        "agreeing materializations must not add drift: {v:?}"
    );
}
