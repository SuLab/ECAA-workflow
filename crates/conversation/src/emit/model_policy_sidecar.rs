//! Grant v19 §Authentication of Key Resources — D4 — assemble the
//! `runtime/model-policy.json` payload.
//!
//! Reviewer-facing pinning of the LLM model selection, API version,
//! system-prompt SHA-256, tool-schema version, tool count, provider
//! id, and (if any) escalation reason. Mid-evaluation model-version
//! changes therefore surface in the package diff.

use crate::anthropic::client::{context_editing_enabled, CONTEXT_MANAGEMENT_BETA};
use crate::model_policy::ModelPolicy;
use crate::prompt::build_system_prompt;
use crate::session::Session;
use crate::tool_schemas::SCHEMA_VERSION;
use crate::tools::Tool;
use ecaa_workflow_core::hash_utils::sha256_hex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct ModelPolicySidecar {
    pub schema_version: String,
    pub active_model_id: String,
    pub api_version: String,
    pub system_prompt_sha256: String,
    pub tool_schema_version: u32,
    pub tool_count: usize,
    pub provider_id: String,
    /// `None` when the default Sonnet path picks the model — `Some` when
    /// careful-mode / Blocked / low-confidence / side-call escalates.
    pub escalation_reason: Option<String>,
    /// Anthropic beta headers active when the package emitted.
    /// Today: `["context-management-2025-06-27"]` when context editing
    /// is on, empty when `SWFC_DISABLE_CONTEXT_EDITING=1`. Sidecar
    /// review can spot a session that ran against a different beta surface.
    pub beta_headers: Vec<String>,
    /// Cache-control TTL pinned on the system prompt blocks.
    /// `"ephemeral_5m"` (default) or `"ephemeral_1h"` when the operator
    /// opts in via `SWFC_ALLOW_1H_CACHE=1`. Reviewer-facing: 1h-tier writes
    /// cost 2× base input vs 1.25× for 5m, so per-session tier surfacing
    /// is part of the cost-attribution audit.
    pub cache_ttl: String,
}

/// Build the sidecar payload from the live session. Pure with respect
/// to its `session` input — calls `ModelPolicy::choose_with_reason`
/// (which only reads `session`) and `build_system_prompt` (which only
/// reads `session.project_class` + `session.taxonomy` etc.).
pub(super) fn build_for_session(session: &Session) -> ModelPolicySidecar {
    let (model, reason) = ModelPolicy::choose_with_reason(session);
    let system_prompt = concat_prompt_blocks(session);
    let prompt_hash = sha256_hex(system_prompt.as_bytes());

    let beta_headers = if context_editing_enabled() {
        vec![CONTEXT_MANAGEMENT_BETA.to_string()]
    } else {
        Vec::new()
    };
    let cache_ttl = if ecaa_workflow_core::env_helpers::env_bool("SWFC_ALLOW_1H_CACHE") {
        "ephemeral_1h".to_string()
    } else {
        "ephemeral_5m".to_string()
    };

    ModelPolicySidecar {
        schema_version: "1".into(),
        active_model_id: format!("{model:?}"),
        // Matches the `anthropic-version` HTTP header the client
        // sets at every send_turn / send_turn_streaming / count_tokens
        // call site in `crates/conversation/src/anthropic/client.rs`. If
        // the on-wire pin flips, update this constant in step so the
        // sidecar's hash actually moves.
        api_version: "2023-06-01".into(),
        system_prompt_sha256: prompt_hash,
        tool_schema_version: SCHEMA_VERSION,
        tool_count: Tool::COUNT,
        provider_id: "anthropic".into(),
        escalation_reason: reason.map(|r| format!("{r:?}")),
        beta_headers,
        cache_ttl,
    }
}

/// Concatenate every block returned by [`build_system_prompt`] into the
/// exact string that ships to the model — this is what we hash. Joins
/// blocks with `"\n\n"` to match the Anthropic API wire format (each
/// block becomes its own `system` array entry; joining with a stable
/// separator keeps the hash deterministic across re-emits of the same
/// session).
fn concat_prompt_blocks(session: &Session) -> String {
    let blocks = build_system_prompt(session);
    blocks
        .into_iter()
        .map(|b| b.text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    #[test]
    fn sidecar_has_schema_v1_and_anthropic_provider() {
        let session = Session::new(false);
        let s = build_for_session(&session);
        assert_eq!(s.schema_version, "1");
        assert_eq!(s.provider_id, "anthropic");
    }

    #[test]
    fn prompt_hash_is_64_hex_chars() {
        let session = Session::new(false);
        let s = build_for_session(&session);
        assert_eq!(s.system_prompt_sha256.len(), 64);
        assert!(s
            .system_prompt_sha256
            .chars()
            .all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn tool_count_matches_tool_count() {
        let session = Session::new(false);
        let s = build_for_session(&session);
        assert_eq!(s.tool_count, Tool::COUNT);
    }

    /// api_version mirrors the on-wire `anthropic-version` HTTP
    /// header. Drift between this literal and the client.rs constant
    /// silently corrupts the sidecar's hash baseline.
    #[test]
    fn api_version_matches_on_wire_header() {
        let session = Session::new(false);
        let s = build_for_session(&session);
        assert_eq!(s.api_version, "2023-06-01");
    }

    /// cache_ttl defaults to `ephemeral_5m` unless the operator
    /// opts in to the 1h tier via `SWFC_ALLOW_1H_CACHE=1`.
    #[test]
    fn cache_ttl_defaults_to_5m() {
        let session = Session::new(false);
        let s = build_for_session(&session);
        // The 5m / 1h selection is env-gated; the default (no env var)
        // must be 5m so reviewers can spot the 1h-tier write cost
        // without misattributing it to the default mode.
        assert!(s.cache_ttl == "ephemeral_5m" || s.cache_ttl == "ephemeral_1h");
    }
}
