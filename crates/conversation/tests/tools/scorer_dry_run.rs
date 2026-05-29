//! Dry-run integration test for the rubric scorer. The live scorer
//! test in `src/scorer.rs::tests::score_transcript_smoke` is
//! `#[ignore]`'d because it hits the real Anthropic API. This file
//! exercises the same code path with a `MockLlmBackend` returning a
//! hand-crafted scorer response, so the parser, transcript rendering,
//! and TOTAL drift verification all run on every `cargo test` without
//! an `ANTHROPIC_API_KEY`.

use ecaa_workflow_conversation::{
    score_transcript, BatchableTool, ConversationService, LlmBackend, MetricsStore, MockLlmBackend,
    RubricScore, SessionStore, StopReason, Tool, Turn, TurnResponse, Usage,
};
use std::path::PathBuf;
use std::sync::Arc;

use crate::common::TestEnv;

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

async fn make_service(scripted: Vec<TurnResponse>) -> (ConversationService, TestEnv) {
    let env = TestEnv::new();
    let store = SessionStore::open(env.path()).await.unwrap();
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    (ConversationService::new(backend, store, config_dir()), env)
}

fn assistant(text: &str) -> TurnResponse {
    TurnResponse {
        assistant_content: text.into(),
        tool_uses: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

fn tool_use(t: Tool) -> TurnResponse {
    TurnResponse {
        assistant_content: String::new(),
        tool_uses: vec![(uuid::Uuid::new_v4(), t)],
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

#[tokio::test]
async fn scorer_dry_run_against_captured_transcript() {
    // Step 1 — drive a real ConversationService through a small intake
    // → confirm → emit flow so we capture a real-shaped Vec<Turn>
    // (including the synthetic [tool results] system turns that the
    // scorer's render_transcript should know to skip).
    // `_env` keeps the tempdir alive (RAII via Arc<TempDir>) until the
    // test function exits.
    let (svc, _env) = make_service(vec![
        tool_use(Tool::Batchable(BatchableTool::AppendIntakeProse {
            prose: "single cell scRNA-seq from human IVD samples, 47 libs, 7 studies, degenerated vs healthy".into(),
        })),
        tool_use(Tool::Batchable(BatchableTool::ProposeSummaryConfirmation {
            summary_markdown: "Plan: 47 libs IVD scRNA-seq, degenerated vs healthy. The package reports statistical patterns, not causal claims. Click Confirm or Make corrections.".into(),
        })),
        assistant("Take a look — Confirm or correct when you're ready."),
    ])
    .await;
    let (session_id, _greeting) = svc.start_session(false).await.unwrap();
    svc.send_turn(
        session_id,
        "We have scRNA-seq data from 47 IVD samples comparing degenerated vs healthy.".into(),
        None,
    )
    .await
    .unwrap();
    let session = svc.get_session(session_id).await.unwrap();
    assert!(
        session.conversation.len() >= 3,
        "captured transcript should have at least greeting + user + assistant"
    );

    // Step 2 — build a separate MockLlmBackend that serves a hand-crafted
    // scorer response so we exercise the parse + drift-check path.
    let scorer_response = "NATURALNESS: 2\n\
        CONTINUITY: 2\n\
        ONE_QUESTION: 2\n\
        METHOD_NEUTRALITY: 2\n\
        CLAIM_BOUNDARY: 1\n\
        TOOL_EFFICIENCY: 2\n\
        CONFIRMATION: 2\n\
        RECOVERY: 2\n\
        TOTAL: 15\n\n\
        NOTES:\n\
        - claim boundary was implicit, not explicitly paraphrased\n";
    let scorer_backend: Arc<dyn LlmBackend> =
        Arc::new(MockLlmBackend::new(vec![assistant(scorer_response)]));

    // Step 3 — run the scorer against the captured transcript, billing
    // into the service's MetricsStore under the same session.
    let score = score_transcript(
        scorer_backend,
        svc.metrics(),
        session_id,
        &session.conversation,
        "Watch for claim boundary phrasing; the assistant should restate it in the SME's context.",
    )
    .await
    .expect("scorer should parse the hand-crafted response");

    assert_eq!(score.naturalness, 2);
    assert_eq!(score.claim_boundary, 1);
    // Legacy captures without HARDWARE_AWARENESS default that dimension
    // to 0, so 14 points across the first 8 dimensions = 14/18 total.
    assert_eq!(score.total(), 15);
    assert!(
        score.total() >= RubricScore::PASS_THRESHOLD,
        "15/18 should pass the 14/18 scorer gate"
    );
    // Mock backend returns zero-usage responses so scorer spend is $0,
    // but the session's scorer bucket should be initialized (the call
    // hit record_scorer_usage with a zero Usage).
    let snap = svc.metrics().snapshot(session_id).await.unwrap();
    assert_eq!(snap.scorer_cost_usd, 0.0);
    assert_eq!(snap.per_model_scorer_cost_usd.get("sonnet_4_6"), Some(&0.0));
}

#[tokio::test]
async fn scorer_dry_run_rejects_drift_above_one() {
    // Verify the per-test drift gate also fires through the live
    // backend interface (not just the unit test in scorer.rs::tests).
    // Scorer responds with TOTAL: 8 but per-dimension sum is 16.
    let scorer_response = "NATURALNESS: 2\n\
        CONTINUITY: 2\n\
        ONE_QUESTION: 2\n\
        METHOD_NEUTRALITY: 2\n\
        CLAIM_BOUNDARY: 2\n\
        TOOL_EFFICIENCY: 2\n\
        CONFIRMATION: 2\n\
        RECOVERY: 2\n\
        TOTAL: 8\n";
    let scorer_backend: Arc<dyn LlmBackend> =
        Arc::new(MockLlmBackend::new(vec![assistant(scorer_response)]));

    let transcript = vec![Turn::user("hello"), Turn::assistant("hi")];
    let metrics = MetricsStore::new();
    let session_id = uuid::Uuid::new_v4();
    let result = score_transcript(
        scorer_backend,
        &metrics,
        session_id,
        &transcript,
        "no notes",
    )
    .await;
    assert!(result.is_err(), "drift > 1 must surface as a parse error");
    let err = format!("{:?}", result.err().unwrap());
    assert!(
        err.contains("disagrees"),
        "error should mention TOTAL drift: {}",
        err
    );
}

#[tokio::test]
async fn scorer_dry_run_handles_low_score_below_threshold() {
    // Sub-threshold scores should still parse and return cleanly — the
    // scorer doesn't fail-fast on low scores; the nightly job decides
    // what to do with them.
    let scorer_response = "NATURALNESS: 1\n\
        CONTINUITY: 0\n\
        ONE_QUESTION: 1\n\
        METHOD_NEUTRALITY: 0\n\
        CLAIM_BOUNDARY: 0\n\
        TOOL_EFFICIENCY: 1\n\
        CONFIRMATION: 1\n\
        RECOVERY: 0\n\
        TOTAL: 4\n";
    let scorer_backend: Arc<dyn LlmBackend> =
        Arc::new(MockLlmBackend::new(vec![assistant(scorer_response)]));

    let transcript = vec![Turn::user("hi"), Turn::assistant("ok")];
    let metrics = MetricsStore::new();
    let session_id = uuid::Uuid::new_v4();
    let score = score_transcript(scorer_backend, &metrics, session_id, &transcript, "")
        .await
        .expect("parser accepts low scores");
    assert_eq!(score.total(), 4);
    assert!(score.total() < RubricScore::PASS_THRESHOLD);
}
