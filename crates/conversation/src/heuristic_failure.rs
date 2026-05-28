//! Failure injection for the [`HeuristicMockBackend`].
//!
//! Tests can request that a specific tool dispatch return a hard
//! `ToolError::ValidationFailure` on its next invocation. The heuristic
//! backend produces the failing tool call by emitting a payload the
//! deterministic dispatcher rejects (a `set_intake_field` against an
//! invented stage id), so the failure stays inside the LLM-side
//! fiction — the dispatcher's real validation produces the error,
//! we don't reach across the tool/server boundary.
//!
//! This is the primitive the failure-injection fixture batch uses to surface
//! `BlockerKind::ToolError` paths to the remediation_proposer side-call
//! without depending on the live harness or a real backend failure.

use crate::tools::{BatchableTool, Tool};
use std::sync::atomic::{AtomicBool, Ordering};

/// One-shot failure spec consumed by [`HeuristicMockBackend`].
///
/// `fired` is an [`AtomicBool`] so `check_and_fire` can be called from
/// the backend's `&self` decision path under `LlmBackend: Send + Sync`.
/// `std::cell::Cell` would have been simpler but it is `!Sync` and the
/// `LlmBackend` trait bound requires `Sync`. The latch flips to `true`
/// on the first match; subsequent dispatches see `None` and fall
/// through to the normal decision table.
#[derive(Debug)]
pub struct FailureInjection {
    /// Tool the heuristic was about to dispatch when the injection
    /// should fire. Matched case-sensitively against the canonical
    /// `Tool::name()` strings (e.g. `"set_intake_field"`,
    /// `"append_intake_prose"`).
    pub tool_name: String,
    /// Operator-supplied free-form reason. Surfaced inside the
    /// synthetic failing tool call's payload so a fixture rubric_notes
    /// reader can correlate the dispatcher rejection with the inject
    /// site.
    pub reason: String,
    /// One-shot latch. `false` at construction; flips to `true` once
    /// `check_and_fire` has returned `Some` for any matching tool.
    pub fired: AtomicBool,
}

impl Clone for FailureInjection {
    fn clone(&self) -> Self {
        Self {
            tool_name: self.tool_name.clone(),
            reason: self.reason.clone(),
            fired: AtomicBool::new(self.fired.load(Ordering::Relaxed)),
        }
    }
}

impl FailureInjection {
    /// Create a one-shot failure injection for the given tool name and reason.
    pub fn new(tool_name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            reason: reason.into(),
            fired: AtomicBool::new(false),
        }
    }

    /// One-shot check. Returns `Some(reason)` exactly once when the
    /// dispatching tool name matches `self.tool_name`; subsequent calls
    /// always return `None`. Pure (no I/O); safe to call from the
    /// heuristic's `&self` decision path.
    pub fn check_and_fire(&self, dispatching_tool: &str) -> Option<&str> {
        if dispatching_tool != self.tool_name {
            return None;
        }
        // `compare_exchange` is the atomic equivalent of the
        // load+test+set the `Cell`-based draft used: only one caller
        // can observe `false` and atomically flip it to `true`. Order
        // is `Relaxed` — the latch carries no other state.
        match self
            .fired
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        {
            Ok(_) => Some(self.reason.as_str()),
            Err(_) => None,
        }
    }

    /// Build a `Tool` whose dispatch the deterministic validation
    /// pipeline will reject with `ToolError::ValidationFailure`.
    ///
    /// The trick is to emit a `set_intake_field` against an obviously
    /// invented stage id. `crates/conversation/src/tools/intake.rs::
    /// set_intake_field` checks the composed DAG and returns
    /// `ToolError::ValidationFailure { reason: "unknown stage ..." }`
    /// when no task matches. That error path is the same shape a
    /// `BlockerKind::ToolError` would surface from a real harness
    /// failure (the proposer side-call consumes the envelope; here
    /// the dispatcher's rejection stands in for it inside fixtures).
    ///
    /// The injected `reason` is embedded in the field value so a
    /// fixture rubric_notes reader can grep for it in the tool_call_log.
    pub fn synthetic_failing_tool_call(&self) -> Tool {
        Tool::Batchable(BatchableTool::SetIntakeField {
            stage: "__failure_injection_invalid_stage__".to_string(),
            field: "__injected_failure__".to_string(),
            value: serde_json::json!({
                "reason": self.reason,
                "source": "heuristic_failure_injection",
            }),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_and_fire_returns_reason_once() {
        let inj = FailureInjection::new("set_intake_field", "simulated dispatcher failure");
        let first = inj.check_and_fire("set_intake_field");
        assert_eq!(first, Some("simulated dispatcher failure"));
        // Second call against the same tool is `None` — one-shot latch.
        assert_eq!(inj.check_and_fire("set_intake_field"), None);
    }

    #[test]
    fn check_and_fire_ignores_non_matching_tool() {
        let inj = FailureInjection::new("set_intake_field", "x");
        assert_eq!(inj.check_and_fire("classify_intake"), None);
        // The latch is still un-fired; the matching tool can still trigger it.
        assert_eq!(inj.check_and_fire("set_intake_field"), Some("x"));
    }

    #[test]
    fn synthetic_failing_tool_call_uses_invented_stage() {
        let inj = FailureInjection::new("set_intake_field", "boom");
        match inj.synthetic_failing_tool_call() {
            Tool::Batchable(BatchableTool::SetIntakeField { stage, value, .. }) => {
                assert!(
                    stage.contains("invalid"),
                    "stage must be obviously invented so DAG validation rejects it: {}",
                    stage
                );
                let payload = value.to_string();
                assert!(
                    payload.contains("boom"),
                    "reason must be threaded through the synthetic payload: {}",
                    payload
                );
            }
            other => panic!("expected SetIntakeField synthetic tool, got {other:?}"),
        }
    }
}
