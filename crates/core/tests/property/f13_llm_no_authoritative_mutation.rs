//! Tier F property tests for F13: the LLM never authoritatively
//! mutates a `WorkflowDag` — every mutation must come through the
//! closed tool vocabulary and the deterministic server endpoints
//! that wrap it.
//!
//! Per `docs/dag_eval.md` Tier F, this property is enforced two ways:
//!
//! 1. The `Tool` enum is closed (variant count exposed at compile
//! time by `strum::EnumCount`). No new mutation path can sneak in
//! without a code change that bumps `Tool::COUNT`.
//! 2. Every alone-in-turn mutation tool (the six high-impact tools
//! documented in CLAUDE.md) is enumerable, has `is_alone_in_turn()
//! == true`, and reports `is_mutation() == true`. This is the
//! invariant the server's dispatcher relies on to refuse a batch
//! that pairs a high-impact tool with any other tool in the same
//! turn.
//!
//! Adversarial corpus expansion (proptest input-fuzzing over malformed
//! tool args) is tracked separately; this file pins the closed-vocab
//! invariant that makes the dispatcher's `is_alone_in_turn`-based
//! refusal sound.

use ecaa_workflow_conversation::tools::Tool;
use strum::EnumCount;

/// The six alone-in-turn / high-impact tools per CLAUDE.md
/// ("Conversation crate" → tools section). Wired as raw `name()`
/// strings so the test fails loud if a variant is renamed without
/// the docs being updated in lock-step.
const ALONE_IN_TURN_TOOLS: &[&str] = &[
    "emit_package",
    "amend_stage_method",
    "rerun_task",
    "select_sensitivity_winner",
    "branch_session",
    "start_execution",
];

#[test]
fn closed_tool_vocabulary_has_at_least_the_alone_in_turn_set() {
    // Tool::COUNT is the canonical compile-time variant count. We
    // assert that the published vocabulary is at least as wide as
    // the documented alone-in-turn set; adding more (read-only,
    // intake mutation, conversational) is fine, removing any of
    // these six is a doc-violating breaking change.
    assert!(
        Tool::COUNT >= ALONE_IN_TURN_TOOLS.len(),
        "Tool::COUNT ({}) must be >= the {} documented alone-in-turn tools",
        Tool::COUNT,
        ALONE_IN_TURN_TOOLS.len()
    );
}

#[test]
fn every_alone_in_turn_tool_is_registered_and_flagged() {
    let variants = Tool::all_variants_for_tests();
    let by_name: std::collections::BTreeMap<&'static str, &Tool> =
        variants.iter().map(|t| (t.name(), t)).collect();

    for expected in ALONE_IN_TURN_TOOLS {
        let tool = by_name.get(expected).unwrap_or_else(|| {
            panic!(
                "alone-in-turn tool `{}` is not in Tool::all_variants_for_tests() \
                 — either CLAUDE.md is stale or the enum dropped a variant",
                expected
            )
        });
        assert!(
            tool.is_alone_in_turn(),
            "tool `{}` claims to be alone-in-turn in CLAUDE.md but \
             is_alone_in_turn() returned false — dispatcher contract \
             violated",
            expected
        );
        assert!(
            tool.is_mutation(),
            "tool `{}` is alone-in-turn but is_mutation() returned false; \
             every alone-in-turn tool is a mutation per the F13 contract",
            expected
        );
    }
}
