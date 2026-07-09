//! D8 audit-proof sidecar emission tests — verifies that
//! `emit_with_conversation_log` writes `runtime/audit-proof-report.json`
//! with the expected schema_version + 6 verdicts, and that
//! `ECAA_ABLATE_AUDIT_PROOF` suppresses the sidecar entirely.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use ecaa_workflow_core::audit_proof::{
    run_audit_proof, InvariantId, InvariantStatus, InvariantVerdict,
};
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;
use serial_test::serial;
use std::path::PathBuf;
use tempfile::tempdir;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

async fn boot_session_with_dag() -> Session {
    let mut session = Session::test_fixture_with_dag();
    let ctx = ToolContext::new(config_dir(), "claude-sonnet-5");
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples comparing degenerated and healthy"
                .into(),
        }),
        &mut session,
        &ctx,
    )
    .await;
    session
}

#[tokio::test]
#[serial]
async fn emit_writes_audit_proof_sidecar() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let sidecar = dir.path().join("runtime/audit-proof-report.json");
    assert!(
        sidecar.exists(),
        "audit-proof-report.json should be emitted"
    );
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(
        body.get("schema_version").and_then(|v| v.as_str()),
        Some("0.1")
    );
    let verdicts = body.get("verdicts").and_then(|v| v.as_array()).unwrap();
    assert_eq!(verdicts.len(), 6);
}

#[tokio::test]
#[serial]
async fn audit_proof_suppressed_when_ablate_audit_proof_set() {
    std::env::set_var("ECAA_ABLATE_AUDIT_PROOF", "1");
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();
    let sidecar = dir.path().join("runtime/audit-proof-report.json");
    let exists = sidecar.exists();
    std::env::remove_var("ECAA_ABLATE_AUDIT_PROOF");
    assert!(
        !exists,
        "audit-proof-report.json should NOT be emitted under ablation"
    );
}

// --- F11 execution-consistency: bare on-disk proofs.jsonl form ----------
//
// The production emit path (`write_phase16_sidecars` → `build_proofs_jsonl`)
// writes BARE `EdgeContract` rows (`{"from_node","from_port","to_node",
// "to_port","proof"}`) with no `workflow:`-enveloped `id`/`computed_from`
// keys. The Inv-6 execution-consistency sub-check must derive the E
// execution-step set from `from_node`/`to_node` so it does NOT report
// spurious drift on every node of a real package, AND still catches a
// genuinely dropped edge.

fn substrate_verdict(
    report: &ecaa_workflow_core::audit_proof::AuditProofReport,
) -> InvariantVerdict {
    report
        .verdicts
        .iter()
        .find(|v| v.id == InvariantId::SubstrateValidity)
        .cloned()
        .expect("report must carry a substrate_validity verdict")
}

/// Regression: a package produced by the REAL conversation emit path
/// (bare proofs.jsonl + a real core `@graph` with `#step-` HowToSteps)
/// must NOT report spurious execution-step drift under Inv 6.
#[tokio::test]
#[serial]
async fn substrate_validity_no_spurious_drift_on_real_package() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    // Sanity: the real path wrote a bare proofs.jsonl (from_node/to_node,
    // no `workflow:`-enveloped id) and a core @graph with #step- HowToSteps.
    let proofs = std::fs::read_to_string(dir.path().join("runtime/proofs.jsonl")).unwrap();
    assert!(
        !proofs.trim().is_empty(),
        "real emit must write a non-empty proofs.jsonl (v4 DAG with edges)"
    );
    assert!(
        proofs.contains("\"from_node\""),
        "proofs.jsonl must carry the bare EdgeContract form: {proofs}"
    );
    let meta = std::fs::read_to_string(dir.path().join("ro-crate-metadata.json")).unwrap();
    assert!(
        meta.contains("#step-"),
        "core @graph must materialize #step- HowToSteps"
    );

    let report = run_audit_proof(dir.path(), &NoopWrrocValidator, &WallClock).unwrap();
    let v = substrate_verdict(&report);
    assert_eq!(
        v.n_violations, 0,
        "real package must report NO spurious execution-step drift: {v:?}"
    );
    assert!(
        v.detail
            .as_deref()
            .map(|d| !d.contains("execution-step drift"))
            .unwrap_or(true),
        "real package must not name any drifted step: {:?}",
        v.detail
    );
    // The base verdict must be preserved (Unverified for the no-op
    // validator), NOT downgraded to Warn by phantom drift.
    assert_eq!(
        v.status,
        InvariantStatus::Unverified,
        "spurious drift must not downgrade the base verdict: {v:?}"
    );
}

/// Positive control: dropping ONE real edge row from the bare proofs.jsonl
/// produced by the real emit path removes that step's endpoint from the E
/// set, which MUST surface as execution-step drift under Inv 6.
#[tokio::test]
#[serial]
async fn substrate_validity_detects_dropped_real_edge() {
    let dir = tempdir().unwrap();
    let mut session = boot_session_with_dag().await;
    emit_with_conversation_log(&mut session, dir.path(), &config_dir())
        .await
        .unwrap();

    let proofs_path = dir.path().join("runtime/proofs.jsonl");
    let proofs = std::fs::read_to_string(&proofs_path).unwrap();
    let lines: Vec<&str> = proofs.lines().filter(|l| !l.trim().is_empty()).collect();
    assert!(
        lines.len() >= 2,
        "need ≥2 edges to drop one and still leave a non-trivial E set: {proofs}"
    );
    let rows: Vec<serde_json::Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).expect("proof row parses"))
        .collect();
    let endpoints = |r: &serde_json::Value| -> Vec<String> {
        ["from_node", "to_node"]
            .iter()
            .filter_map(|k| r.get(*k).and_then(|v| v.as_str()).map(String::from))
            .collect()
    };

    // Tally how many edge-endpoints each node owns across ALL rows. A node
    // with a total count of exactly 1 is referenced by a single edge, so
    // dropping that edge removes the node from E entirely — its @graph
    // HowToStep then has no E counterpart (true drift). This is robust to
    // the DAG's actual topology (no reliance on sort order).
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for r in &rows {
        for n in endpoints(r) {
            *counts.entry(n).or_insert(0) += 1;
        }
    }
    let dropped_node = counts.iter().find(|(_, &c)| c == 1).map(|(n, _)| n.clone());
    // Index of the single edge that references that node; fall back to the
    // last edge if no degree-1 node exists (still detectable drift, just
    // possibly a non-isolated node).
    let drop_idx = match &dropped_node {
        Some(node) => rows
            .iter()
            .position(|r| endpoints(r).iter().any(|e| e == node))
            .expect("degree-1 node must live in some edge"),
        None => rows.len() - 1,
    };
    // The set of step ids that survive in E after the drop.
    let surviving: std::collections::BTreeSet<String> = rows
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != drop_idx)
        .flat_map(|(_, r)| endpoints(r))
        .collect();
    // A node that was in the dropped edge but survives nowhere else =
    // the step we expect to be flagged as drift.
    let expected_drift: Vec<String> = endpoints(&rows[drop_idx])
        .into_iter()
        .filter(|n| !surviving.contains(n))
        .collect();
    assert!(
        !expected_drift.is_empty(),
        "dropping edge {drop_idx} must isolate ≥1 node from E: {proofs}"
    );

    let mut kept: Vec<&str> = lines.clone();
    kept.remove(drop_idx);
    let mut rewritten = kept.join("\n");
    rewritten.push('\n');
    std::fs::write(&proofs_path, rewritten).unwrap();

    let report = run_audit_proof(dir.path(), &NoopWrrocValidator, &WallClock).unwrap();
    let v = substrate_verdict(&report);
    assert!(
        v.n_violations >= 1,
        "dropping a real edge must surface execution-step drift: {v:?}"
    );
    let detail = v.detail.as_deref().unwrap_or("");
    assert!(
        detail.contains("execution-step drift"),
        "drift detail must mark the missing step: {detail}"
    );
    for step in &expected_drift {
        assert!(
            detail.contains(step),
            "drift detail must name the isolated step {step}: {detail}"
        );
    }
}
