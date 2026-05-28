//! HTTP routes for the LLM-mediated conversation layer. Mounted under
//! `/api/chat/...`. Coexists with the legacy classifier-based routes
//! in `main.rs`.
//!
//! This file is the merge-only entry point. Shared infra
//! lives in dedicated submodules:
//! - `app_state.rs` — `ChatAppState`, `ExecutionHandle`, `RateBucket`.
//! - `wire_types.rs` — JSON contract types (`SsePayload`,
//!   `SessionStateSnapshot`, harness wire mirrors).
//! - `event_sink.rs` — `BroadcastEventSink` bridge.
//!
//! Each per-domain submodule (sessions, turns, branches, …) exposes a
//! `pub fn routes()` builder + `pub const ROUTES` inventory.

// Imports surfaced to per-domain submodules via `use super::*;`.
// `#[allow(unused_imports)]` because each name is consumed by at least
// one submodule's glob-import; the local crate-root only references
// `Router`.
#[allow(unused_imports)]
use axum::{extract::Path, Router};
#[allow(unused_imports)]
use ecaa_workflow_conversation::{
    AnthropicClient, BatcherConfig, ConversationService, HarnessBatcher, LlmBackend,
    MockLlmBackend, ServiceEventSink, SessionId, SessionStore, Turn,
};
#[allow(unused_imports)]
use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use std::path::PathBuf;
#[allow(unused_imports)]
use std::sync::Arc;
#[allow(unused_imports)]
use tokio::sync::{broadcast, RwLock};
#[allow(unused_imports)]
use uuid::Uuid;

/// Unauthenticated health router. Mounts `/healthz` (liveness — always 200)
/// and `/readyz` (readiness — 503 on config/filesystem failures). These
/// routes MUST be merged onto the outer application router AFTER all auth
/// layers so load balancers and uptime monitors can probe without tokens.
pub fn health_router(app: ChatAppState) -> Router {
    use axum::routing::get;
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(std::sync::Arc::new(app))
}

/// Top-level chat router. Per-domain submodules each
/// expose `pub fn routes() -> Router<ChatAppState>` and
/// `pub const ROUTES: &[(&str, &str)]` (method, path); this fn merges
/// them all into the single chat surface and applies state once at the
/// end. Adding or removing a route now happens entirely inside the
/// owning submodule — `mod.rs` stays merge-only and small.
///
/// Every submodule's routes are mounted twice: at
/// `/api/chat/...` (the legacy surface) and at `/api/v1/chat/...` (the
/// versioned surface). The v1 surface is implemented as a catch-all
/// route on the same router that rewrites the URI to the canonical
/// `/api/chat/...` form and dispatches via a cloned router service.
/// `axum::middleware::from_fn` cannot do this because `Router::layer`
/// only wraps matched-route handlers — a URI that doesn't match any
/// `/api/chat/*` route never enters the layer chain, so the rewrite
/// would never fire.
pub fn router(app: ChatAppState) -> Router {
    let chat: Router<ChatAppState> = Router::new()
        .merge(sessions::routes())
        .merge(turns::routes())
        .merge(branches::routes())
        .merge(events::routes())
        .merge(execution::routes())
        .merge(tasks::routes())
        .merge(verification::routes())
        .merge(dashboard::routes())
        .merge(summary::routes())
        .merge(share::routes())
        .merge(remediation::routes())
        .merge(auto_title::routes())
        .merge(explain::routes())
        .merge(budget::routes())
        .merge(config::routes())
        .merge(stage_descriptions::routes())
        .merge(inputs::routes())
        .merge(install_log::routes())
        .merge(dispositions::routes())
        .merge(compose::routes())
        .merge(hypothesized_renderer::routes())
        .merge(llm_availability::routes())
        .merge(decision_substrate::routes())
        .merge(unblock_paths::routes())
        .merge(adjudication::routes())
        .merge(repair_proposals::routes())
        .merge(proposal::routes())
        .merge(population_coverage::routes())
        .merge(graduation_candidates::routes())
        .merge(value_add_metrics::routes())
        .merge(atoms::routes())
        .merge(literature_context::routes())
        .merge(stall_signal::routes());
    let canonical: Router = chat.with_state(app);
    let canonical_for_v1 = canonical.clone();
    // `nest_service` strips the `/api/v1/chat` prefix from `req.uri()`
    // before our forwarding service runs. We then re-prepend
    // `/api/chat` so the rewritten URI matches the canonical route
    // tree. Using `nest_service` (instead of a `*rest` catch-all
    // route) is load-bearing: axum's nest machinery tags its
    // wildcard capture with the internal `NEST_TAIL_PARAM` name and
    // the per-request `UrlParams` machinery filters that tag out
    // before downstream `Path<…>` extractors see it. A literal
    // `Router::route("/api/v1/chat/*rest", …)` does NOT get this
    // filtering: the captured `rest` param leaks into the cloned
    // router's `Path<Uuid>` extraction and surfaces as "Wrong number
    // of path arguments".
    let v1_forwarder = tower::service_fn(move |mut req: axum::extract::Request| {
        let inner = canonical_for_v1.clone();
        async move {
            // Re-prepend `/api/chat` to the path-and-query so any
            // `?cursor=…&limit=…` (or other) query parameters survive
            // the rewrite. Using `path_and_query()` instead of the
            // path-only `path()` is load-bearing: the cursor pagination
            // contract round-trips `?cursor=` / `?limit=` through this
            // forwarder, and reading the path alone silently drops the
            // query — causing every paginated v1 GET to land on the
            // canonical handler with an empty query map.
            let suffix = req
                .uri()
                .path_and_query()
                .map(|pq| pq.as_str())
                .unwrap_or("/");
            let new_uri_str = format!("/api/chat{suffix}");
            if let Ok(new_uri) = new_uri_str.parse::<axum::http::Uri>() {
                *req.uri_mut() = new_uri;
            }
            tower::ServiceExt::oneshot(inner, req).await
        }
    });
    canonical.nest_service("/api/v1/chat", v1_forwarder)
}

/// Concatenated route inventory across every submodule,
/// used by the `documented_constants.rs` doc-as-contract gate to
/// assert the route count documented in CLAUDE.md matches what the
/// router actually exposes. The flat slice is built at compile time
/// from each submodule's `ROUTES` const so there's no drift between
/// what the gate sees and what the router serves.
pub const ALL_ROUTES: &[&[(&str, &str)]] = &[
    sessions::ROUTES,
    turns::ROUTES,
    branches::ROUTES,
    events::ROUTES,
    execution::ROUTES,
    tasks::ROUTES,
    verification::ROUTES,
    dashboard::ROUTES,
    summary::ROUTES,
    share::ROUTES,
    remediation::ROUTES,
    auto_title::ROUTES,
    explain::ROUTES,
    budget::ROUTES,
    config::ROUTES,
    stage_descriptions::ROUTES,
    inputs::ROUTES,
    install_log::ROUTES,
    dispositions::ROUTES,
    compose::ROUTES,
    hypothesized_renderer::ROUTES,
    llm_availability::ROUTES,
    decision_substrate::ROUTES,
    unblock_paths::ROUTES,
    adjudication::ROUTES,
    repair_proposals::ROUTES,
    proposal::ROUTES,
    population_coverage::ROUTES,
    graduation_candidates::ROUTES,
    value_add_metrics::ROUTES,
    atoms::ROUTES,
    literature_context::ROUTES,
    stall_signal::ROUTES,
];

/// Total chat-route count summed across every submodule's `ROUTES`.
/// Used by `documented_constants.rs` to gate route-count parity with
/// CLAUDE.md without recomputing the slice lengths every call site.
pub const fn total_route_count() -> usize {
    let mut n = 0usize;
    let mut i = 0;
    while i < ALL_ROUTES.len() {
        n += ALL_ROUTES[i].len();
        i += 1;
    }
    n
}

// per-domain submodules. Handlers are re-exported
// via `pub use` so `router()` in this file can reference them by their
// bare names without knowing which submodule each lives in.
mod _client_ip;
mod _etag;
mod _git_hook_pool;
mod _idempotency;
mod _json_depth;
mod _pagination;
mod _path_jail;
pub(crate) mod _rate_limits;
pub(crate) mod _session_lock;
mod adjudication;
pub(crate) mod app_state;
mod atoms;
mod auto_title;
mod branches;
mod budget;
mod compose;
mod config;
mod dashboard;
mod decision_substrate;
pub(crate) mod dispositions;
mod event_sink;
mod events;
mod execution;
mod explain;
mod graduation_candidates;
pub mod health;
mod hypothesized_renderer;
mod inputs;
mod install_log;
mod literature_context;
mod llm_availability;
mod population_coverage;
mod proposal;
mod remediation;
mod repair_proposals;
mod sessions;
// `pub mod` so `read_only.rs` can call
// `share::hash_share_token` for the constant-time hash compare.
pub mod share;
mod stage_descriptions;
mod stall_signal;
mod summary;
mod tasks;
mod turns;
mod unblock_paths;
mod value_add_metrics;
mod verification;
mod wire_types;

// Re-export the path-jail helpers so per-domain submodules
// can call them as `super::safe_segment_join` (top-level) or
// `super::super::safe_segment_join` (nested tasks/, dispositions/). The
// `pub` visibility (not `pub(crate)`) is load-bearing: the
// `crates/server/tests/path_jail_fuzz.rs` integration test reaches the
// helpers through the same import path the production handlers use, so
// they need to be reachable as `ecaa_workflow_server::chat_routes::*`.
pub use _path_jail::{
    assert_under_root, runtime_outputs_for_task, safe_relative_join, safe_segment_join,
    PathJailError,
};

// Re-export the bounded git-hook pool and its drop-notifier type so
// call sites can wire SSE fanouts for dropped hooks and integration
// tests can reach them at the same import path the production handlers
// use.
pub use _git_hook_pool::{DropNotifier, GitHookPool};

// Re-export the server-side session-store flock so `lib.rs` can
// acquire it at boot and emit a clear contention message when a
// second server points at the same ECAA_CHAT_SESSIONS_DIR.
pub use _session_lock::ServerSessionStoreLock;

// Re-export the ETag helpers so per-domain submodules can opt into
// optimistic-concurrency `If-Match` checks on high-impact mutation
// handlers.
pub use _etag::{
    check_if_match, etag_for_session, insert_etag, precondition_failed_response, IfMatchOutcome,
};

// Re-export the idempotency primitives so high-impact
// mutating handlers (confirm, branch_session, start-execution) can
// reach them as `super::IdempotencyStore` / `super::IdempotencyTicket`.
pub use _idempotency::{
    IdempotencyStore, IdempotencyTicket, DEFAULT_IDEMPOTENCY_TTL_SECS, HEADER_NAME,
    MAX_IDEMPOTENCY_ENTRIES,
};

// Re-export the pagination helpers so the
// growing-collection list endpoints (decisions, transcript,
// share-tokens, harness-events) share a uniform wire envelope.
pub use _pagination::{Page as PaginatedPage, Params as PaginationParams};

// Re-export the bounded-depth JSON extractor so high-impact mutation
// routes (`/turn`, `/confirm`, `/reject`, `/unblock`, `/branch`,
// `/start_execution`, `/sessions`, `/sessions/from-intent`, the
// propose-* endpoints, `set_intake_field`, `amend_stage_method`) can
// use `super::BoundedJson<T>` instead of `axum::Json<T>` to reject
// pathologically-nested payloads before deserialization recurses.
pub use _json_depth::{BoundedJson, MAX_JSON_DEPTH};

// Re-export the client-IP
// helper so SME-action handlers can call `super::client_ip_from(...)`
// when they record decisions. Mirrors the `safe_segment_join` re-export
// convention.
pub use _client_ip::client_ip_from;

// Re-export shared infra so the rest of the server crate keeps
// referencing them as `chat_routes::ChatAppState` /
// `chat_routes::ExecutionHandle` / etc. without knowing they live
// in `app_state.rs`.
pub use app_state::{
    ArtifactCache, ChatAppState, ExecutionHandle, LlmRateBuckets, RateBucket, ScorerCache,
    PROGRESS_RATE_BURST, PROGRESS_RATE_PER_SEC, SCORER_CACHE_TTL_SECS,
};

// Re-export wire types so the rest of the server crate (handlers in
// submodules + the lib root + tests) keeps referencing them as
// `chat_routes::SsePayload` / `chat_routes::SessionStateSnapshot` /
// etc. without knowing they live in `wire_types.rs`.
pub(super) use wire_types::session_state_kind;
pub use wire_types::{
    AgentUsageWire, ArtifactRef, CheckpointDecisionRequest, CreateSessionRequest,
    CreateSessionResponse, EnvelopedEvent, ExecutionStatusResponse, ExecutorInfoWire,
    HarnessProgressEvent, OrphanReapWire, ProgressClientHealthWire, ProgressSummary,
    RemoteExecutionInfoWire, SendTurnRequest, SessionStateSnapshot, SsePayload,
    StartExecutionRequest, StartSessionFromIntentRequest,
};

pub use branches::{branch_session_endpoint, list_recent_sessions, list_sessions_by_parent};
pub use dashboard::dashboard_index;
pub use events::{events_stream, post_progress};
pub use execution::{
    get_dag, get_execution, post_kill_execution, post_pause_execution, post_resume_execution,
    post_stop_execution, start_execution,
};
pub use health::{healthz, readyz};
pub use sessions::{
    create_session, create_session_from_intent, get_decisions, get_harness_events, get_metrics,
    get_state, get_transcript, score_session,
};
pub use tasks::{
    auto_approve_discoveries, get_active_tasks, get_artifact, get_progress_log, get_stuck_tasks,
    get_task_blocker, get_task_log_tail, get_task_result, get_task_status_sentinels,
    list_task_logs, list_task_scripts, post_amend_method, post_impact_preview, post_rerun,
    post_rerun_script, post_sme_decisions, post_sme_selection, post_task_note, post_undo_amendment,
};
pub use turns::{confirm, reject, send_turn, unblock};
pub use verification::{
    get_cross_version_diff, get_cross_version_diff_table, get_pilot_report, verify_task_endpoint,
};

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
