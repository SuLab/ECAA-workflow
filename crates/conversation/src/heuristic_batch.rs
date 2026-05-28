//! Batching strategy plug-in for [`HeuristicMockBackend`].
//!
//! Phase-0 of the non-scripted backend emits exactly one `tool_use`
//! per response (see `heuristic_mock.rs`). The non-scripted mock's
//! This module adds optional batching so the test path can drive the
//! dispatcher's alone-in-turn enforcement from a non-scripted backend.
//!
//! Two new shapes are exposed:
//!
//! * [`BatchStrategy::BatchedReadOnly`] — emits multiple
//!   [`BatchableTool`](crate::tools::BatchableTool) entries in a single
//!   `TurnResponse`. Always legal under the
//!   [`BatchableTool`](crate::tools::BatchableTool) /
//!   [`HighImpactTool`](crate::tools::HighImpactTool) sealed-enum split.
//! * [`BatchStrategy::BatchedHighImpactIllegal`] — mixes one
//!   [`HighImpactTool`](crate::tools::HighImpactTool) with a sibling
//!   [`BatchableTool`](crate::tools::BatchableTool). Always rejected by
//!   `dispatch_batch` with `ToolError::PreconditionFailure` whose reason
//!   carries the "must be the only tool call in its turn" sentinel —
//!   fixture 63 asserts the rejection fires and that the session does
//!   NOT transition through `emit_package`.
//!
//! The strategy is consulted only when the underlying decision table
//! would have emitted a non-empty `tool_uses` set. Greeting / closing
//! assistant-text turns are passed through unchanged so the rule-7
//! fallback path stays observable.

use crate::anthropic::{StopReason, TurnResponse, Usage};
use crate::tools::{BatchableTool, HighImpactTool, Tool};
use uuid::Uuid;

/// Batching strategy for [`HeuristicMockBackend`](crate::HeuristicMockBackend).
/// Tests can inject a strategy to exercise alone-in-turn enforcement on
/// the dispatcher.
///
/// The default ([`BatchStrategy::Single`]) preserves the Phase-0
/// behavior — exactly one tool call per response, mirroring the way the
/// production decision table emits.
#[derive(Default, Clone, Debug)]
pub enum BatchStrategy {
    /// Single tool per response (Phase-0 default).
    #[default]
    Single,
    /// Batch multiple read-only + intake-mutation tools per response.
    /// Always legal under the [`BatchableTool`] sealed enum — the
    /// dispatcher's `dispatch_batch` consumes every entry in order.
    BatchedReadOnly,
    /// Mix a [`HighImpactTool`] with another tool. ALWAYS rejected by
    /// the dispatcher with `ToolError::PreconditionFailure` whose reason
    /// contains "must be the only tool call in its turn". Fixture 63
    /// asserts the rejection fires and the session does NOT transition.
    BatchedHighImpactIllegal,
}

/// Build a `TurnResponse` for the requested strategy. The `base_tool`
/// is the tool the underlying decision table would have emitted — the
/// strategy decides whether to ship it alone or fan it out into a
/// multi-tool batch.
///
/// * [`BatchStrategy::Single`] → one `tool_use` carrying `base_tool`.
/// * [`BatchStrategy::BatchedReadOnly`] → when `base_tool` is a
///   [`BatchableTool`], pair it with a sibling
///   `BatchableTool::GetClassificationEvidence` (no-arg read-only, no
///   side effects). When `base_tool` is a [`HighImpactTool`] the
///   strategy passes through as a single call — pairing it would
///   collide with the dispatcher's alone-in-turn guard, which would
///   defeat the fixture-62 goal of exercising LEGAL batching.
/// * [`BatchStrategy::BatchedHighImpactIllegal`] →
///   `HighImpactTool::EmitPackage` plus a sibling
///   `BatchableTool::GetSessionState`. The dispatcher rejects the
///   whole batch before any handler runs, so the order is irrelevant
///   — `base_tool` is ignored (the strategy always synthesizes the
///   illegal pair so the rejection is observable from the very first
///   turn).
pub fn build_batched_response(strategy: &BatchStrategy, base_tool: Tool) -> TurnResponse {
    let tool_uses = match (strategy, &base_tool) {
        (BatchStrategy::Single, _) => vec![(Uuid::new_v4(), base_tool)],
        (BatchStrategy::BatchedReadOnly, Tool::Batchable(_)) => vec![
            (Uuid::new_v4(), base_tool),
            (
                Uuid::new_v4(),
                Tool::Batchable(BatchableTool::GetClassificationEvidence),
            ),
        ],
        (BatchStrategy::BatchedReadOnly, Tool::HighImpact(_)) => {
            // HighImpact tools must stay alone-in-turn; fanning out
            // would defeat the LEGAL-batching contract this strategy
            // exists to exercise. Pass through as Single.
            vec![(Uuid::new_v4(), base_tool)]
        }
        (BatchStrategy::BatchedHighImpactIllegal, _) => vec![
            (
                Uuid::new_v4(),
                Tool::HighImpact(HighImpactTool::EmitPackage { output_dir: None }),
            ),
            (
                Uuid::new_v4(),
                Tool::Batchable(BatchableTool::GetSessionState),
            ),
        ],
    };
    TurnResponse {
        assistant_content: String::new(),
        tool_uses,
        stop_reason: StopReason::ToolUse,
        usage: Usage::default(),
        request_metadata: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_strategy_emits_exactly_one_tool() {
        let base = Tool::Batchable(BatchableTool::ClassifyIntake { prose: "x".into() });
        let resp = build_batched_response(&BatchStrategy::Single, base);
        assert_eq!(resp.tool_uses.len(), 1);
        assert!(matches!(resp.stop_reason, StopReason::ToolUse));
    }

    #[test]
    fn batched_read_only_emits_two_batchable_tools() {
        let base = Tool::Batchable(BatchableTool::ClassifyIntake { prose: "x".into() });
        let resp = build_batched_response(&BatchStrategy::BatchedReadOnly, base);
        assert_eq!(resp.tool_uses.len(), 2);
        for (_, t) in &resp.tool_uses {
            assert!(matches!(t, Tool::Batchable(_)));
        }
    }

    #[test]
    fn batched_high_impact_illegal_includes_one_high_impact() {
        let base = Tool::Batchable(BatchableTool::ClassifyIntake { prose: "x".into() });
        let resp = build_batched_response(&BatchStrategy::BatchedHighImpactIllegal, base);
        assert_eq!(resp.tool_uses.len(), 2);
        let high_impact_count = resp
            .tool_uses
            .iter()
            .filter(|(_, t)| matches!(t, Tool::HighImpact(_)))
            .count();
        assert_eq!(high_impact_count, 1);
    }

    #[test]
    fn batched_read_only_passes_high_impact_through_as_single() {
        // A HighImpact base under BatchedReadOnly must NOT be fanned
        // out — the strategy exists to exercise LEGAL batching, and
        // pairing a HighImpact tool with anything else would trip the
        // dispatcher's alone-in-turn rejection.
        let base = Tool::HighImpact(HighImpactTool::EmitPackage { output_dir: None });
        let resp = build_batched_response(&BatchStrategy::BatchedReadOnly, base);
        assert_eq!(resp.tool_uses.len(), 1);
        assert!(matches!(&resp.tool_uses[0].1, Tool::HighImpact(_)));
    }
}
