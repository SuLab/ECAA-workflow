//! `GET /api/chat/llm-availability` — v3 P10, closes v4 §6.4.
//!
//! Read-only endpoint the UI polls at mount time so it can decide
//! between the conversational `<Chat>` surface and the MVP
//! `<StructuredIntakeForm>` fallback when the LLM is `Disabled` or
//! `Unavailable`. The detection runs against the process env via
//! [`LlmAvailability::detect_from_env`] (operator kill-switch +
//! API-key probe) so the same answer is shared by every session
//! started on this server.
//!
//! Per-session availability cached on `ConversationService` is a
//! separate code path used by the tool loop's mock-fallback decision;
//! this endpoint stays env-only because the UI's mount-time decision
//! has no session id yet.

use super::ChatAppState;
use axum::{response::Json, Router};
use scripps_workflow_core::llm_availability::LlmAvailability;

/// Handler — pure env detection. No state needed; the `ChatAppState`
/// generic is only kept for router-merge compatibility.
pub(super) async fn get_llm_availability() -> Json<LlmAvailability> {
    Json(LlmAvailability::detect_from_env())
}

/// Route inventory for the doc-as-contract gate.
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/llm-availability")];

pub(super) fn routes() -> Router<ChatAppState> {
    Router::new().route(
        "/api/chat/llm-availability",
        axum::routing::get(get_llm_availability),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handler_returns_one_of_the_three_kinds() {
        let resp = get_llm_availability().await;
        let av = resp.0;
        // The handler should always return one of the three variants.
        // We accept any of them because the test env may or may not
        // have an API key configured.
        match av {
            LlmAvailability::Available
            | LlmAvailability::Unavailable { .. }
            | LlmAvailability::Disabled { .. } => {}
        }
    }
}
