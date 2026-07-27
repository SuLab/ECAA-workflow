//! Emit-time reconciliation of prose-named input paths.
//!
//! The failure this locks down: an SME writes "the counts are in
//! `<dir>`" and then says "go ahead, don't wait on registering a path".
//! The LLM never calls `register_input_path`, so `session.inputs` stays
//! empty, `sync_user_inputs_to_package` no-ops, `runtime/inputs.json` is
//! never written — and `scripts/agent-claude.sh` builds its container
//! bind-mount args ONLY from that file. The SME's directory is therefore
//! ENOENT inside the task container and the acquisition stage silently
//! substitutes a public dataset. The path survived only as an
//! unregistered `pending_input_hints` entry.
//!
//! After reconciliation there are exactly two outcomes, and neither is
//! silent: the path exists and becomes a real registration, or it does
//! not and the package says so on its face.

use ecaa_workflow_conversation::emit::emit_with_conversation_log;
use ecaa_workflow_conversation::intake_path_hints::InputPathHint;
use ecaa_workflow_conversation::session::state::{UserInput, UserInputFile, UserInputKind};
use ecaa_workflow_conversation::session::Session;
use ecaa_workflow_conversation::tools::{dispatch_one, BatchableTool, Tool, ToolContext};
use ecaa_workflow_core::decision_log::DecisionType;
use serial_test::serial;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

/// Session with a composed DAG (core's `emit_package` needs one). The
/// prose deliberately names no filesystem path, so any hint on the
/// session is one the test put there.
async fn boot_session_with_dag() -> Session {
    let mut session = Session::test_fixture_with_dag();
    let ctx = ToolContext::new(config_dir(), "claude-sonnet-5");
    dispatch_one(
        &Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "bulk RNA-seq differential expression in human airway smooth muscle cells"
                .into(),
        }),
        &mut session,
        &ctx,
    )
    .await;
    session
}

/// Canonicalized tempdir path. The reconciler re-validates each hint
/// against `ECAA_INPUT_ROOTS` with canonicalized roots, so the test's
/// allowlist and hint roots must be canonical too.
fn canonical(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Every `AssumptionRecorded` id on the session, in record order.
fn assumption_ids(session: &Session) -> Vec<String> {
    session
        .decisions
        .iter()
        .filter_map(|r| match &r.decision {
            DecisionType::AssumptionRecorded { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect()
}

fn assumption_statement(session: &Session, id_prefix: &str) -> Option<String> {
    session.decisions.iter().find_map(|r| match &r.decision {
        DecisionType::AssumptionRecorded { id, statement, .. } if id.starts_with(id_prefix) => {
            Some(statement.clone())
        }
        _ => None,
    })
}

/// A prose-named path that IS on disk at emit must become a real
/// `local_path` registration, so `runtime/inputs.json` exists and the
/// harness has something to bind-mount.
#[tokio::test]
#[serial]
async fn existing_prose_path_is_auto_registered_at_emit() {
    let roots = tempdir().unwrap();
    let root_path = canonical(roots.path());
    let data_dir = root_path.join("himes-inputs");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("counts.tsv"), "gene\ts1\ts2\nA\t1\t2\n").unwrap();
    std::fs::write(data_dir.join("samples.csv"), "sample,group\ns1,ctrl\n").unwrap();

    let mut session = boot_session_with_dag().await;
    std::env::set_var("ECAA_INPUT_ROOTS", root_path.display().to_string());

    // What the extractor would have stashed from the SME's prose.
    session.pending_input_hints.push(InputPathHint {
        raw_mention: data_dir.join("counts.tsv").display().to_string(),
        canonical_root: data_dir.display().to_string(),
        matched_extension: "tsv".into(),
        file_mention: true,
        file_relpath: Some("counts.tsv".into()),
    });
    assert!(
        session.inputs.is_empty(),
        "precondition: the SME never registered the path"
    );

    let pkg = tempdir().unwrap();
    emit_with_conversation_log(&mut session, pkg.path(), &config_dir())
        .await
        .expect("emit must succeed");
    std::env::remove_var("ECAA_INPUT_ROOTS");

    // 1. runtime/inputs.json exists and names the SME's directory —
    //    this is the file agent-claude.sh reads to build `-v` args.
    let manifest_path = pkg.path().join("runtime/inputs.json");
    assert!(
        manifest_path.exists(),
        "runtime/inputs.json must be written for a prose-named path that exists on disk"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    let entries = manifest.as_array().expect("inputs.json must be an array");
    assert_eq!(
        entries.len(),
        1,
        "one auto-registered input; got {manifest}"
    );
    assert_eq!(
        entries[0]["root_path"].as_str(),
        Some(data_dir.display().to_string().as_str()),
        "the registered root must be the SME's directory"
    );
    assert_eq!(
        entries[0]["kind"].as_str(),
        Some("local_path"),
        "auto-registration must use the local_path kind the harness mount path filters on"
    );
    let files: Vec<&str> = entries[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["relpath"].as_str().unwrap())
        .collect();
    assert_eq!(
        files,
        vec!["counts.tsv", "samples.csv"],
        "the manifest must inventory both files, relpath-sorted"
    );

    // 2. The session now carries the registration.
    assert_eq!(
        session.inputs.len(),
        1,
        "session.inputs must gain the entry"
    );
    assert!(
        session.pending_input_hints.is_empty(),
        "a reconciled hint must be cleared so the UI stops offering to register it"
    );

    // 3. A DecisionRecord says it was auto-registered from prose.
    let ids = assumption_ids(&session);
    assert!(
        ids.iter()
            .any(|id| id.starts_with("a_input_path_") && !id.starts_with("a_input_path_missing_")),
        "an auto-registration DecisionRecord must exist; got {ids:?}"
    );
    let statement = assumption_statement(&session, "a_input_path_")
        .expect("the auto-registration record must carry a statement");
    assert!(
        statement.contains(&data_dir.display().to_string()),
        "the record must name the path; got {statement}"
    );

    // 4. The record reached the on-disk audit log.
    let decisions = std::fs::read_to_string(pkg.path().join("runtime/decisions.jsonl")).unwrap();
    assert!(
        decisions.contains("a_input_path_"),
        "runtime/decisions.jsonl must carry the reconciliation record"
    );

    // 5. No "not found" note for a path that WAS found.
    assert!(
        !pkg.path().join("runtime/inputs-unavailable.json").exists(),
        "a resolvable path must not be reported as unavailable"
    );
}

/// A prose-named path that is NOT on disk at emit must not vanish: no
/// registration, but a visible CONTEXT.md note plus a machine-readable
/// sidecar and a DecisionRecord, so a downstream public-source
/// substitution can never be a surprise.
#[tokio::test]
#[serial]
async fn absent_prose_path_is_recorded_as_unavailable() {
    let roots = tempdir().unwrap();
    let root_path = canonical(roots.path());
    // Inside the allowlist, but never created.
    let missing_dir = root_path.join("himes-inputs");

    let mut session = boot_session_with_dag().await;
    std::env::set_var("ECAA_INPUT_ROOTS", root_path.display().to_string());
    session.pending_input_hints.push(InputPathHint {
        raw_mention: missing_dir.join("counts.tsv").display().to_string(),
        canonical_root: missing_dir.display().to_string(),
        matched_extension: "tsv".into(),
        file_mention: true,
        file_relpath: Some("counts.tsv".into()),
    });

    let pkg = tempdir().unwrap();
    emit_with_conversation_log(&mut session, pkg.path(), &config_dir())
        .await
        .expect("emit must succeed even when a named path is gone");
    std::env::remove_var("ECAA_INPUT_ROOTS");

    // 1. Nothing registered — an absent path must never be faked into
    //    a manifest the agent would then try to read.
    assert!(
        !pkg.path().join("runtime/inputs.json").exists(),
        "an absent path must not produce a registration manifest"
    );
    assert!(session.inputs.is_empty(), "session.inputs must stay empty");

    // 2. CONTEXT.md carries the visible "named but not present" note.
    let context = std::fs::read_to_string(pkg.path().join("CONTEXT.md")).unwrap();
    assert!(
        context.contains("## SME-named data inputs NOT found at emit"),
        "CONTEXT.md must carry the unavailable-inputs section"
    );
    assert!(
        context.contains(&missing_dir.display().to_string()),
        "the note must name the path the SME gave"
    );
    assert!(
        context.contains("not present on disk at emit"),
        "the note must state why the path was unusable"
    );
    assert!(
        context.contains("DEVIATION"),
        "the note must tell the agent a public-source fallback is a reportable deviation"
    );

    // 3. Machine-readable twin for the reporting stage.
    let sidecar = pkg.path().join("runtime/inputs-unavailable.json");
    assert!(
        sidecar.exists(),
        "runtime/inputs-unavailable.json must be written"
    );
    let body: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sidecar).unwrap()).unwrap();
    assert_eq!(
        body.as_array().map(Vec::len),
        Some(1),
        "one unavailable entry; got {body}"
    );
    assert_eq!(
        body[0]["canonical_root"].as_str(),
        Some(missing_dir.display().to_string().as_str())
    );

    // 4. DecisionRecord.
    let ids = assumption_ids(&session);
    assert!(
        ids.iter().any(|id| id.starts_with("a_input_path_missing_")),
        "an unavailable-input DecisionRecord must exist; got {ids:?}"
    );
    let decisions = std::fs::read_to_string(pkg.path().join("runtime/decisions.jsonl")).unwrap();
    assert!(
        decisions.contains("a_input_path_missing_"),
        "runtime/decisions.jsonl must carry the unavailable-input record"
    );

    // 5. The hint stays pending so the SME can still restore the path
    //    and register it through the Inputs tab.
    assert_eq!(
        session.pending_input_hints.len(),
        1,
        "an unresolved hint must stay pending for SME recovery"
    );
}

/// Regression: a session that registered its inputs normally must emit
/// exactly as before — reconciliation is additive, not a rewrite.
#[tokio::test]
#[serial]
async fn registered_inputs_still_sync_unchanged() {
    let roots = tempdir().unwrap();
    let root_path = canonical(roots.path());
    let data_dir = root_path.join("cohort");
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::write(data_dir.join("counts.tsv"), "gene\ts1\nA\t1\n").unwrap();

    let mut session = boot_session_with_dag().await;
    std::env::set_var("ECAA_INPUT_ROOTS", root_path.display().to_string());

    // The normal path: the SME clicked "Register" and the REST handler
    // pushed a fully-built UserInput. No hint is left pending.
    session.inputs.push(UserInput {
        input_id: "0123456789abcdef".into(),
        label: "cohort".into(),
        kind: UserInputKind::LocalPath,
        root_path: data_dir.display().to_string(),
        files: vec![UserInputFile {
            relpath: "counts.tsv".into(),
            size_bytes: 14,
            sha256: "a".repeat(64),
        }],
        registered_at: chrono::Utc::now(),
        registered_by: session.owner_user.clone(),
    });
    assert!(
        session.pending_input_hints.is_empty(),
        "precondition: nothing left to reconcile"
    );

    let pkg = tempdir().unwrap();
    emit_with_conversation_log(&mut session, pkg.path(), &config_dir())
        .await
        .expect("emit must succeed");
    std::env::remove_var("ECAA_INPUT_ROOTS");

    // The registration syncs exactly as before, untouched.
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(pkg.path().join("runtime/inputs.json")).unwrap(),
    )
    .unwrap();
    let entries = manifest.as_array().unwrap();
    assert_eq!(entries.len(), 1, "no extra entries; got {manifest}");
    assert_eq!(
        entries[0]["input_id"].as_str(),
        Some("0123456789abcdef"),
        "the SME's registration must survive verbatim"
    );
    assert_eq!(
        entries[0]["files"][0]["sha256"].as_str(),
        Some("a".repeat(64).as_str()),
        "the registered manifest must not be recomputed"
    );
    assert_eq!(
        session.inputs.len(),
        1,
        "reconciliation must not duplicate an existing registration"
    );

    // CONTEXT.md keeps the existing SME-supplied section, and gains no
    // unavailable-inputs block.
    let context = std::fs::read_to_string(pkg.path().join("CONTEXT.md")).unwrap();
    assert!(
        context.contains("## SME-supplied data inputs"),
        "the registered-inputs narrative must still be appended"
    );
    assert!(
        !context.contains("## SME-named data inputs NOT found at emit"),
        "no unavailable-inputs block when nothing was unresolved"
    );
    assert!(
        !pkg.path().join("runtime/inputs-unavailable.json").exists(),
        "no unavailable sidecar when nothing was unresolved"
    );

    // And no reconciliation DecisionRecord fired.
    let ids = assumption_ids(&session);
    assert!(
        !ids.iter().any(|id| id.starts_with("a_input_path_")),
        "reconciliation must not fire for an already-registered input; got {ids:?}"
    );
}
