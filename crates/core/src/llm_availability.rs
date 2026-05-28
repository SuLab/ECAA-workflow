//! v3 P10 (closing v4 §6.4) — typed three-state LLM availability.
//!
//! The chat surface treats the LLM as a *proposer*, not the brain of
//! the system. Whether the LLM is reachable at all (network, quota,
//! operator policy) is an orthogonal axis from whether the deterministic
//! compiler is happy with the current intake. Capturing it as a typed
//! three-state value lets the UI fall back to the MVP structured-form
//! intake when the LLM is unreachable or disabled without polluting the
//! main chat-state machine.
//!
//! Detection is deliberately env-driven so it stays deterministic and
//! testable: the canonical sources are `ECAA_CHAT_MODE=offline` (operator
//! kill-switch) and the API-key pair (`ECAA_ANTHROPIC_API_KEY` /
//! `ANTHROPIC_API_KEY`). The conversation service caches the result per
//! session and refreshes it when an Anthropic call returns a transient
//! `Unavailable` error so the UI can re-mount the form mid-session
//! without server restart.

use serde::{Deserialize, Serialize};

/// Three-state LLM availability surfaced to the UI via
/// `GET /api/chat/llm-availability`. The tagged-enum shape matches the
/// other chat session-state wire types so the React `kind`-switch
/// idiom keeps working.
///
/// R6-U7: ts-rs export removed — the UI hand-types this in
/// `chatClient.ts` (`export type LlmAvailability =...`) and the
/// generated `ui/src/types/LlmAvailability.ts` was unused.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmAvailability {
    /// LLM is reachable and the chat surface should mount as normal.
    Available,
    /// LLM is configured but currently failing (network blip, transient
    /// Anthropic 5xx, rate-limit cooldown). Carries a human-readable
    /// reason and an optional retry hint so the UI can offer "try again
    /// in N seconds" alongside the structured-form fallback.
    Unavailable {
        /// Reason.
        reason: String,
        /// Retry after seconds.
        retry_after_seconds: Option<u64>,
    },
    /// Operator has turned the LLM off (kill-switch or no API key
    /// configured). The UI must commit to the structured form; no
    /// "retry" affordance is offered because the cause is policy, not
    /// transient failure.
    Disabled { reason: String, set_by: String },
}

impl LlmAvailability {
    /// Detect availability from the process environment. Read once at
    /// the call site (per session start) and cached by the conversation
    /// service. The compiler binary itself never calls this — only the
    /// server's chat route + the conversation service's session-start
    /// path consume the result.
    pub fn detect_from_env() -> Self {
        // Operator kill-switch first: even if an API key is present,
        // `ECAA_CHAT_MODE=offline` forces the structured-form path.
        if std::env::var("ECAA_CHAT_MODE").as_deref() == Ok("offline") {
            return Self::Disabled {
                reason: "ECAA_CHAT_MODE=offline".into(),
                set_by: "operator".into(),
            };
        }
        // No API key configured anywhere — disable regardless of mode.
        // Both env names are accepted (legacy ANTHROPIC_API_KEY support
        // matches the rest of the chat surface).
        if std::env::var("ECAA_ANTHROPIC_API_KEY").is_err()
            && std::env::var("ANTHROPIC_API_KEY").is_err()
        {
            return Self::Disabled {
                reason: "no API key configured".into(),
                set_by: "operator".into(),
            };
        }
        Self::Available
    }

    /// Convenience predicate used by the conversation service's force-mock
    /// branch — when the LLM is `Disabled`, the service mounts
    /// `MockLlmBackend` regardless of whether an API key happens to also
    /// be set. `Unavailable` does NOT force mock; the live backend stays
    /// installed so retries succeed when the upstream recovers.
    pub fn is_disabled(&self) -> bool {
        matches!(self, Self::Disabled { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Operator kill-switch takes precedence over the API-key check.
    ///
    /// Serialized on `ECAA_CHAT_MODE` so this and the
    /// `composer_offline` integration tests can't interleave each
    /// other's set/remove sequences.
    #[serial_test::serial(ECAA_CHAT_MODE)]
    #[test]
    fn offline_mode_overrides_api_key() {
        // SAFETY: env mutation inside a single-test scope. Restore
        // any prior value to avoid contaminating downstream tests.
        let prior_mode = std::env::var("ECAA_CHAT_MODE").ok();
        let prior_key = std::env::var("ECAA_ANTHROPIC_API_KEY").ok();
        std::env::set_var("ECAA_CHAT_MODE", "offline");
        std::env::set_var("ECAA_ANTHROPIC_API_KEY", "sk-doesntmatter");

        let av = LlmAvailability::detect_from_env();
        assert!(matches!(av, LlmAvailability::Disabled { .. }));

        // Restore.
        match prior_mode {
            Some(v) => std::env::set_var("ECAA_CHAT_MODE", v),
            None => std::env::remove_var("ECAA_CHAT_MODE"),
        }
        match prior_key {
            Some(v) => std::env::set_var("ECAA_ANTHROPIC_API_KEY", v),
            None => std::env::remove_var("ECAA_ANTHROPIC_API_KEY"),
        }
    }

    #[test]
    fn unavailable_is_not_disabled() {
        let av = LlmAvailability::Unavailable {
            reason: "transient 5xx".into(),
            retry_after_seconds: Some(30),
        };
        assert!(!av.is_disabled());
    }

    #[test]
    fn disabled_predicate_matches() {
        let av = LlmAvailability::Disabled {
            reason: "no api key".into(),
            set_by: "operator".into(),
        };
        assert!(av.is_disabled());
    }
}
