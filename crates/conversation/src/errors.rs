//! Tool dispatch error contract — see remediation plan §4.5.
//!
//! Single top-level `ConversationError` enum (defined
//! in this module) wraps the per-layer error types via `#[from]`:
//! `ToolError` (tool dispatch), `ServiceError` (service / Anthropic
//! backend), `TransitionError` (state-machine), plus
//! `anyhow::Error` for catch-all internal context. Sub-cases stay
//! independent — sideways conversions between siblings (e.g.
//! `ToolError → ServiceError`) are deliberately NOT provided.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Error returned from the tool dispatch layer to the LLM. Serialized
/// directly into the `tool_result` content block so the LLM can
/// understand and paraphrase it for the SME.
#[derive(Debug, Clone, Serialize, Deserialize, TS, thiserror::Error)]
#[ts(export)]
#[serde(tag = "error_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolError {
    /// A tool argument failed semantic validation (e.g. unknown stage id, blank field).
    #[error("validation_failure: {reason}")]
    ValidationFailure {
        /// Human-readable reason suitable for LLM paraphrase.
        reason: String,
        /// Suggested valid alternatives (may be empty when no alternatives exist).
        valid_alternatives: Vec<String>,
        /// LLM-actionable hint for how to retry.
        hint: String,
    },
    /// A session-state precondition was not met (e.g. `emit_package` before confirm).
    #[error("precondition_failure: {reason}")]
    PreconditionFailure {
        /// Description of the unmet precondition.
        reason: String,
        /// LLM-actionable hint for how to resolve.
        hint: String,
    },
    /// An unexpected server-side error occurred.
    #[error("internal_error: {reason}")]
    InternalError {
        /// Internal error message (may reference crate-internal names; LLM should paraphrase).
        reason: String,
    },
    /// The Anthropic API returned a 429; caller should back off.
    #[error("rate_limited: retry after {retry_after_ms}ms")]
    RateLimited {
        /// Suggested delay in milliseconds before retrying.
        retry_after_ms: u64,
    },
    /// Returned by `amend_stage_method` / `rerun_task` when the target
    /// stage is prespecified in a confirmatory session and no non-empty
    /// rationale was supplied. The LLM should surface the hint to the
    /// SME and re-ask.
    #[error("rationale_required: stage {stage}")]
    RationaleRequired {
        /// The stage id that requires a rationale.
        stage: String,
        /// Guidance the LLM should relay to the SME.
        hint: String,
    },
}

impl ToolError {
    /// Constructor for the "this string field is empty" pattern
    /// that repeats across `set_intake_method`, `amend_stage_method`,
    /// and `append_intake_prose`. Callers pass the field name plus a
    /// hint describing what the SME should provide. The `reason`
    /// field is LLM-facing (it becomes part of the `tool_result`
    /// body the LLM paraphrases); keep it phrased so the LLM can
    /// translate it into SME prose without leaking "field" / "arg".
    pub fn empty_string(field: &str, hint: &str) -> Self {
        ToolError::ValidationFailure {
            reason: format!("the `{}` value was missing or blank", field),
            valid_alternatives: vec![],
            hint: hint.to_string(),
        }
    }

    /// Constructor for the "that stage id isn't in the DAG" pattern
    /// shared by `set_intake_method`, `amend_stage_method`, and
    /// `select_sensitivity_winner`. The alternatives list is the
    /// handful of DAG stage ids the caller pre-computes for SME
    /// guidance. The reason avoids the internal word "stage" so the
    /// LLM's paraphrase stays in SME vocabulary ("step" in the plan).
    pub fn unknown_stage(stage: &str, alternatives: Vec<String>, hint: &str) -> Self {
        ToolError::ValidationFailure {
            reason: format!("the step `{}` is not in this plan", stage),
            valid_alternatives: alternatives,
            hint: hint.to_string(),
        }
    }

    /// Constructor for the "the session state doesn't permit this
    /// tool" precondition failure. Reason + hint are LLM-facing and
    /// reference the `SessionState` enum by name because the LLM is
    /// expected to map those to SME language ("still gathering intake",
    /// "waiting for Confirm", etc.) per prompt_role.txt.
    pub fn wrong_state(expected: &str, actual: impl std::fmt::Display) -> Self {
        ToolError::PreconditionFailure {
            reason: format!("expected session state {}, got {}", expected, actual),
            hint: format!("This tool only runs when the session is in {}.", expected),
        }
    }

    /// Constructor for the "no taxonomy loaded — classify first"
    /// precondition failure surfaced by the intake-mutation tools. The
    /// reason deliberately avoids the word "taxonomy" (forbidden in
    /// SME-facing prose by prompt_role.txt) so a naive paraphrase
    /// doesn't leak it. The hint points at the specific tool to call
    /// — that's LLM-actionable, and the LLM is expected to rephrase
    /// the tool name to SME language ("capture the user's prose
    /// description of the analysis first").
    pub fn no_taxonomy() -> Self {
        ToolError::PreconditionFailure {
            reason: "the analysis hasn't been classified yet".into(),
            hint: "Call `append_intake_prose` with the user's latest prose so the system can \
                   classify the analysis and load its plan."
                .into(),
        }
    }

    /// Short reason string for the post-handler error
    /// hooks. Returns the underlying `reason` field for variants that
    /// have one, or the variant name otherwise. Used by
    /// `emit_package_post_err` to populate `EmitPackageErr.reason`.
    pub fn short_reason(&self) -> String {
        match self {
            ToolError::ValidationFailure { reason, .. } => reason.clone(),
            ToolError::PreconditionFailure { reason, .. } => reason.clone(),
            ToolError::InternalError { reason } => reason.clone(),
            ToolError::RateLimited { retry_after_ms } => {
                format!("rate-limited; retry after {}ms", retry_after_ms)
            }
            ToolError::RationaleRequired { stage, .. } => {
                format!("rationale required for amend on `{}`", stage)
            }
        }
    }
}

/// Result returned to the LLM for a single tool dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ToolResult {
    /// Whether the tool call produced an error. The LLM uses this flag
    /// to decide whether to retry or surface a message to the SME.
    pub is_error: bool,
    /// Structured result or error payload. On success, tool-specific
    /// JSON; on error, a serialized `ToolError`.
    pub content: serde_json::Value,
}

impl ToolResult {
    /// Construct a successful `ToolResult` with the given JSON payload.
    pub fn ok(content: serde_json::Value) -> Self {
        Self {
            is_error: false,
            content,
        }
    }

    /// Construct an error `ToolResult` by serializing a `ToolError`.
    pub fn err(error: ToolError) -> Self {
        Self {
            is_error: true,
            content: serde_json::to_value(&error).expect("ToolError serialization"),
        }
    }
}

/// Single top-level error enum. Wraps the per-layer
/// error types via `#[from]` so callers in `crates/conversation`
/// can write `fn foo() -> Result<(), ConversationError>` and use
/// `?` against any of the sub-types. Sub-types stay independent
/// (no sideways conversions).
///
/// `anyhow::Error` is included as a catch-all so legacy code that
/// returns `anyhow::Error` interoperates without a forced rewrite —
/// the migration is gradual.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConversationError {
    /// A tool-dispatch error (argument validation, precondition, rate-limit).
    #[error(transparent)]
    Tool(#[from] ToolError),
    /// A service-layer error (Anthropic backend, LLM loop).
    #[error(transparent)]
    Service(#[from] crate::service::ServiceError),
    /// A state-machine transition error (invalid state change).
    #[error(transparent)]
    Transition(#[from] crate::session::TransitionError),
    /// Catch-all for errors that haven't been given a dedicated variant yet.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_failure_serializes_with_alternatives() {
        let e = ToolError::ValidationFailure {
            reason: "field 'foo' not found on stage 'preprocessing'".into(),
            valid_alternatives: vec!["bar".into(), "baz".into()],
            hint: "Did you mean 'bar'?".into(),
        };
        let result = ToolResult::err(e);
        let v = result.content.as_object().unwrap();
        assert_eq!(v["error_kind"], "validation_failure");
        assert!(v["valid_alternatives"].as_array().unwrap().len() == 2);
        assert!(result.is_error);
    }

    #[test]
    fn precondition_failure_carries_hint() {
        let e = ToolError::PreconditionFailure {
            reason: "user_confirmed is false".into(),
            hint: "Call propose_summary_confirmation first.".into(),
        };
        let r = ToolResult::err(e);
        assert!(r.is_error);
        assert_eq!(r.content["error_kind"], "precondition_failure");
    }

    #[test]
    fn ok_result_is_not_error() {
        let r = ToolResult::ok(serde_json::json!({"x": 1}));
        assert!(!r.is_error);
        assert_eq!(r.content["x"], 1);
    }

    /// `ConversationError` wraps the per-layer enums so
    /// `?` in conversation-crate code returns a single error type.
    /// Each `#[from]` impl is mechanical; this is the smoke test that
    /// confirms each conversion compiles + preserves the underlying
    /// Display.
    #[test]
    fn conversation_error_wraps_subtypes_via_from() {
        // ToolError → ConversationError
        let te = ToolError::ValidationFailure {
            reason: "boom".into(),
            valid_alternatives: vec![],
            hint: "fix".into(),
        };
        let ce: super::ConversationError = te.into();
        assert!(matches!(ce, super::ConversationError::Tool(_)));

        // ServiceError → ConversationError
        let se = crate::service::ServiceError::Internal("svc-broken".into());
        let ce: super::ConversationError = se.into();
        assert!(format!("{}", ce).contains("svc-broken"));

        // anyhow::Error → ConversationError (catch-all)
        let ae: anyhow::Error = anyhow::anyhow!("misc");
        let ce: super::ConversationError = ae.into();
        assert!(matches!(ce, super::ConversationError::Other(_)));
    }
}
