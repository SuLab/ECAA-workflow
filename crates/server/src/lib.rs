//! `scripps-workflow-server` lib target.
//!
//! Hybrid lib + bin layout. The bin's job (`src/main.rs`) is just to
//! parse the port arg, install the tracing subscriber, and call
//! [`run`]. All the routing modules live here so integration tests
//! under `crates/server/tests/` can reach `chat_routes::router` and
//! `git_routes::router` via `tower::ServiceExt::oneshot`.
//!
//! The "no surface widening as a side-effect of test prep" rule from
//! `tests/git_hooks_integration.rs` is honored by re-exporting only
//! the modules that already had public APIs at module scope. Cross-
//! module references inside the crate continue to use `crate::` so
//! the move is mechanical.

pub mod auth;
pub mod chat_routes;
pub mod cors;
pub mod error;
pub mod git_routes;
pub mod read_only;
pub mod sec_fetch;
pub mod security_headers;
pub mod verification;

use crate::auth::{auth_middleware, AuthConfig};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http;
use axum::middleware;
use axum::middleware::from_fn_with_state;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower_governor::governor::GovernorConfigBuilder;
use tower_governor::GovernorLayer;
use tower_http::classify::{ServerErrorsAsFailures, SharedClassifier};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::services::{ServeDir, ServeFile};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer};
use tracing::Level;

/// Resolve the per-request body size cap from `SWFC_REQUEST_BODY_LIMIT_KB`
/// (default 256 KiB). Returned value is bytes; consumed by the global
/// `tower_http::RequestBodyLimitLayer` so a single malicious POST cannot
/// exhaust memory.
///
/// The chunked-upload endpoint (`/api/chat/session/:id/inputs/upload`)
/// opts out via `DefaultBodyLimit::disable()` on its sub-router; the
/// chunk handler enforces its own 32 MiB hard cap (see
/// `chat_routes::inputs::upload::MAX_UPLOAD_CHUNK_BYTES`).
pub fn resolve_request_body_limit_bytes() -> usize {
    let kb = std::env::var("SWFC_REQUEST_BODY_LIMIT_KB")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(256);
    kb.saturating_mul(1024)
}

/// Resolve the per-request timeout from `SWFC_REQUEST_TIMEOUT_SECS`
/// (default 300s). Consumed by the `tower_http::TimeoutLayer` to cap
/// long-poll DoS by a connection that sends headers and then never
/// finishes the body.
///
/// The 300s ceiling matches `chat-llm`'s test harness and what
/// Anthropic Messages allow before they close their own upstream
/// connection. A confirmation-card turn running
/// `propose_summary_confirmation` after a `classify_intake` +
/// `get_taxonomy_info` round-trip routinely passes 60s on the Sonnet
/// model, so a 60s ceiling caught every real `/turn` against the live
/// API.
pub fn resolve_request_timeout() -> Duration {
    let secs = std::env::var("SWFC_REQUEST_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(300);
    Duration::from_secs(secs)
}

/// Function pointer alias for the `make_span_with` callback so the
/// `HttpTraceLayer` type alias stays readable.
type MakeHttpSpanFn = fn(&http::Request<Body>) -> tracing::Span;

/// Layer alias for the audit-trail `TraceLayer` returned by
/// [`build_trace_layer`]. Pulled out as a type alias so the public
/// signature of `build_trace_layer` is legible (clippy was flagging
/// the inlined four-parameter `TraceLayer<...>` as a complex type).
pub type HttpTraceLayer = TraceLayer<
    SharedClassifier<ServerErrorsAsFailures>,
    MakeHttpSpanFn,
    DefaultOnRequest,
    DefaultOnResponse,
>;

/// Build the `TraceLayer` that
/// Emits one `info_span!("http",...)` per request capturing the
/// HTTP method, request URI path, and client IP. The IP is sourced
/// from the `X-Forwarded-For` header first (operator behind a trusted
/// reverse proxy can opt in) and falls back to the per-connection
/// `ConnectInfo<SocketAddr>` extension set by
/// `axum::serve(..).into_make_service_with_connect_info::<SocketAddr>()`.
/// When neither is present (synthetic requests, integration tests not
/// going through `into_make_service_with_connect_info`) the field is
/// stamped `"-"` so audit replay never has to special-case a missing
/// field.
///
/// Layered as the outer-most application layer in `run()` so EVERY
/// inbound request — including the synthetic 401 / 408 / 413 / 429
/// emitted by the inner auth / timeout / body-limit / governor layers
/// — is traced before any other middleware runs. The
/// `DefaultOnRequest` / `DefaultOnResponse` finishers preserve the
/// off-the-shelf request/response logging at `INFO` while the custom
/// `make_span_with` adds the audit-trail fields.
pub fn build_trace_layer() -> HttpTraceLayer {
    fn make_http_span(req: &http::Request<Body>) -> tracing::Span {
        let xff_ip: Option<String> = req
            .headers()
            .get("x-forwarded-for")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string());
        let connect_ip: Option<String> = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.to_string());
        let client_ip = xff_ip.or(connect_ip).unwrap_or_else(|| "-".to_string());
        tracing::info_span!(
            "http",
            method = %req.method(),
            uri = %req.uri().path(),
            client_ip = %client_ip,
        )
    }
    let make_span: MakeHttpSpanFn = make_http_span;
    TraceLayer::new_for_http()
        .make_span_with(make_span)
        .on_request(DefaultOnRequest::new().level(Level::INFO))
        .on_response(DefaultOnResponse::new().level(Level::INFO))
}

/// Resolve the bind address for the Axum HTTP server. Reads
/// `SWFC_BIND_ADDR` (default `127.0.0.1`) and combines with `port`.
///
/// Default is loopback-only. Operators who want the server reachable
/// on a LAN must opt in via `SWFC_BIND_ADDR=0.0.0.0` (or a specific
/// interface), and in that case `SWFC_SERVER_AUTH_TOKEN` is also
/// required — see `crate::auth::AuthConfig::from_env`.
pub fn resolve_bind_addr(port: u16) -> String {
    let host = std::env::var("SWFC_BIND_ADDR")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "127.0.0.1".to_string());
    format!("{host}:{port}")
}

/// Boot the Axum server. Called by `main.rs` after argument parsing
/// + tracing-subscriber install. Reads `--port` (default 3000) and
///   the chat init via `ChatAppState::new`. Exits the process on chat
///   init failure (matches the pre-S5.13 behavior in `main.rs`).
pub async fn run() {
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("Loaded .env from {}", path.display()),
        Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("WARNING: failed to load .env: {}", e),
    }

    let port: u16 = std::env::args()
        .skip_while(|a| a != "--port")
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);

    let chat_state = match chat_routes::ChatAppState::new().await {
        Ok(s) => Arc::new(s),
        Err(e) => {
            eprintln!(
                "FATAL: chat routes failed to initialize — {} (set SWFC_ANTHROPIC_API_KEY or SWFC_CHAT_MODE=offline)",
                e
            );
            std::process::exit(1);
        }
    };

    // R3.7: acquire a host-level flock on the configured session-store
    // directory so two server processes pointed at the same store can
    // never race on the atomic-rename writes inside `SessionStore`.
    // The kernel drops the flock on exit (even via SIGKILL); bypass
    // for multi-server tests with SWFC_SERVER_DEBUG_ALLOW_MULTI_PROCESS=1.
    // The guard is held by the run() stack frame so it lives exactly
    // as long as the server process.
    let session_store_dir = chat_state.config.chat_sessions_dir.clone();
    let _store_lock = match chat_routes::ServerSessionStoreLock::acquire(&session_store_dir) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("FATAL: {}", e);
            std::process::exit(2);
        }
    };

    let addr = resolve_bind_addr(port);
    let is_lan_bind = addr.starts_with("0.0.0.0") || addr.starts_with("[::]");
    let auth_token_present = std::env::var("SWFC_SERVER_AUTH_TOKEN")
        .ok()
        .map(|t| !t.is_empty())
        .unwrap_or(false);
    if is_lan_bind && !auth_token_present {
        // The auth middleware already rejects every request fail-closed
        // when SWFC_SERVER_AUTH_TOKEN is unset on a non-loopback bind,
        // but starting a server in that posture is almost never what
        // the operator wanted. Refuse to start unless they've explicitly
        // ack'd via SWFC_SERVER_LAN_NO_AUTH=1; the bare warning the
        // previous version emitted was easy to lose in log volume.
        let allow_unsafe = scripps_workflow_core::env_helpers::env_bool("SWFC_SERVER_LAN_NO_AUTH");
        if allow_unsafe {
            tracing::error!(
                "server binding {} — LAN-exposed without auth (SWFC_SERVER_LAN_NO_AUTH=1 \
                 opt-in honored); the auth middleware will still reject every request",
                addr
            );
        } else {
            eprintln!(
                "FATAL: server bind {} is non-loopback and SWFC_SERVER_AUTH_TOKEN is unset.\n\
                 Set SWFC_SERVER_AUTH_TOKEN=<random-string>, bind 127.0.0.1, or\n\
                 explicitly opt in with SWFC_SERVER_LAN_NO_AUTH=1 (not recommended).",
                addr
            );
            std::process::exit(1);
        }
    }
    let auth_cfg = AuthConfig::from_env(&addr);

    let body_limit = resolve_request_body_limit_bytes();
    let request_timeout = resolve_request_timeout();

    // Per-IP global rate limit.
    // tower_governor's `.per_second(N)` is misnamed — it's the
    // *interval in seconds* between token refills, NOT requests/sec.
    // Default 2/30 = "one new token every 2 s, burst 30" = 0.5 req/s
    // sustained. tower_governor uses `PeerIpKeyExtractor` by default
    // (we don't sit behind a trusted reverse proxy that forwards
    // `X-Forwarded-For`), so each TCP peer is tracked independently.
    // Configured BEFORE auth so a token-probing flood is rejected
    // without ever hitting the constant-time `subtle::ConstantTimeEq`.
    //
    // Local-dev escape hatch: SWFC_RATE_LIMIT_INTERVAL_MS (refill
    // period in milliseconds; LOWER = faster) and SWFC_RATE_LIMIT_BURST
    // override the production defaults. Multiple headed Playwright
    // sessions share 127.0.0.1 and the UI polls /state, /dag,
    // /dispositions, /proposals, /metrics every few seconds — easy to
    // burn through 30 burst within a single chat turn, which then 429s
    // the chat itself. For local dev set INTERVAL_MS=10 BURST=10000.
    // 2 s == prior `.per_second(2)` behavior
    let rl_interval_ms: u64 =
        scripps_workflow_core::env_helpers::env_parse("SWFC_RATE_LIMIT_INTERVAL_MS", 2_000);
    let rl_burst: u32 = scripps_workflow_core::env_helpers::env_parse("SWFC_RATE_LIMIT_BURST", 30);
    let governor_conf = std::sync::Arc::new(
        GovernorConfigBuilder::default()
            .per_millisecond(rl_interval_ms.max(1))
            .burst_size(rl_burst)
            .finish()
            .expect("GovernorConfig from env (or 2000ms/30 default) is well-formed"),
    );
    // Spawn a background task that periodically reaps idle entries in
    // the governor's keyed state store. Without this the per-IP key map
    // grows unbounded over the server's lifetime as new clients connect.
    let governor_limiter = governor_conf.limiter().clone();
    tokio::spawn(async move {
        let interval = Duration::from_secs(60);
        loop {
            tokio::time::sleep(interval).await;
            governor_limiter.retain_recent();
        }
    });
    let governor_layer = GovernorLayer {
        config: governor_conf,
    };

    let shared = (*chat_state).clone();
    // Build the authenticated application core first. Every `.layer()` call
    // wraps all routes assembled so far — auth, governor, CORS, etc. all
    // apply to the `/api/chat/*` and `/api/git/*` surfaces.
    let authenticated_app = chat_routes::router((*chat_state).clone())
        .merge(git_routes::router((*chat_state).clone()))
        // Owner-user authZ. Compares
        // `X-Scripps-User` (proxy-injected) against `Session.owner_user`
        // for every per-session route. Layered INSIDE the bearer auth
        // (auth_middleware) — bearer-auth is the bouncer for "any
        // upstream traffic", verify_owner is the per-session
        // permission check. SWFC_OWNER_AUTHZ_DISABLE=1 disables; the
        // "local" sentinel default for single-user dev also bypasses.
        .layer(from_fn_with_state(
            (*chat_state).clone(),
            crate::auth::verify_owner_middleware,
        ))
        // RequestPrincipal extraction (C1): derives a single auth
        // identity from share-token / bearer+X-Scripps-User /
        // X-Harness-Token and stamps it into request extensions.
        // Layered INSIDE bearer auth so the principal is always
        // available to handler code via `Extension<RequestPrincipal>`.
        // Layered OUTSIDE verify_owner so the principal is computed
        // before per-session permission checks read it.
        .layer(from_fn_with_state(
            (*chat_state).clone(),
            crate::auth::principal::extract_principal,
        ))
        .layer(from_fn_with_state(auth_cfg, auth_middleware))
        // Layer the GovernorLayer OUTSIDE the auth middleware. Axum's
        // `.layer()` stacks outward — the last `.layer()` is the
        // outermost — so this fires BEFORE auth in execution order. A
        // token-probing flood is short-circuited with 429 before any
        // crypto compare runs.
        .layer(governor_layer)
        .layer(middleware::from_fn_with_state(
            shared,
            read_only::read_only_guard,
        ))
        .layer(axum::middleware::from_fn(crate::sec_fetch::sec_fetch_guard))
        .layer(crate::cors::build_cors())
        // Global per-request body cap + timeout.
        // Placed outermost so an oversized body / slowloris connection is
        // rejected before any auth / CORS / read-only machinery runs.
        // `/inputs/upload` opts out of the body cap via
        // `DefaultBodyLimit::disable()` on its sub-router and enforces a
        // 32 MiB per-chunk hard cap in the handler instead.
        .layer(RequestBodyLimitLayer::new(body_limit))
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        // Stamp CSP,
        // X-Frame-Options, X-Content-Type-Options, Referrer-Policy, and
        // Permissions-Policy on every response. Layered OUTERMOST so the
        // headers ride on every response — REST, SSE, the bundled UI
        // (`ui/dist` fallback service), and the synthetic 408/413/429
        // emitted by Timeout/BodyLimit/Governor on the inner layers.
        .layer(axum::middleware::from_fn(
            crate::security_headers::security_headers_middleware,
        ))
        // Trace every inbound
        // request with method + URI path + client IP. Layered
        // OUTER-MOST (after security_headers, which writes outbound
        // headers regardless) so even requests rejected by an inner
        // layer (auth 401, governor 429, body-limit 413, timeout 408)
        // produce an audit-trail span. The make_span_with closure
        // reads `X-Forwarded-For` first and falls back to the
        // `ConnectInfo<SocketAddr>` extension that
        // `into_make_service_with_connect_info::<SocketAddr>()`
        // injects on every accepted connection.
        .layer(build_trace_layer());

    // Merge health routes AFTER all auth/rate-limit layers so `/healthz`
    // and `/readyz` are unauthenticated. In Axum 0.7 merging a fresh
    // `Router` onto a fully-layered router does not propagate the inner
    // layers to the merged routes — this is the load-bearing property
    // that lets LBs probe without a bearer token.
    let app = authenticated_app
        .merge(chat_routes::health_router((*chat_state).clone()))
        .fallback_service(
            ServeDir::new("ui/dist").not_found_service(ServeFile::new("ui/dist/index.html")),
        );
    println!(
        "scripps-workflow-server listening on http://{} (set SWFC_BIND_ADDR=0.0.0.0 to widen)",
        addr
    );

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    // `PeerIpKeyExtractor` in `tower_governor` reads the socket's peer IP
    // from `ConnectInfo<SocketAddr>`, which is only populated by
    // `into_make_service_with_connect_info`. Without this wrapping every
    // request is rejected with `UnableToExtractKey` (HTTP 500). Matches
    // the recommended axum + tower_governor wiring in the upstream README.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal((*chat_state).clone()))
    .await
    .unwrap();
}

async fn shutdown_signal(app: chat_routes::ChatAppState) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("install ctrl-c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received; flushing harness batcher");
    app.batcher.clone().flush_pending().await;
}
