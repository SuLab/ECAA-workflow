//! Deterministic reproducibility endpoints: audit-proof re-verify +
//! replay (Tier-1 integrity + Tier-2 re-execute).
//!
//! All heavy lifting is synchronous `core` (`run_audit_proof_with_verifier`,
//! `run_replay`) run under `tokio::task::spawn_blocking` — `core` is
//! tokio-free and must never be `.await`-ed on a worker thread. Cheap
//! actions (re-verify, replay `verify`) return JSON synchronously; the
//! heavy `execute`/`all` replay runs as a backgrounded job tracked by a
//! `ReplayHandle` (mirrors `ExecutionHandle`) with SSE progress.
//!
//! No new LLM `Tool` — these are deterministic server actions like
//! `verify`/`confirm`/`unblock`.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use uuid::Uuid;

use crate::chat_routes::wire_types::SsePayload;
use crate::chat_routes::{ChatAppState, ReplayHandle, ReplayJobStatus};
use ecaa_workflow_core::audit_proof::{run_audit_proof_with_verifier, AuditProofReport};
use ecaa_workflow_core::audit_writer::AuditWriter;
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_core::replay::{run_replay, ReplayOptions, ReplayReport, Tier};
use ecaa_workflow_core::wrroc_validator::NoopWrrocValidator;

/// The ECAA spec version this build of the reader implements. Threaded
/// into `ReplayOptions::reader_version` so re-verify can tell real
/// tampering (reader matches writer) from version drift.
fn reader_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// `POST /api/chat/session/:id/audit-proof/reverify` — re-run the 6
/// audit-proof invariants with the in-process session HMAC secret. The
/// secret de-vacuifies the claim-completeness / evidence-coverage
/// invariants (Inv 1/5) that read the signed verdict sink. The secret
/// never leaves the server.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub(super) async fn reverify_audit_proof(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound("package not yet emitted".into()).into_response();
    };
    let secret = session.audit_writer_secret;
    // Imported packages carry no originating HMAC secret (it never leaves the
    // origin's process), so re-verify runs verifier-less — structural
    // invariants only, no signed-verdict-sink read. Locally-created emitted
    // sessions still verify against the in-process secret.
    let imported = session.imported;
    let joined = tokio::task::spawn_blocking(move || {
        let validator = NoopWrrocValidator;
        if imported {
            run_audit_proof_with_verifier(&root, &validator, &WallClock, None)
        } else {
            let writer = AuditWriter::with_secret(secret);
            run_audit_proof_with_verifier(&root, &validator, &WallClock, Some(&writer))
        }
    })
    .await;
    match joined {
        Ok(Ok(report)) => Json(report).into_response(),
        Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
    }
}

/// `GET /api/chat/session/:id/audit-proof` — return the last-written
/// `runtime/audit-proof-report.json`. 404 when the session hasn't
/// emitted or no report has been produced yet.
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub(super) async fn get_audit_proof(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound("package not yet emitted".into()).into_response();
    };
    let p = root.join("runtime").join("audit-proof-report.json");
    match tokio::fs::read(&p).await {
        Ok(bytes) => match serde_json::from_slice::<AuditProofReport>(&bytes) {
            Ok(report) => Json(report).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
        },
        Err(_) => {
            crate::error::ApiError::NotFound("audit-proof report not yet produced".into())
                .into_response()
        }
    }
}

/// Request body for `POST /api/chat/session/:id/replay`.
#[derive(serde::Deserialize)]
pub(super) struct ReplayRequest {
    /// `"verify"` (Tier-1, synchronous) | `"execute"` | `"all"` (Tier-2,
    /// backgrounded).
    pub(super) tier: String,
    /// Reserved for a future strict verdict gate; accepted but not yet
    /// acted on so the wire contract is stable.
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) strict: bool,
}

fn parse_tier(s: &str) -> Option<Tier> {
    match s {
        "verify" => Some(Tier::Verify),
        "execute" => Some(Tier::Execute),
        "all" => Some(Tier::All),
        _ => None,
    }
}

/// Lowercase verdict string (`pass` | `partial` | `fail`) for the SSE
/// `replay_completed` payload.
fn verdict_str(r: &ReplayReport) -> String {
    format!("{:?}", r.verdict).to_lowercase()
}

/// `POST /api/chat/session/:id/replay` — run a replay.
///
/// `tier="verify"` runs Tier-1 (deterministic re-verify) synchronously
/// and returns the `ReplayReport` as `200`. `tier="execute"|"all"` runs
/// the heavy container re-execution as a backgrounded job and returns
/// `202 {"replay_id": <uuid>}`; poll `GET …/replay` (or listen for the
/// `replay_completed` SSE) for the result.
#[tracing::instrument(skip(app, body), fields(session_id = %session_id))]
pub(super) async fn start_replay(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
    body: Option<axum::Json<ReplayRequest>>,
) -> impl IntoResponse {
    let req = body.map(|b| b.0).unwrap_or(ReplayRequest {
        tier: "verify".into(),
        strict: false,
    });
    let Some(tier) = parse_tier(&req.tier) else {
        return (StatusCode::BAD_REQUEST, "tier must be verify|execute|all").into_response();
    };
    let Some(session) = app.conversation.get_session(session_id).await else {
        return crate::error::ApiError::NotFound("session not found".into()).into_response();
    };
    let Some(root) = session.emitted_package_path.clone() else {
        return crate::error::ApiError::NotFound("package not yet emitted".into()).into_response();
    };

    // ── Tier-1: synchronous re-verify ────────────────────────────────────
    if tier == Tier::Verify {
        let rv = reader_version();
        let joined = tokio::task::spawn_blocking(move || {
            let opts = ReplayOptions {
                tier: Tier::Verify,
                scratch_dir: None,
                bounds: None,
                allow_rebuild: false,
                reader_version: rv,
            };
            run_replay(&root, &opts)
        })
        .await;
        return match joined {
            Ok(Ok(report)) => Json::<ReplayReport>(report).into_response(),
            Ok(Err(e)) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
            Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, format!("join: {e}")).into_response(),
        };
    }

    // ── Tier-2 availability gate (imported packages) ─────────────────────
    // Re-probe the physical completeness of an IMPORTED package: a Tier-2
    // (execute/all) replay needs re-executable task surfaces (scripts +
    // tables + provisionable env). Minimal-audit / incomplete deposits return
    // 412 rather than backgrounding a job that would immediately fail
    // unprovisionable. Scoped to imported sessions: locally-created emitted
    // sessions keep their existing Tier-2 semantics (the reservation/exec-lock
    // checks below), and are re-executable by construction once run.
    if session.imported && matches!(tier, Tier::Execute | Tier::All) {
        let probe_root = root.clone();
        let caps = tokio::task::spawn_blocking(move || {
            ecaa_workflow_core::package_import::probe_package_capabilities(&probe_root)
        })
        .await
        .ok();
        if !caps.map(|c| c.replay_tier2).unwrap_or(false) {
            return (
                StatusCode::PRECONDITION_FAILED,
                "package is not re-executable (Tier-2 replay unavailable for this completeness tier)",
            )
                .into_response();
        }
    }

    // ── Tier-2: heavy, backgrounded ──────────────────────────────────────
    // Two-flag mutual exclusion with the execution-spawn path: reproducing
    // recorded compute while the harness produces fresh compute for the same
    // package would race artifact writes.
    //
    // BOTH sides are "reserve (set) THEN check, roll back on conflict". Here we
    // reserve the replay slot in `replays` first (the `entry` write-lock also
    // rejects a second concurrent replay — TOCTOU), then check `executions` +
    // `starting_executions`. `spawn_harness_for_session` mirrors this: it
    // reserves `starting_executions`, THEN checks `replays`. Set-then-check on
    // BOTH sides is required — a check-then-set on either reopens a window where
    // a replay and an execution both proceed. Worst case under exact
    // simultaneity is that both refuse (safe — the caller retries); there is no
    // interleaving where both run.
    let replay_id = Uuid::new_v4();
    let status = std::sync::Arc::new(std::sync::Mutex::new(ReplayJobStatus::Running));
    {
        use dashmap::mapref::entry::Entry;
        match app.replays.entry(session_id) {
            Entry::Occupied(mut o) => {
                if o.get().is_running() {
                    return (StatusCode::CONFLICT, "a replay is already running")
                        .into_response();
                }
                o.insert(ReplayHandle {
                    started_at: chrono::Utc::now(),
                    status: status.clone(),
                });
            }
            Entry::Vacant(v) => {
                v.insert(ReplayHandle {
                    started_at: chrono::Utc::now(),
                    status: status.clone(),
                });
            }
        }
    }
    // We now hold the replay slot. Refuse (and roll back) if an execution is
    // running or mid-spawn.
    let execution_active = app
        .executions
        .get(&session_id)
        .map(|e| e.value().exit_status_get().is_none())
        .unwrap_or(false)
        || app.starting_executions.contains(&session_id);
    if execution_active {
        app.replays.remove(&session_id); // roll back our reservation
        return (
            StatusCode::CONFLICT,
            "cannot replay while an execution is running or starting",
        )
            .into_response();
    }
    let rv = reader_version();
    let tier_label = req.tier.clone();
    let app2 = app.clone();
    // There is NO `.await` between reserving the replay slot (above) and this
    // spawn, so handler-future cancellation (client disconnect) cannot leave a
    // Running handle wedged in `app.replays` — the spawned task always owns the
    // reservation's terminal transition. The ReplayStarted SSE is fired from
    // inside the task for the same reason (it is advisory; the tab self-polls).
    tokio::spawn(async move {
        app2.broadcast(session_id, SsePayload::ReplayStarted { tier: tier_label })
            .await;
        let joined = tokio::task::spawn_blocking(move || {
            let opts = ReplayOptions {
                tier,
                scratch_dir: None,
                bounds: None,
                allow_rebuild: false,
                reader_version: rv,
            };
            run_replay(&root, &opts)
        })
        .await;
        let (new_status, verdict) = match joined {
            Ok(Ok(report)) => {
                let v = verdict_str(&report);
                (ReplayJobStatus::Done(Box::new(report)), v)
            }
            Ok(Err(e)) => (ReplayJobStatus::Failed(e.to_string()), "fail".into()),
            Err(e) => (ReplayJobStatus::Failed(format!("join: {e}")), "fail".into()),
        };
        *status.lock().unwrap_or_else(|p| p.into_inner()) = new_status;
        app2.broadcast(session_id, SsePayload::ReplayCompleted { verdict })
            .await;
    });
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "replay_id": replay_id })),
    )
        .into_response()
}

/// `GET /api/chat/session/:id/replay` — poll the backgrounded replay
/// job's status. `{"status":"idle"}` when no replay has been started;
/// otherwise `running` / `done` (+`report`) / `failed` (+`error`).
#[tracing::instrument(skip(app), fields(session_id = %session_id))]
pub(super) async fn get_replay(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    match app.replays.get(&session_id) {
        None => Json(serde_json::json!({ "status": "idle" })).into_response(),
        Some(h) => {
            let s = h.value().status.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match s {
                ReplayJobStatus::Running => {
                    Json(serde_json::json!({ "status": "running" })).into_response()
                }
                ReplayJobStatus::Done(r) => {
                    Json(serde_json::json!({ "status": "done", "report": *r })).into_response()
                }
                ReplayJobStatus::Failed(e) => {
                    Json(serde_json::json!({ "status": "failed", "error": e })).into_response()
                }
            }
        }
    }
}

/// Route inventory for the doc-as-contract gate + `routes()` builder.
pub(super) const ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/chat/session/:id/audit-proof/reverify"),
    ("GET", "/api/chat/session/:id/audit-proof"),
    ("POST", "/api/chat/session/:id/replay"),
    ("GET", "/api/chat/session/:id/replay"),
];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new()
        .route(
            "/api/chat/session/:id/audit-proof/reverify",
            axum::routing::post(reverify_audit_proof),
        )
        .route(
            "/api/chat/session/:id/audit-proof",
            axum::routing::get(get_audit_proof),
        )
        .route(
            "/api/chat/session/:id/replay",
            axum::routing::post(start_replay).get(get_replay),
        )
}

#[cfg(test)]
mod tests {
    use crate::chat_routes::test_support::{
        insert_running_execution, make_router, seed_session_with_completed_task,
    };
    use axum::body::{to_bytes, Body};
    use axum::http::{Request, StatusCode};
    use tower::util::ServiceExt;

    /// Re-verify runs the 6 audit-proof invariants over the emitted
    /// package and returns one verdict per invariant.
    #[tokio::test]
    async fn reverify_returns_six_invariants_for_emitted_package() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/audit-proof/reverify"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "reverify must return 200");
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let report: ecaa_workflow_core::audit_proof::AuditProofReport =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            report.verdicts.len(),
            6,
            "one verdict per audit-proof invariant"
        );
    }

    /// GET audit-proof is a 404 when no report file has been written yet.
    #[tokio::test]
    async fn get_audit_proof_is_404_when_absent() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{id}/audit-proof"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "absent report must 404");
    }

    /// GET audit-proof returns the on-disk report when it exists.
    #[tokio::test]
    async fn get_audit_proof_returns_written_report() {
        let pkg = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(pkg.path().join("runtime")).unwrap();
        // Produce a real report via the re-verify code path, then persist
        // it where the GET handler reads from.
        let report = ecaa_workflow_core::audit_proof::run_audit_proof_with_verifier(
            pkg.path(),
            &ecaa_workflow_core::wrroc_validator::NoopWrrocValidator,
            &ecaa_workflow_core::clock::WallClock,
            None,
        )
        .unwrap();
        std::fs::write(
            pkg.path().join("runtime").join("audit-proof-report.json"),
            serde_json::to_vec(&report).unwrap(),
        )
        .unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/chat/session/{id}/audit-proof"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "written report must be served");
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let got: ecaa_workflow_core::audit_proof::AuditProofReport =
            serde_json::from_slice(&bytes).unwrap();
        assert_eq!(got.verdicts.len(), 6, "six invariants round-trip");
    }

    /// Tier-1 replay (verify) runs synchronously and returns a report.
    #[tokio::test]
    async fn replay_verify_returns_report() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tier":"verify"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "replay verify must return 200");
        let bytes = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let _r: ecaa_workflow_core::replay::ReplayReport =
            serde_json::from_slice(&bytes).unwrap();
    }

    /// An unknown tier string is rejected with 400.
    #[tokio::test]
    async fn replay_rejects_unknown_tier() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tier":"bogus"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "unknown tier must 400");
    }

    /// Tier-2 replay (execute) is backgrounded: POST returns 202 and the
    /// job eventually reaches a terminal status observable via GET. In
    /// the test env there is no container runtime, so the job finishes
    /// quickly with a PARTIAL/unprovisionable verdict — we assert only
    /// that it reaches ANY terminal status, not a specific verdict.
    #[tokio::test]
    async fn replay_execute_is_backgrounded_and_reports_status() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        let start = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tier":"execute"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(start.status(), StatusCode::ACCEPTED, "backgrounded replay must 202");

        let mut terminal = false;
        for _ in 0..100 {
            let s = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("GET")
                        .uri(format!("/api/chat/session/{id}/replay"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let bytes = to_bytes(s.into_body(), 1 << 20).await.unwrap();
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if v["status"] != "running" {
                assert!(
                    v["status"] == "done" || v["status"] == "failed",
                    "terminal status must be done or failed, got {v}"
                );
                terminal = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        assert!(terminal, "replay job reached a terminal status");
    }

    /// A replay is refused with 409 while an execution is running.
    #[tokio::test]
    async fn replay_refused_while_execution_running() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        insert_running_execution(&app, id, pkg.path().to_path_buf());
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tier":"execute"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "replay refused while execution runs");
    }

    /// A replay is refused with 409 during the execution SPAWN WINDOW: the
    /// session is reserved in `starting_executions` before its `ExecutionHandle`
    /// lands in `executions`, so checking only `executions` would let a Tier-2
    /// replay race the harness's artifact writes.
    #[tokio::test]
    async fn replay_refused_while_execution_starting() {
        let pkg = tempfile::TempDir::new().unwrap();
        let (router, app) = make_router(vec![]).await;
        let id =
            seed_session_with_completed_task(&app, "t_demo", Some(pkg.path().to_path_buf())).await;
        app.starting_executions.insert(id);
        let resp = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/api/chat/session/{id}/replay"))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"tier":"execute"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::CONFLICT,
            "replay refused during execution spawn window"
        );
        // The reserve-first-then-check path must ROLL BACK its reservation on
        // refusal — no lingering Running handle that would wedge future replays.
        assert!(
            app.replays
                .get(&id)
                .map_or(true, |h| !h.value().is_running()),
            "replay reservation rolled back after refusal"
        );
    }
}
