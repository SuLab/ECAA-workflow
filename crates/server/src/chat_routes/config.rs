//! `GET /api/chat/config` — read-only feature-flag + policy discovery
//! endpoint for the UI. Lets the browser know whether to surface
//! optional affordances (auto-title button, future side-call features)
//! without having to probe a route and interpret a 503.
//!
//! The payload is deliberately small and stable. Every field is
//! optional-by-default on the UI side so a new client against an old
//! server (field absent) degrades to the same behavior as a flag that
//! happens to be off.

use super::ChatAppState;
use axum::{extract::State, response::IntoResponse, Json};
use serde::Serialize;

/// Response shape. Mirrors the UI's `ChatConfig` interface; field
/// names are snake_case to match the rest of the chat API.
#[derive(Debug, Clone, Serialize)]
pub(super) struct ChatConfig {
    /// True when `SWFC_AUTO_TITLE=1` is set. The UI uses this to hide
    /// the "Auto-name session" button on servers / deployments that
    /// haven't opted into the Haiku side call yet.
    pub auto_title_enabled: bool,
    /// Minimum non-system turn count required before the auto-title
    /// route will succeed (matches
    /// `scripps_workflow_conversation::side_calls::AUTO_TITLE_MIN_TURNS`).
    /// The UI disables the button below this threshold so users don't
    /// click it just to see the 400 response.
    pub auto_title_min_turns: usize,
}

pub(super) async fn get_config(State(app): State<ChatAppState>) -> impl IntoResponse {
    // `auto_title_enabled()` consults `app.auto_title_override` first
    // (test-only pin; `None` in production) and falls back to
    // `app.config.auto_title` (the pre-loaded boot value).
    let auto_title_enabled = app.auto_title_enabled();
    Json(ChatConfig {
        auto_title_enabled,
        auto_title_min_turns: scripps_workflow_conversation::side_calls::AUTO_TITLE_MIN_TURNS,
    })
    .into_response()
}

/// Route inventory for the doc-as-contract gate.
pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/config")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route("/api/chat/config", axum::routing::get(get_config))
}
