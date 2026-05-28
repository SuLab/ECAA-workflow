//! `POST /api/chat/session/:id/auto-title` — generate a short human-
//! friendly label for a session using a one-shot Haiku 4.5 call.
//!
//! The heavy lifting lives in
//! `crates/conversation/src/side_calls/generate_session_title`; this
//! file is the HTTP seam that:
//!
//! 1. Gates the feature on `SWFC_AUTO_TITLE=1`
//! 2. Enforces idempotence (a session that already has a title just
//!    returns the cached value, no LLM billing)
//! 3. Enforces the budget-overrun guard (don't burn side-call tokens on
//!    a session that's already exceeded its soft-block ceiling)
//! 4. Persists the generated title atomically via
//!    `SessionStore::update`
//!
//! Status-code contract:
//! - 200 — generated successfully, or returned the existing title on a
//!   second call (idempotent)
//! - 400 — session has fewer than 3 non-system turns; ask the user to
//!   chat a bit more first
//! - 402 — session has already exceeded its token budget; don't add
//!   side-call spend on top of a runaway session
//! - 404 — unknown session id
//! - 503 — feature flag is off (`SWFC_AUTO_TITLE` is unset or != "1")
//!
//! UI impact: `SessionTree` renders `session.title ?? session.id_short`,
//! so sessions that haven't been auto-titled (or legacy persisted
//! sessions) degrade to the short uuid without a visible error.

use super::ChatAppState;
use axum::extract::Path;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::Serialize;
use uuid::Uuid;

/// Response body for the endpoint. Kept minimal — the UI only needs the
/// title string. `from_cache` tells Vitest / operators whether the call
/// actually hit the LLM or short-circuited on the session's existing
/// title; it's `false` on first success and `true` on every subsequent
/// call for the same session.
#[derive(Debug, Clone, Serialize)]
pub(super) struct AutoTitleResponse {
    pub title: String,
    pub from_cache: bool,
}

pub(super) async fn auto_title(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    // Feature flag: off by default until operators have reviewed a few
    // Haiku-generated titles. The 503 body carries a stable string so
    // the UI can suppress the button when the server is old or the
    // operator hasn't flipped the flag. `auto_title_enabled()` consults
    // `app.auto_title_override` (test-only; `None` in production) and
    // falls back to `app.config.auto_title` (the pre-loaded boot value).
    if !app.auto_title_enabled() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "auto-title disabled (set SWFC_AUTO_TITLE=1 to enable)",
        )
            .into_response();
    }

    // Load the session. The service layer owns persistence + the per-
    // session lock; we read a clone here, then persist back via
    // SessionStore::update() below so the title write races safely with
    // concurrent turn writes.
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    // Idempotent short-circuit. A previous call has already generated a
    // title for this session — return it without invoking the LLM.
    // Matches the "once per session lifetime" billing invariant called
    // out in the plan.
    if let Some(existing) = session.title.clone() {
        return Json(AutoTitleResponse {
            title: existing,
            from_cache: true,
        })
        .into_response();
    }

    // Plan D-R4 / S5.4 — refuse to fire until the classifier has run
    // (i.e. session.classification is Some). Auto-titling a session
    // before classification produces generic placeholders since the
    // model has no signal beyond the SME's first one or two
    // sentences. The MIN_TURNS gate (6) catches the same population
    // statistically, but `has_classification` is the deterministic
    // version of "is the conversation about a real analysis yet."
    if session.classification.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "session has not been classified yet; auto-title waits until the classifier has identified a modality",
        )
            .into_response();
    }

    // Budget guard: if the session has already exceeded its soft-block
    // ceiling (rare, but possible for a runaway session that hit the
    // cap mid-turn), don't charge it another side-call Haiku call.
    // 402 keeps the UI from retrying and surfaces a clear error.
    if let Some(metrics) = app.conversation.metrics_snapshot(session_id).await {
        if let Some(budget) = metrics.session_token_budget {
            if metrics.total_input_tokens >= budget {
                return (
                    StatusCode::PAYMENT_REQUIRED,
                    format!(
                        "session input tokens ({}) have exceeded the budget ({}); \
                         auto-title suppressed to avoid additional spend",
                        metrics.total_input_tokens, budget
                    ),
                )
                    .into_response();
            }
        }
    }

    // Fire the one-shot Haiku call. The helper enforces the MIN_TURNS
    // gate internally and returns an error carrying that message so the
    // HTTP layer can surface it as 400.
    let backend = app.conversation.llm_for_scoring();
    let metrics = app.conversation.metrics();
    // Pass the archetype id (when the composer
    // pinned one via S6.9) so the auto-title surfaces as
    // `"<summary> — <archetype_id>"`.
    let archetype_id = session.archetype_snapshot.as_ref().map(|a| a.id.clone());
    match scripps_workflow_conversation::side_calls::generate_session_title(
        backend,
        metrics,
        session_id,
        &session.conversation,
        archetype_id.as_deref(),
    )
    .await
    {
        Ok(title) => {
            // Persist atomically. The closure runs while the per-
            // session Mutex is held, so a concurrent turn can't race
            // this write.
            let store = app.conversation.store_handle();
            let title_for_write = title.clone();
            match store
                .update(session_id, move |s| {
                    // Don't overwrite a title set between the read above
                    // and here (belt-and-braces — the short-circuit at
                    // the top already handles this).
                    if s.title.is_none() {
                        s.title = Some(title_for_write);
                    }
                    Ok(())
                })
                .await
            {
                Ok(_) => Json(AutoTitleResponse {
                    title,
                    from_cache: false,
                })
                .into_response(),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("failed to persist title: {}", e),
                )
                    .into_response(),
            }
        }
        Err(e) => {
            let msg = e.to_string();
            // The helper's short-conversation error carries a stable
            // substring we match on here so the HTTP layer can upgrade
            // it to a user-facing 400 instead of a generic 500. Phase
            // 4 (F3) — emit the typed `ApiError` envelope so the UI
            // can branch on `code` rather than substring-matching the
            // body.
            if msg.contains("auto-title requires at least") {
                crate::error::ApiError::BadRequest(
                    "session has too few turns for auto-titling (needs at least 3 non-system turns)"
                        .to_string(),
                )
                .into_response()
            } else {
                crate::error::ApiError::Internal(anyhow::anyhow!(msg)).into_response()
            }
        }
    }
}

/// Health hint — used by `get_state`-adjacent callers that want to
/// know whether the auto-title button should render. `None` means the
/// feature is off at this server; `Some(_)` exposes the threshold the
/// UI can cross-check against the session's turn count.
#[allow(dead_code)] // reserved-for-ui-health-hint: get_state-adjacent surface; F59 follow-up
pub(super) fn auto_title_min_turns() -> Option<usize> {
    // Standalone helper has no `ChatAppState` handle, so it falls back
    // to the env-var read directly. Routes that DO have ChatAppState
    // use `ChatAppState::auto_title_enabled` which prefers the test
    // override + falls back to `config.auto_title`.
    scripps_workflow_core::env_helpers::env_bool("SWFC_AUTO_TITLE")
        .then_some(scripps_workflow_conversation::side_calls::AUTO_TITLE_MIN_TURNS)
}

/// Route inventory for the doc-as-contract gate +
/// route-count parity assertion against CLAUDE.md. (METHOD, PATH).
pub(super) const ROUTES: &[(&str, &str)] = &[("POST", "/api/chat/session/:id/auto-title")];

/// Per-domain `routes()` builder. `mod.rs::router()`
/// merges every submodule's builder into the single chat surface.
pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/auto-title",
        axum::routing::post(auto_title),
    )
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup/teardown; the bounded waiver is
    // scoped to this `mod tests` block.
    #![allow(unsafe_code)]
    use crate::chat_routes::test_support::{
        assistant, body_json, make_router, make_router_with_auto_title_enabled,
    };
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tokio::sync::Mutex;
    use tower::util::ServiceExt;

    /// Serialize env-var mutation across the tests below. The
    /// process-wide env is shared state; parallel test runs otherwise
    /// flip the flag under each other's feet. tokio::sync::Mutex (vs
    /// std::sync::Mutex) is async-aware, so holding the guard across
    /// the test's `.await` calls doesn't trigger
    /// `clippy::await_holding_lock`.
    static AUTO_TITLE_ENV_LOCK: Mutex<()> = Mutex::const_new(());

    /// RAII env guard for the disabled-flag test. The enabled-flag
    /// tests went through `make_router_with_auto_title_enabled` so they
    /// no longer touch the process-wide env table; only the 503 test
    /// still needs to ensure `SWFC_AUTO_TITLE` is unset.
    struct AutoTitleEnvGuard(Option<String>);
    impl AutoTitleEnvGuard {
        fn set_disabled() -> Self {
            let prior = std::env::var("SWFC_AUTO_TITLE").ok();
            // SAFETY: callers hold AUTO_TITLE_ENV_LOCK; single-writer.
            unsafe { std::env::remove_var("SWFC_AUTO_TITLE") };
            Self(prior)
        }
    }
    impl Drop for AutoTitleEnvGuard {
        fn drop(&mut self) {
            match &self.0 {
                Some(v) => unsafe { std::env::set_var("SWFC_AUTO_TITLE", v) },
                None => unsafe { std::env::remove_var("SWFC_AUTO_TITLE") },
            }
        }
    }

    /// Seed a session with `n` non-system turns + (optionally) a
    /// classification record so the auto-title helper's MIN_TURNS gate
    /// AND the post-S5.4 `has_classification` gate are both satisfied.
    /// Uses the public `update` path so the test doesn't have to know
    /// the internal Session layout.
    async fn seed_session_with_n_turns(
        app: &crate::chat_routes::ChatAppState,
        n: usize,
    ) -> uuid::Uuid {
        seed_session_with_n_turns_classified(app, n, true).await
    }

    async fn seed_session_with_n_turns_classified(
        app: &crate::chat_routes::ChatAppState,
        n: usize,
        with_classification: bool,
    ) -> uuid::Uuid {
        let (id, _) = app.conversation.start_session(false).await.unwrap();
        let store = app.conversation.store_handle();
        store
            .update(id, |s| {
                use scripps_workflow_conversation::Turn;
                let mut turns: Vec<Turn> = Vec::new();
                for i in 0..n {
                    if i % 2 == 0 {
                        turns.push(Turn::user(format!("user turn {}", i)));
                    } else {
                        turns.push(Turn::assistant(format!("assistant turn {}", i)));
                    }
                }
                s.conversation = std::sync::Arc::new(turns);
                if with_classification {
                    s.classification =
                        Some(scripps_workflow_core::classify::ClassificationResult {
                            modality: "single_cell_rnaseq".into(),
                            taxonomy_path: String::new(),
                            domain: String::new(),
                            workflow_description: String::new(),
                            edam_topic: String::new(),
                            edam_operation: String::new(),
                            confidence: 0.9,
                            confidence_label: "high".into(),
                            organisms: vec![],
                            methods_specified: vec![],
                            data_sources: vec![],
                            intake_text: String::new(),
                            goal: None,
                            archetype_id: None,
                            additional_modalities: vec![],
                            tie_candidates: vec![],
                        });
                }
                Ok(())
            })
            .await
            .unwrap();
        id
    }

    #[tokio::test]
    async fn returns_503_when_feature_flag_is_off() {
        let _lock = AUTO_TITLE_ENV_LOCK.lock().await;
        let _guard = AutoTitleEnvGuard::set_disabled();
        let (router, app) = make_router(vec![]).await;
        let id = seed_session_with_n_turns(&app, 3).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn returns_404_for_unknown_session() {
        // Auto-title pinned via `auto_title_override` rather than the
        // `SWFC_AUTO_TITLE` env-var so parallel test runs can't race
        // each other's env mutations.
        let (router, _app) = make_router_with_auto_title_enabled(vec![]).await;
        let fake = uuid::Uuid::new_v4();
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", fake))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn returns_400_for_sessions_below_min_turns() {
        // Auto-title pinned via `auto_title_override`; see the
        // 404-unknown-session test for the rationale.
        let (router, app) = make_router_with_auto_title_enabled(vec![]).await;
        // Below the MIN_TURNS=6 gate (post-S5.4) — even with
        // classification set, the helper rejects.
        let id = seed_session_with_n_turns(&app, 4).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn generates_title_and_persists_then_idempotent_second_call() {
        // MockLlmBackend pops scripted responses in order; auto-title
        // issues exactly one `send_turn` on a successful run, so one
        // canned response covers it. The second call short-circuits on
        // the persisted title and never hits the backend. Auto-title is
        // pinned via `auto_title_override`; see the 404 test.
        let (router, app) =
            make_router_with_auto_title_enabled(vec![assistant("Bulk RNA-seq DE liver")]).await;
        // Post-S5.4 gate: 6 non-system turns + classification both
        // required.
        let id = seed_session_with_n_turns(&app, 6).await;

        // First call — should generate and persist.
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["title"], "Bulk RNA-seq DE liver");
        assert_eq!(body["from_cache"], false);

        // Verify persistence: the session's title field is now populated.
        let session = app.conversation.get_session(id).await.unwrap();
        assert_eq!(session.title.as_deref(), Some("Bulk RNA-seq DE liver"));

        // Second call — must short-circuit. If it hit the backend again
        // the MockLlmBackend would return the exhausted-turn error.
        let req2 = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", id))
            .body(Body::empty())
            .unwrap();
        let resp2 = router.oneshot(req2).await.unwrap();
        assert_eq!(resp2.status(), StatusCode::OK);
        let body2 = body_json(resp2.into_body()).await;
        assert_eq!(body2["title"], "Bulk RNA-seq DE liver");
        assert_eq!(body2["from_cache"], true);
    }

    #[tokio::test]
    async fn config_endpoint_reflects_flag_state() {
        // GET /api/chat/config surfaces auto_title_enabled + MIN_TURNS
        // so the UI can hide the button pre-emptively. Exercises the
        // true-flag path here (pinned via `auto_title_override`); the
        // disabled path is covered by the 503 test above.
        let (router, _app) = make_router_with_auto_title_enabled(vec![]).await;
        let req = Request::builder()
            .method("GET")
            .uri("/api/chat/config")
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp.into_body()).await;
        assert_eq!(body["auto_title_enabled"], true);
        // Post-S5.4: MIN_TURNS bumped from 3 to 6.
        assert_eq!(body["auto_title_min_turns"], 6);
    }

    #[tokio::test]
    async fn returns_400_when_classification_is_missing() {
        // Plan S5.4 — auto-title refuses to fire on unclassified
        // sessions even when MIN_TURNS is satisfied. A classifier
        // signal is the deterministic version of "this conversation
        // is about a real analysis." Auto-title pinned via
        // `auto_title_override`; see the 404 test.
        let (router, app) = make_router_with_auto_title_enabled(vec![]).await;
        // 6 turns satisfies MIN_TURNS, but classification is None.
        let id = seed_session_with_n_turns_classified(&app, 6, false).await;
        let req = Request::builder()
            .method("POST")
            .uri(format!("/api/chat/session/{}/auto-title", id))
            .body(Body::empty())
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
