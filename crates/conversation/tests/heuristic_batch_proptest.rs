//! Property test for the dispatcher's alone-in-turn enforcement.
//!
//! The wire-level `Tool` enum is partitioned into `BatchableTool` and
//! `HighImpactTool` sealed sub-enums (CLAUDE.md C20). The dispatcher's
//! `dispatch_batch` rejects any batch that mixes a `HighImpactTool` with
//! a sibling tool — every result in the rejected batch surfaces a
//! `ToolError::PreconditionFailure` whose reason carries the
//! "must be the only tool call in its turn" sentinel.
//!
//! This property test fuzzes batches of `Tool` variants and asserts the
//! invariant from both sides:
//!
//! 1. **Illegal**: any batch of length ≥ 2 containing at least one
//!    `Tool::HighImpact(_)` is rejected. Every `ToolResult` carries
//!    `is_error = true`, `content["error_kind"] == "precondition_failure"`,
//!    and `content["reason"]` contains the sentinel substring.
//! 2. **Legal**: any batch containing zero `Tool::HighImpact(_)` is NOT
//!    rejected by the alone-in-turn guard. Individual handlers may
//!    still surface errors (e.g. `amend_stage_method` will refuse to
//!    run against a freshly-constructed session), but no result will
//!    carry the alone-in-turn sentinel reason.
//!

use proptest::prelude::*;
use scripps_workflow_conversation::{
    dispatch_batch, BatchableTool, HighImpactTool, Session, Tool, ToolContext,
};
use std::path::PathBuf;
use uuid::Uuid;

/// Substring that uniquely identifies the alone-in-turn rejection
/// reason as written by `dispatch_batch` in
/// `crates/conversation/src/tools/mod.rs`.
const ALONE_IN_TURN_SENTINEL: &str = "must be the only tool call in its turn";

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn ctx() -> ToolContext {
    ToolContext::new(config_dir(), "claude-sonnet-4-6")
}

/// Strategy producing a single `Tool` chosen uniformly from the
/// closed `Tool::all_variants_for_tests()` set (covers both
/// `BatchableTool` and `HighImpactTool`). Index-based selection keeps
/// the strategy `Clone + 'static` without per-variant proptest derives.
fn arb_tool() -> impl Strategy<Value = Tool> {
    let variants = Tool::all_variants_for_tests();
    let n = variants.len();
    (0..n).prop_map(move |i| variants[i].clone())
}

/// Strategy producing a single [`BatchableTool`]. Used by the
/// legal-batch proptest so every generated batch is guaranteed to
/// contain zero [`HighImpactTool`] variants — no `prop_assume!`
/// rejection budget needed.
fn arb_batchable_tool() -> impl Strategy<Value = Tool> {
    let variants = BatchableTool::all_variants_for_tests();
    let n = variants.len();
    (0..n).prop_map(move |i| Tool::Batchable(variants[i].clone()))
}

/// Strategy producing a single [`HighImpactTool`]. Used as a seed by
/// the illegal-batch proptest to guarantee at least one HighImpact
/// variant in every generated batch.
fn arb_high_impact_tool() -> impl Strategy<Value = Tool> {
    let variants = HighImpactTool::all_variants_for_tests();
    let n = variants.len();
    (0..n).prop_map(move |i| Tool::HighImpact(variants[i].clone()))
}

/// Strategy: a batch of 2..=4 tools where at least one entry is a
/// [`HighImpactTool`]. Builds the batch by sampling 1..=3 arbitrary
/// `Tool` variants and inserting a guaranteed HighImpact at a random
/// position — no `prop_assume!` rejection budget required.
fn arb_illegal_batch() -> impl Strategy<Value = Vec<Tool>> {
    (
        proptest::collection::vec(arb_tool(), 1..=3),
        arb_high_impact_tool(),
        any::<usize>(),
    )
        .prop_map(|(mut rest, high_impact, pos_seed)| {
            let pos = pos_seed % (rest.len() + 1);
            rest.insert(pos, high_impact);
            rest
        })
}

/// Strategy: a batch of 1..=4 tools where every entry is a
/// [`BatchableTool`]. Guaranteed legal — the alone-in-turn guard
/// must not fire.
fn arb_legal_batch() -> impl Strategy<Value = Vec<Tool>> {
    proptest::collection::vec(arb_batchable_tool(), 1..=4)
}

fn batch_contains_high_impact(batch: &[Tool]) -> bool {
    batch.iter().any(|t| matches!(t, Tool::HighImpact(_)))
}

fn result_is_alone_in_turn_rejection(content: &serde_json::Value) -> bool {
    content.get("error_kind").and_then(|v| v.as_str()) == Some("precondition_failure")
        && content
            .get("reason")
            .and_then(|v| v.as_str())
            .map(|r| r.contains(ALONE_IN_TURN_SENTINEL))
            .unwrap_or(false)
}

proptest! {
    #![proptest_config(ProptestConfig {
        // Tighter case count keeps the suite fast under `cargo test`
        // while still covering the structural invariant — every batch
        // permutation reaches the alone-in-turn branch via the same
        // code path.
        cases: 64,
        .. ProptestConfig::default()
    })]

    /// Invariant: any batch of length ≥ 2 that contains at least one
    /// `Tool::HighImpact(_)` MUST be rejected by `dispatch_batch` with
    /// `ToolError::PreconditionFailure` carrying the alone-in-turn
    /// sentinel reason. The whole batch fails — every result is an
    /// error. The strategy guarantees both conditions structurally
    /// (no `prop_assume!` rejection budget needed).
    #[test]
    fn batch_with_high_impact_is_always_rejected(batch in arb_illegal_batch()) {
        // Sanity check the strategy upheld its contract.
        prop_assert!(batch.len() >= 2);
        prop_assert!(batch_contains_high_impact(&batch));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let results = rt.block_on(async {
            let mut session = Session::new(false);
            let tagged: Vec<(Uuid, Tool)> = batch
                .iter()
                .cloned()
                .map(|t| (Uuid::new_v4(), t))
                .collect();
            dispatch_batch(tagged, &mut session, &ctx()).await
        });

        prop_assert_eq!(results.len(), batch.len());
        for (_, r) in &results {
            prop_assert!(
                r.is_error,
                "expected error result for batch containing HighImpact: {:?}",
                r.content
            );
            prop_assert!(
                result_is_alone_in_turn_rejection(&r.content),
                "expected alone-in-turn rejection signature, got: {:?}",
                r.content
            );
        }
    }

    /// Counter-invariant: batches containing zero `Tool::HighImpact(_)`
    /// are NOT rejected by the alone-in-turn guard. Individual
    /// `BatchableTool` handlers may still surface unrelated errors
    /// against a freshly-constructed `Session`, but the alone-in-turn
    /// sentinel reason must never appear.
    #[test]
    fn batchable_only_never_trips_alone_in_turn(batch in arb_legal_batch()) {
        prop_assert!(!batch_contains_high_impact(&batch));

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let results = rt.block_on(async {
            let mut session = Session::new(false);
            let tagged: Vec<(Uuid, Tool)> = batch
                .iter()
                .cloned()
                .map(|t| (Uuid::new_v4(), t))
                .collect();
            dispatch_batch(tagged, &mut session, &ctx()).await
        });

        prop_assert_eq!(results.len(), batch.len());
        for (_, r) in &results {
            prop_assert!(
                !result_is_alone_in_turn_rejection(&r.content),
                "batch without HighImpact unexpectedly tripped alone-in-turn: {:?}",
                r.content
            );
        }
    }
}

#[test]
fn single_high_impact_does_not_trip_alone_in_turn() {
    // Sanity check at the boundary: a batch of exactly one
    // HighImpactTool is legal (the alone-in-turn guard only fires for
    // batches of length > 1).
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let results = rt.block_on(async {
        let mut session = Session::new(false);
        let batch = vec![(
            Uuid::new_v4(),
            Tool::HighImpact(HighImpactTool::EmitPackage { output_dir: None }),
        )];
        dispatch_batch(batch, &mut session, &ctx()).await
    });
    assert_eq!(results.len(), 1);
    // The lone EmitPackage may still error for state-machine reasons
    // (the freshly-constructed session is not in ReadyToEmit), but the
    // alone-in-turn sentinel must not appear.
    let (_, r) = &results[0];
    assert!(
        !result_is_alone_in_turn_rejection(&r.content),
        "single HighImpact batch unexpectedly tripped alone-in-turn: {:?}",
        r.content
    );
}

#[test]
fn batched_high_impact_with_batchable_is_rejected() {
    // Explicit non-fuzzed example mirroring fixture 63: pair EmitPackage
    // with GetSessionState and assert both results carry the alone-in-turn
    // rejection signature.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");
    let results = rt.block_on(async {
        let mut session = Session::new(false);
        let batch = vec![
            (
                Uuid::new_v4(),
                Tool::HighImpact(HighImpactTool::EmitPackage { output_dir: None }),
            ),
            (
                Uuid::new_v4(),
                Tool::Batchable(BatchableTool::GetSessionState),
            ),
        ];
        dispatch_batch(batch, &mut session, &ctx()).await
    });
    assert_eq!(results.len(), 2);
    for (_, r) in &results {
        assert!(r.is_error);
        assert!(
            result_is_alone_in_turn_rejection(&r.content),
            "expected alone-in-turn rejection, got: {:?}",
            r.content
        );
    }
}
