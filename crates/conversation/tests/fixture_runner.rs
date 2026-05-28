//! Scripted-LLM fixture runner. Loads each YAML file under
//! `tests/conversation-fixtures/fixtures/`, drives a
//! `ConversationService` backed by a `MockLlmBackend` whose responses
//! come from the fixture's `mock_responses` block, runs the `flow`
//! (user turns + button clicks), and asserts the
//! `expected_final_state` matches the resulting session.
//!
//! When the fixture references `{tempdir}` in any `output_dir`
//! argument, the runner substitutes a fresh temp directory at load
//! time so each fixture writes into an isolated location.

use ecaa_workflow_conversation::{
    BatchStrategy, BatcherConfig, ConversationService, HarnessBatcher, HarnessEvent,
    HeuristicMockBackend, LlmBackend, MockLlmBackend, SessionId, SessionState, SessionStore,
    StopReason, Tool, TurnResponse, Usage,
};
use serde::Deserialize;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[path = "common/mod.rs"]
mod common;
use common::TestEnv;

struct EnvVarGuard {
    key: &'static str,
    previous: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

// ── Fixture schema ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct Fixture {
    id: String,
    #[allow(dead_code)]
    category: String,
    #[allow(dead_code)]
    description: Option<String>,
    flow: Vec<FlowStep>,
    /// Scripted-tape responses for the `MockLlmBackend`. Empty / omitted
    /// for fixtures driving the [`HeuristicMockBackend`] path (R5.19),
    /// where tool selection is computed from session state rather than
    /// replayed from a recorded tape.
    #[serde(default)]
    mock_responses: Vec<FixtureLlmResponse>,
    /// Opt-in backend selector. Omitted (default) keeps the legacy
    /// scripted-tape path; `heuristic` swaps in `HeuristicMockBackend`
    /// and ignores `mock_responses`.
    #[serde(default)]
    mock_backend: MockBackendKind,
    /// Phase-3 batching strategy for the heuristic backend. Default
    /// (`single`) preserves the one-tool-per-response shape; non-default
    /// values route the heuristic's chosen tool through
    /// `heuristic_batch::build_batched_response` so the dispatcher's
    /// alone-in-turn enforcement is exercised. Ignored when
    /// `mock_backend` is not a heuristic variant.
    #[serde(default)]
    heuristic_batch_strategy: HeuristicBatchStrategyKind,
    expected_final_state: ExpectedFinalState,
    #[serde(default)]
    #[allow(dead_code)]
    rubric_notes: Option<String>,
    /// Pin the session's composer version. When set, the runner overwrites
    /// `Session::composer_version` after `start_session` so the fixture's
    /// expected behavior is independent of the `ECAA_COMPOSER` default.
    /// Use this when a fixture's assertions depend on a specific composer
    /// path (e.g. legacy taxonomy `discover_*` stages that the v4 archetype
    /// path doesn't author yet).
    #[serde(default)]
    composer_version: Option<u32>,
    /// Install a one-shot failure injection on the
    /// `HeuristicMockBackend`. The next decision that would dispatch
    /// `tool_name` instead emits a synthetic payload the deterministic
    /// dispatcher rejects with `ToolError::ValidationFailure`. Ignored
    /// for the scripted-tape backend.
    #[serde(default)]
    heuristic_failure: Option<HeuristicFailureSpec>,
}

#[derive(Debug, Deserialize)]
struct HeuristicFailureSpec {
    tool_name: String,
    reason: String,
}

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum MockBackendKind {
    /// Tape-recorder mock backed by `mock_responses` (default).
    #[default]
    Scripted,
    /// State-driven oracle (`HeuristicMockBackend`). `mock_responses`
    /// is ignored. R5.19 — first foundation; see roadmap doc.
    Heuristic,
    /// Same as `Heuristic`, but treats every turn after the first
    /// `propose_summary_confirmation` as confirmed (skips the SME-click
    /// inference). Fixtures that drive `service.confirm` explicitly
    /// don't need this; tests that want to exercise the post-confirm
    /// path without injecting the marker do.
    HeuristicAutoConfirm,
}

/// Phase-3 batching strategy selector. Mirrors `BatchStrategy` from
/// `heuristic_batch.rs` as a YAML-deserializable enum (the production
/// type doesn't derive `Deserialize` so fixtures don't lock the wire
/// shape).
#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum HeuristicBatchStrategyKind {
    #[default]
    Single,
    BatchedReadOnly,
    BatchedHighImpactIllegal,
}

impl HeuristicBatchStrategyKind {
    fn to_strategy(self) -> BatchStrategy {
        match self {
            Self::Single => BatchStrategy::Single,
            Self::BatchedReadOnly => BatchStrategy::BatchedReadOnly,
            Self::BatchedHighImpactIllegal => BatchStrategy::BatchedHighImpactIllegal,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FlowStep {
    /// Send a user prose turn through `service.send_turn`.
    User { text: String },
    /// Click the deterministic Confirm button.
    Confirm,
    /// Click the deterministic Make-corrections (reject) button.
    Reject,
    /// Click the deterministic Unblock button.
    Unblock,
    /// fire StateTrigger::InfraError on the session
    /// to simulate a backend failure (Anthropic API down, session lost,
    /// etc.). Used by fixtures 17/18.
    InjectInfraError { reason: String },
    /// push a synthetic HarnessEvent into the
    /// per-fixture HarnessBatcher. The batcher uses a 50 ms window when
    /// constructed via `make_service_with_batcher` so the flush happens
    /// before the fixture finishes asserting. Used by fixtures 24/25.
    /// The `event_kind` field name avoids colliding with the enum's own
    /// `kind` tag in the serde representation.
    EnqueueHarnessEvent {
        event_kind: String,
        task_id: String,
        detail: String,
        /// Optional backend identifier for the event's `remote` field.
        /// When set alongside `instance_type`, drives the
        /// `HarnessBatcher` to surface a `[<backend> · <type>]` tag in
        /// the synthetic assistant turn (see fixture 43 for the SLURM
        /// exercise).
        #[serde(default)]
        backend: Option<String>,
        #[serde(default)]
        instance_id: Option<String>,
        #[serde(default)]
        instance_type: Option<String>,
    },
    /// Sleep for the given duration so a fixture can wait for the
    /// HarnessBatcher to flush before asserting on the resulting
    /// synthetic turn. Avoids hard-coding sleeps in the runner.
    Wait { ms: u64 },
    /// Directly put the session into `Blocked {
    /// BlockerKind::AwaitingSmeSelection { stage_id, candidates } }`.
    /// In production this transition is driven by the
    /// harness/chat_routes post_progress path when a
    /// sensitivity_comparison stage finishes and surfaces its candidate
    /// outputs for SME selection. There is no StateTrigger for this
    /// state (it bypasses the table the way the force-assigned tests in
    /// tools.rs do), so the flow step does the same direct-assignment
    /// via the SessionStore. Used by fixture 29 to exercise
    /// `select_sensitivity_winner`, whose only valid precondition is
    /// exactly this Blocked shape.
    InjectAwaitingSmeSelection {
        stage_id: String,
        candidates: Vec<String>,
    },
}

/// One LLM response in the scripted sequence. The mock returns these in
/// declaration order regardless of which `flow` step triggered the call —
/// the conversation tool loop drains them as needed.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FixtureLlmResponse {
    /// Plain assistant text with `stop_reason = end_turn`. Ends the loop.
    Text { text: String },
    /// One tool call; the loop continues with another mock response.
    ToolUse {
        #[serde(rename = "tool")]
        tool_use: Tool,
    },
}

#[derive(Debug, Deserialize)]
struct ExpectedFinalState {
    /// `SessionState` serde tag value (e.g. "intake", "ready_to_emit", "emitted").
    state_kind: String,
    user_confirmed: bool,
    /// Tool names that must appear in `session.tool_call_log` in this order
    /// (gaps allowed — extra reads between are OK).
    #[serde(default)]
    tool_calls_observed: Vec<String>,
    /// Tool names that must NOT appear in `session.tool_call_log` at all.
    /// Used by neutrality/refusal fixtures (e.g. fixture 61)
    /// where the assertion is "the heuristic declined to dispatch X".
    #[serde(default)]
    tool_calls_excluded: Vec<String>,
    /// Files (relative to the emitted package dir) that must exist.
    #[serde(default)]
    package_artifacts_present: Vec<String>,
    /// exact size of `session.harness_events` after the flow
    /// completes. Used by fixture 24 to verify the batcher actually
    /// drained the events into the session via the synthetic turn flush.
    #[serde(default)]
    harness_events_count: Option<usize>,
    /// substring that must appear in the most recent assistant
    /// turn's content. Used by fixtures 24/25 to verify the
    /// HarnessBatcher's synthetic turn carries the expected phrasing.
    #[serde(default)]
    last_assistant_contains: Option<String>,
    /// substring that must appear in the Blocked state's
    /// `reason` field. Used by fixtures 17/18.
    #[serde(default)]
    blocked_reason_contains: Option<String>,
    /// Expected count of side-call invocations
    /// recorded against this session's `MetricsStore`. `None` skips
    /// the check; `Some(n)` asserts the sum across all model buckets
    /// equals `n`. Fixture 64 sets this to `0` because the
    /// conversation tool-loop doesn't fire the remediation_proposer
    /// itself (that lives behind the server route); once the
    /// auto-fire path lands, fixtures will assert `>= 1` here.
    #[serde(default)]
    side_call_count: Option<u32>,
}

// ── Loading ─────────────────────────────────────────────────────────────────

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("tests/conversation-fixtures/fixtures")
}

fn config_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn load_fixture(name: &str) -> (Fixture, tempfile::TempDir) {
    let path = fixtures_dir().join(format!("{}.yaml", name));
    let raw =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {}", path.display(), e));
    let temp = tempfile::tempdir().expect("creating temp output dir");
    let temp_str = temp.path().to_string_lossy().to_string();
    // Substitute the {tempdir} placeholder so per-fixture emit paths are isolated.
    let resolved = raw.replace("{tempdir}", &temp_str);
    let fixture: Fixture = serde_yml::from_str(&resolved)
        .unwrap_or_else(|e| panic!("parsing {}: {}", path.display(), e));
    (fixture, temp)
}

fn build_mock(responses: &[FixtureLlmResponse]) -> Vec<TurnResponse> {
    responses
        .iter()
        .map(|r| match r {
            FixtureLlmResponse::Text { text } => TurnResponse {
                assistant_content: text.clone(),
                tool_uses: vec![],
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),

                request_metadata: Default::default(),
            },
            FixtureLlmResponse::ToolUse { tool_use } => TurnResponse {
                assistant_content: String::new(),
                tool_uses: vec![(uuid::Uuid::new_v4(), tool_use.clone())],
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),

                request_metadata: Default::default(),
            },
        })
        .collect()
}

async fn make_service(scripted: Vec<TurnResponse>) -> (ConversationService, TestEnv) {
    // Returned `TestEnv` keeps the tempdir alive via RAII Arc<TempDir>;
    // every caller binds it so the session store outlives the test.
    let env = TestEnv::new();
    let store = SessionStore::open(env.path()).await.unwrap();
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    (ConversationService::new(backend, store, config_dir()), env)
}

/// R5.19 — build a service backed by the non-scripted
/// `HeuristicMockBackend`. Tool selection runs from session state +
/// user prose rather than from a recorded tape; see
/// `crates/conversation/src/heuristic_mock.rs`.
///
/// `batch_strategy` routes the heuristic's chosen tool
/// through `heuristic_batch::build_batched_response` to exercise the
/// dispatcher's alone-in-turn enforcement.
///
/// `failure` installs a one-shot failure injection so the
/// next dispatch of the named tool surfaces a
/// `ToolError::ValidationFailure` from the dispatcher.
async fn make_service_heuristic(
    auto_confirm: bool,
    batch_strategy: BatchStrategy,
    failure: Option<&HeuristicFailureSpec>,
) -> (ConversationService, TestEnv) {
    let env = TestEnv::new();
    let store = SessionStore::open(env.path()).await.unwrap();
    let mut heuristic = HeuristicMockBackend::new()
        .with_auto_confirm(auto_confirm)
        .with_batch_strategy(batch_strategy);
    if let Some(spec) = failure {
        heuristic = heuristic.with_failure_at(spec.tool_name.clone(), spec.reason.clone());
    }
    let backend: Arc<dyn LlmBackend> = Arc::new(heuristic);
    (ConversationService::new(backend, store, config_dir()), env)
}

/// Constructs a `ConversationService` and a `HarnessBatcher` that share
/// the same `SessionStore`. Used by fixtures 24/25 which need to enqueue
/// synthetic harness events into the batcher and observe the resulting
/// synthetic assistant turn appear in the same session the conversation
/// flow drives. The batcher uses a short 50 ms window so the test doesn't
/// have to wait the full 10 s production default.
async fn make_service_with_batcher(
    scripted: Vec<TurnResponse>,
) -> (ConversationService, Arc<HarnessBatcher>, TestEnv) {
    // `TestEnv` (RAII Arc<TempDir>) kept by the caller so the tempdir
    // outlives the test; replaces the prior `std::mem::forget(dir)`.
    let env = TestEnv::new();
    let store = SessionStore::open(env.path()).await.unwrap();
    let backend: Arc<dyn LlmBackend> = Arc::new(MockLlmBackend::new(scripted));
    let service = ConversationService::new(backend, store.clone(), config_dir());
    let batcher = Arc::new(HarnessBatcher::new(
        store,
        BatcherConfig {
            window: Duration::from_millis(50),
            ..BatcherConfig::default()
        },
    ));
    (service, batcher, env)
}

// ── Drive a fixture ─────────────────────────────────────────────────────────

async fn drive_fixture(fixture: &Fixture, _tempdir: &tempfile::TempDir) -> SessionId {
    let _auto_title_guard = EnvVarGuard::set("ECAA_AUTO_TITLE", "0");
    let mock_responses = build_mock(&fixture.mock_responses);
    // Use the batcher-equipped service whenever the fixture has any
    // EnqueueHarnessEvent step. Cheap to always construct, but we keep
    // the simpler path for the common case so failures point at the
    // right test helper. `_session_env` keeps the SessionStore tempdir
    // alive (RAII via Arc<TempDir>) for the lifetime of this function
    // — replaces the prior `std::mem::forget(dir)` pattern.
    let needs_batcher = fixture
        .flow
        .iter()
        .any(|s| matches!(s, FlowStep::EnqueueHarnessEvent { .. }));
    let (service, batcher, _session_env) = match fixture.mock_backend {
        MockBackendKind::Scripted => {
            if needs_batcher {
                let (svc, b, env) = make_service_with_batcher(mock_responses).await;
                (svc, Some(b), env)
            } else {
                let (svc, env) = make_service(mock_responses).await;
                (svc, None, env)
            }
        }
        MockBackendKind::Heuristic | MockBackendKind::HeuristicAutoConfirm => {
            // R5.19 — heuristic backend ignores mock_responses; the
            // batcher path remains available for future heuristic
            // fixtures that exercise harness events but no current
            // fixture combines them.
            assert!(
                !needs_batcher,
                "fixture {}: heuristic backend + EnqueueHarnessEvent is not wired yet — \
                add a batcher-aware variant of make_service_heuristic when needed",
                fixture.id
            );
            let auto_confirm =
                matches!(fixture.mock_backend, MockBackendKind::HeuristicAutoConfirm);
            let strategy = fixture.heuristic_batch_strategy.to_strategy();
            let (svc, env) =
                make_service_heuristic(auto_confirm, strategy, fixture.heuristic_failure.as_ref())
                    .await;
            (svc, None, env)
        }
    };
    let (session_id, _greeting) = service.start_session(false).await.unwrap();

    // Honor an explicit `composer_version:` pin in the fixture YAML so a
    // fixture's expected behavior is independent of the `ECAA_COMPOSER`
    // Default (which flipped from v1 → v4 on).
    if let Some(pin) = fixture.composer_version {
        service
            .store_handle()
            .update(session_id, |s| {
                s.composer_version = pin;
                Ok(())
            })
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "fixture {}: pinning composer_version={} failed: {}",
                    fixture.id, pin, e
                )
            });
    }

    for (i, step) in fixture.flow.iter().enumerate() {
        match step {
            FlowStep::User { text } => {
                service
                    .send_turn(session_id, text.clone(), None)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("fixture {} step {}: send_turn failed: {}", fixture.id, i, e)
                    });
            }
            FlowStep::Confirm => {
                service.confirm(session_id).await.unwrap_or_else(|e| {
                    panic!("fixture {} step {}: confirm failed: {}", fixture.id, i, e)
                });
            }
            FlowStep::Reject => {
                service.reject(session_id).await.unwrap_or_else(|e| {
                    panic!("fixture {} step {}: reject failed: {}", fixture.id, i, e)
                });
            }
            FlowStep::Unblock => {
                service.unblock(session_id).await.unwrap_or_else(|e| {
                    panic!("fixture {} step {}: unblock failed: {}", fixture.id, i, e)
                });
            }
            FlowStep::InjectInfraError { reason } => {
                service
                    .inject_infra_error(session_id, reason.clone())
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "fixture {} step {}: inject_infra_error failed: {}",
                            fixture.id, i, e
                        )
                    });
            }
            FlowStep::EnqueueHarnessEvent {
                event_kind,
                task_id,
                detail,
                backend,
                instance_id,
                instance_type,
            } => {
                let batcher = batcher
                    .as_ref()
                    .expect("EnqueueHarnessEvent flow step requires make_service_with_batcher");
                let remote = backend.as_ref().and_then(|b| {
                    instance_type.as_ref().map(|t| {
                        ecaa_workflow_conversation::session::RemoteExecutionInfo {
                            backend: b.clone(),
                            instance_id: instance_id.clone().unwrap_or_default(),
                            instance_type: t.clone(),
                        }
                    })
                });
                let event = HarnessEvent {
                    kind: event_kind.clone(),
                    task_id: task_id.clone(),
                    status: "running".into(),
                    detail: detail.clone(),
                    remote,
                    timestamp: chrono::Utc::now(),
                };
                batcher.clone().enqueue(session_id, event).await;
            }
            FlowStep::Wait { ms } => {
                tokio::time::sleep(Duration::from_millis(*ms)).await;
            }
            FlowStep::InjectAwaitingSmeSelection {
                stage_id,
                candidates,
            } => {
                use ecaa_workflow_core::blocker::{BlockerContext, BlockerKind};
                let stage_id = stage_id.clone();
                let candidates = candidates.clone();
                let store = service.store_handle();
                store
                    .update(session_id, |s| {
                        s.state = SessionState::Blocked {
                            blockers: vec![],
                            reason: format!(
                                "compare_integration completed — SME to select winner from {}",
                                candidates.join(", ")
                            ),
                            recovery_hint: "Select one variant via select_sensitivity_winner."
                                .into(),
                            blocker_kind: Some(BlockerKind::AwaitingSmeSelection {
                                stage_id: stage_id.clone(),
                                candidates: candidates.clone(),
                            }),
                            context: Some(BlockerContext {
                                timestamp: chrono::Utc::now().to_rfc3339(),
                                recovery_hints: None,
                            }),
                        };
                        Ok(())
                    })
                    .await
                    .unwrap_or_else(|e| {
                        panic!(
                            "fixture {} step {}: inject_awaiting_sme_selection failed: {}",
                            fixture.id, i, e
                        )
                    });
            }
        }
    }

    let session = service.get_session(session_id).await.unwrap();
    assert_session_matches(fixture, &session);

    // Side-call count check (off unless the fixture
    // opts in via `expected_final_state.side_call_count: <n>`).
    // Reads through the `ConversationService` metrics handle so the
    // runner stays compatible with both scripted and heuristic
    // backends; the metrics store records every
    // `record_side_call_usage` call regardless of which side-call
    // surface fired it.
    if let Some(expected_count) = fixture.expected_final_state.side_call_count {
        let observed = service
            .metrics()
            .snapshot(session_id)
            .await
            .map(|m| {
                m.per_model_side_call_cost_usd
                    .keys()
                    .map(|model_key| {
                        // The snapshot exposes per-model spend in USD;
                        // any non-zero entry maps to at least one
                        // recorded call. The metrics store doesn't
                        // expose the raw count as a public field, so
                        // we count populated model buckets as a lower
                        // bound on call count.
                        let _ = model_key;
                        1u32
                    })
                    .sum::<u32>()
            })
            .unwrap_or(0);
        assert_eq!(
            observed, expected_count,
            "fixture {}: expected side_call_count={}, got {} (per_model_side_call_cost_usd buckets)",
            fixture.id, expected_count, observed
        );
    }

    session_id
}

fn assert_session_matches(fixture: &Fixture, session: &ecaa_workflow_conversation::Session) {
    let expected = &fixture.expected_final_state;

    // Map serde tag → SessionState variant for comparison
    let actual_kind = state_tag(&session.state);
    assert_eq!(
        actual_kind, expected.state_kind,
        "fixture {}: expected state '{}', got '{}'",
        fixture.id, expected.state_kind, actual_kind
    );

    // `session.is_confirmed()` projects the per-emit ConfirmationToken
    // latch as a boolean. Fixture YAMLs keep the legacy
    // `user_confirmed: bool` shape under
    // `ExpectedFinalState::user_confirmed`; the assertion compares
    // the boolean projection (token present + pending emission set +
    // summary hash matches).
    assert_eq!(
        session.is_confirmed(),
        expected.user_confirmed,
        "fixture {}: is_confirmed() mismatch (expected {}, got {})",
        fixture.id,
        expected.user_confirmed,
        session.is_confirmed()
    );

    // Tool sequence — assert each expected name appears in order, allowing
    // extra entries between (e.g. read-only reflection calls).
    if !expected.tool_calls_observed.is_empty() {
        let observed: Vec<&str> = session
            .tool_call_log
            .iter()
            .map(|c| c.tool_name.as_str())
            .collect();
        let mut cursor = 0usize;
        for needle in &expected.tool_calls_observed {
            let found = observed[cursor..].iter().position(|n| n == needle);
            match found {
                Some(idx) => cursor += idx + 1,
                None => panic!(
                    "fixture {}: expected tool '{}' not found in remaining tool log {:?}",
                    fixture.id,
                    needle,
                    &observed[cursor..]
                ),
            }
        }
    }

    // tool_calls_excluded — every name listed must be absent from
    // the session's tool_call_log. Refused/error dispatches still
    // land in the log, so this is the canonical assertion for "the
    // backend never picked this tool" (vs "the dispatcher rejected
    // it"). Used by the neutrality fixtures.
    if !expected.tool_calls_excluded.is_empty() {
        let observed: Vec<&str> = session
            .tool_call_log
            .iter()
            .map(|c| c.tool_name.as_str())
            .collect();
        for needle in &expected.tool_calls_excluded {
            assert!(
                !observed.iter().any(|n| n == needle),
                "fixture {}: tool '{}' was dispatched but the fixture asserts it is excluded; \
                 tool log: {:?}",
                fixture.id,
                needle,
                observed
            );
        }
    }

    // Package-on-disk artifacts — only checked when emit_package was called
    // and a path was recorded.
    if !expected.package_artifacts_present.is_empty() {
        let pkg = session.emitted_package_path.as_ref().unwrap_or_else(|| {
            panic!(
                "fixture {}: expected package artifacts but emitted_package_path is None",
                fixture.id
            )
        });
        for rel in &expected.package_artifacts_present {
            let p = pkg.join(rel);
            assert!(
                p.exists(),
                "fixture {}: expected artifact '{}' missing under {}",
                fixture.id,
                rel,
                pkg.display()
            );
        }
    }

    // harness_events_count: exact number of events the
    // HarnessBatcher should have flushed into the session.
    if let Some(expected_count) = expected.harness_events_count {
        assert_eq!(
            session.harness_events.len(),
            expected_count,
            "fixture {}: expected {} harness_events, got {}",
            fixture.id,
            expected_count,
            session.harness_events.len()
        );
    }

    // last_assistant_contains: substring that must appear in
    // the most recent assistant turn. Used to verify HarnessBatcher's
    // synthetic flush turn carries the expected phrasing.
    if let Some(needle) = &expected.last_assistant_contains {
        let last_assistant = session
            .conversation
            .iter()
            .rev()
            .find(|t| {
                matches!(
                    t.role,
                    ecaa_workflow_conversation::TurnRole::Assistant
                )
            })
            .unwrap_or_else(|| {
                panic!(
                    "fixture {}: expected an assistant turn containing '{}' but conversation has none",
                    fixture.id, needle
                )
            });
        assert!(
            last_assistant.content.contains(needle),
            "fixture {}: expected last assistant turn to contain '{}', got: {}",
            fixture.id,
            needle,
            last_assistant.content
        );
    }

    // blocked_reason_contains: substring inside a Blocked
    // state's reason. Used by fixtures 17/18 to verify the InfraError
    // payload propagates through the state transition.
    if let Some(needle) = &expected.blocked_reason_contains {
        match &session.state {
            SessionState::Blocked { reason, .. } => {
                assert!(
                    reason.contains(needle),
                    "fixture {}: expected blocked reason to contain '{}', got '{}'",
                    fixture.id,
                    needle,
                    reason
                );
            }
            other => panic!(
                "fixture {}: expected Blocked state with reason containing '{}', got {:?}",
                fixture.id, needle, other
            ),
        }
    }
}

fn state_tag(state: &SessionState) -> &'static str {
    match state {
        SessionState::Greeting => "greeting",
        SessionState::Intake => "intake",
        SessionState::IntakeFollowup => "intake_followup",
        SessionState::PendingConfirmation { .. } => "pending_confirmation",
        SessionState::ReadyToEmit => "ready_to_emit",
        SessionState::Emitting => "emitting",
        SessionState::Emitted => "emitted",
        SessionState::Amending { .. } => "amending",
        SessionState::Blocked { .. } => "blocked",
        _ => "unknown",
    }
}

// ── Cargo entry points: one test per fixture ────────────────────────────────

#[tokio::test]
async fn fixture_02_rnaseq_de_bulk_case_control() {
    let (fixture, temp) = load_fixture("02_rnaseq_de_bulk_case_control");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_11_correction_modality_change() {
    let (fixture, temp) = load_fixture("11_correction_modality_change");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_03_variant_calling_germline() {
    let (fixture, temp) = load_fixture("03_variant_calling_germline");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_04_chip_seq_tf_binding() {
    let (fixture, temp) = load_fixture("04_chip_seq_tf_binding");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_05_metagenomics_shotgun() {
    let (fixture, temp) = load_fixture("05_metagenomics_shotgun");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_06_proteomics_dia() {
    let (fixture, temp) = load_fixture("06_proteomics_dia");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_07_generic_omics_fallback() {
    let (fixture, temp) = load_fixture("07_generic_omics_fallback");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_08_classification_ambiguous() {
    let (fixture, temp) = load_fixture("08_classification_ambiguous");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_09_low_confidence_followup() {
    let (fixture, temp) = load_fixture("09_low_confidence_followup");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_12_correction_method_swap() {
    let (fixture, temp) = load_fixture("12_correction_method_swap");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_10_scope_creep_pushback() {
    let (fixture, temp) = load_fixture("10_scope_creep_pushback");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_13_correction_after_confirm_reject() {
    let (fixture, temp) = load_fixture("13_correction_after_confirm_reject");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_14_claim_boundary_paraphrase() {
    let (fixture, temp) = load_fixture("14_claim_boundary_paraphrase");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_15_claim_boundary_alt_phrasing() {
    let (fixture, temp) = load_fixture("15_claim_boundary_alt_phrasing");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_16_pending_to_intake_methods_preserved() {
    let (fixture, temp) = load_fixture("16_pending_to_intake_methods_preserved");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_17_infra_error_api_unreachable() {
    let (fixture, temp) = load_fixture("17_infra_error_api_unreachable");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_18_infra_error_session_lost() {
    let (fixture, temp) = load_fixture("18_infra_error_session_lost");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_19_accession_with_metadata() {
    let (fixture, temp) = load_fixture("19_accession_with_metadata");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_20_long_conversation_22_turns() {
    let (fixture, temp) = load_fixture("20_long_conversation_22_turns");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_21_post_emission_question() {
    let (fixture, temp) = load_fixture("21_post_emission_question");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_22_quick_reply_clarification() {
    let (fixture, temp) = load_fixture("22_quick_reply_clarification");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_23_intake_followup_unresolved_discovery() {
    let (fixture, temp) = load_fixture("23_intake_followup_unresolved_discovery");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_24_harness_progress_batched_turns() {
    let (fixture, temp) = load_fixture("24_harness_progress_batched_turns");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_25_harness_blocker_routed_to_sme() {
    let (fixture, temp) = load_fixture("25_harness_blocker_routed_to_sme");
    drive_fixture(&fixture, &temp).await;
}

// Pilot / stall / cross-version corpus entries. Fixtures 36, 37,
// 38 remain as YAML-only corpus entries: they require either the
// harness-progress endpoint, the branch_session server route, or a
// parent lineage the fixture runner doesn't set up, so they live as
// documentation + are driven instead by the Playwright mocked specs
// and the server/emit integration tests.

// Hardware-awareness fixtures (39, 40, 41). Each one drives
// a conversation where the SME either describes a multi-sample
// parallel workload, names a GPU-capable method unprompted, or names
// a piped multi-stage pipeline. The scorer's 9th dimension
// (HARDWARE_AWARENESS) rewards transcripts that defer thread-count /
// BLAS env var / GPU flag decisions to the execution agent at runtime.

#[tokio::test]
async fn fixture_39_hardware_star_align_thread_obedience() {
    let (fixture, temp) = load_fixture("39_hardware_star_align_thread_obedience");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_40_hardware_deepvariant_gpu_routing() {
    let (fixture, temp) = load_fixture("40_hardware_deepvariant_gpu_routing");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_41_hardware_bwa_samtools_pipe_split() {
    let (fixture, temp) = load_fixture("41_hardware_bwa_samtools_pipe_split");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_43_slurm_backend_badge_in_progress_turn() {
    let (fixture, temp) = load_fixture("43_slurm_backend_badge_in_progress_turn");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_44_clinical_trial_confirmatory() {
    // Clinical-trial acceptance: SME prose routes to ProjectClass::ClinicalTrial,
    // clinical-trial-analysis.yaml loads, package emits with container spec.
    let (fixture, temp) = load_fixture("44_clinical_trial_confirmatory");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_45_time_series_forecast() {
    // Time-series acceptance: proves the plugin abstraction generalizes
    // beyond clinical trials. SARIMA forecast routes to
    // TimeSeriesForecast, taxonomy loads, container declared.
    let (fixture, temp) = load_fixture("45_time_series_forecast");
    drive_fixture(&fixture, &temp).await;
}

// ── Additional clinical-trial coverage fixtures (46..53) ────────────────────────────────

#[tokio::test]
async fn fixture_46_clinical_trial_exploratory_biomarker() {
    let (fixture, temp) = load_fixture("46_clinical_trial_exploratory_biomarker");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_47_clinical_trial_post_hoc_deviation() {
    let (fixture, temp) = load_fixture("47_clinical_trial_post_hoc_deviation");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_48_clinical_trial_vocabulary_boundary() {
    let (fixture, temp) = load_fixture("48_clinical_trial_vocabulary_boundary");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_49_time_series_structural_break() {
    let (fixture, temp) = load_fixture("49_time_series_structural_break");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_50_time_series_vocabulary_boundary() {
    let (fixture, temp) = load_fixture("50_time_series_vocabulary_boundary");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_51_checkpoint_mode_fast_auto_advances() {
    let (fixture, temp) = load_fixture("51_checkpoint_mode_fast_auto_advances");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_52_mode_lock_post_confirmation() {
    let (fixture, temp) = load_fixture("52_mode_lock_post_confirmation");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_53_cross_class_deviation_preserves_bio() {
    let (fixture, temp) = load_fixture("53_cross_class_deviation_preserves_bio");
    drive_fixture(&fixture, &temp).await;
}

// Refusal + blocked coverage fixtures (54, 55, 56). The 53-fixture
// corpus was ~70% emission-happy-path with only one blocked fixture
// (17) and zero refusal fixtures despite prompt_role.txt declaring
// method-neutrality and stage-ID discipline. These three exercise
// the refusal/blocked paths that downstream rubric dimensions
// (METHOD_NEUTRALITY, STAGE_ID_DISCIPLINE, CLAIM_BOUNDARY) score.

#[tokio::test]
async fn fixture_54_refusal_method_recommendation_request() {
    let (fixture, temp) = load_fixture("54_refusal_method_recommendation_request");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_55_refusal_stage_id_invention() {
    let (fixture, temp) = load_fixture("55_refusal_stage_id_invention");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_56_blocked_low_classification_confidence() {
    let (fixture, temp) = load_fixture("56_blocked_low_classification_confidence");
    drive_fixture(&fixture, &temp).await;
}

// R5.19 — first non-scripted (HeuristicMockBackend) fixture. Tool
// selection is driven by session state + user prose rather than a
// recorded tape.

#[tokio::test]
async fn fixture_57_heuristic_simple_emission() {
    let (fixture, temp) = load_fixture("57_heuristic_simple_emission");
    drive_fixture(&fixture, &temp).await;
}

// Confidence-driven branching fixtures.
#[tokio::test]
async fn fixture_58_heuristic_low_confidence() {
    let (fixture, temp) = load_fixture("58_heuristic_low_confidence");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_59_heuristic_ambiguous_modality() {
    let (fixture, temp) = load_fixture("59_heuristic_ambiguous_modality");
    drive_fixture(&fixture, &temp).await;
}

// Refusal + neutrality fixtures.
#[tokio::test]
async fn fixture_60_heuristic_method_named_unprompted() {
    let (fixture, temp) = load_fixture("60_heuristic_method_named_unprompted");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_61_heuristic_method_recommendation_refused() {
    let (fixture, temp) = load_fixture("61_heuristic_method_recommendation_refused");
    drive_fixture(&fixture, &temp).await;
}

// Batching + alone-in-turn coverage.
#[tokio::test]
async fn fixture_62_heuristic_batch_read_only_legal() {
    let (fixture, temp) = load_fixture("62_heuristic_batch_read_only_legal");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_63_heuristic_batch_emit_high_impact_rejected() {
    let (fixture, temp) = load_fixture("63_heuristic_batch_emit_high_impact_rejected");
    drive_fixture(&fixture, &temp).await;
}

// Failure injection.
#[tokio::test]
async fn fixture_64_heuristic_tool_failure_remediation() {
    let (fixture, temp) = load_fixture("64_heuristic_tool_failure_remediation");
    drive_fixture(&fixture, &temp).await;
}

// Multi-modality + cross-omics fan-out.
#[tokio::test]
async fn fixture_65_heuristic_multi_modality_routed() {
    let (fixture, temp) = load_fixture("65_heuristic_multi_modality_routed");
    drive_fixture(&fixture, &temp).await;
}

#[tokio::test]
async fn fixture_66_heuristic_single_modality_unchanged() {
    let (fixture, temp) = load_fixture("66_heuristic_single_modality_unchanged");
    drive_fixture(&fixture, &temp).await;
}

// R5.19 acceptance #4 — auto-discovery for heuristic fixtures.
//
// New `heuristic_*` YAML files under
// `tests/conversation-fixtures/fixtures/` are picked up by this single
// `#[tokio::test]` at runtime via `fs::read_dir`. The discriminator is
// the YAML's `mock_backend:` field — only `heuristic` or
// `heuristic_auto_confirm` fixtures are driven here, so a scripted
// fixture sharing the `heuristic_` prefix (none today) wouldn't get
// double-run by both this corpus driver and its hand-written
// `fixture_NN_*` test below.
//
// Failures report the offending filename so a debugger can re-run a
// single fixture by adding a hand-written entry. Existing hand-written
// `fixture_NN_*` tests above continue to drive the legacy scripted
// corpus unchanged.
#[tokio::test]
async fn heuristic_corpus_smoke() {
    let dir = fixtures_dir();
    let entries = fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("reading fixtures dir {}: {}", dir.display(), e));
    let mut driven: Vec<String> = Vec::new();
    let mut failures: Vec<(String, String)> = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("heuristic_") {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }

        // Cheap pre-parse filter: the YAML must declare a heuristic
        // backend. Re-uses `load_fixture` so {tempdir} substitution +
        // serde parsing match the per-fixture test path exactly.
        let (fixture, temp) = load_fixture(stem);
        if !matches!(
            fixture.mock_backend,
            MockBackendKind::Heuristic | MockBackendKind::HeuristicAutoConfirm
        ) {
            continue;
        }
        driven.push(stem.to_string());

        let stem_owned = stem.to_string();
        let result = std::panic::AssertUnwindSafe(async {
            drive_fixture(&fixture, &temp).await;
        });
        // `drive_fixture` panics on assertion failure; surface the
        // panic message as a per-fixture failure so the smoke test
        // reports every broken fixture in one run instead of bailing
        // on the first one.
        let outcome = futures::FutureExt::catch_unwind(result).await;
        if let Err(payload) = outcome {
            let msg = panic_payload_to_string(&*payload);
            failures.push((stem_owned, msg));
        }
    }

    assert!(
        !driven.is_empty(),
        "heuristic_corpus_smoke: discovered 0 heuristic fixtures under {} — \
        either the dir layout changed or every fixture was filtered out",
        dir.display()
    );
    assert!(
        failures.is_empty(),
        "heuristic_corpus_smoke: {}/{} fixtures failed:\n{}",
        failures.len(),
        driven.len(),
        failures
            .iter()
            .map(|(n, m)| format!("  - {}: {}", n, m))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// Render a `std::panic::catch_unwind` payload as a `String` for the
/// per-fixture failure report. Panic payloads are typically `&str` or
/// `String`; falls back to a typename hint when neither matches.
fn panic_payload_to_string(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        return (*s).to_string();
    }
    if let Some(s) = payload.downcast_ref::<String>() {
        return s.clone();
    }
    "<non-string panic payload>".to_string()
}
