// S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
// `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
// edition because the env table is not thread-safe). All call sites
// are single-threaded test setup/teardown; the bounded waiver is
// scoped to this `mod tests` block.
#![allow(unsafe_code)]
use super::*;
use crate::model_policy::ModelId;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn empty_snapshot_is_none() {
    let store = MetricsStore::new();
    assert!(store.snapshot(Uuid::new_v4()).await.is_none());
}

#[test]
fn resolve_model_api_id_strips_context_variant_suffix() {
    // Claude Code CLI's `-p --output-format=json` reports context
    // variants as `claude-opus-4-8[1m]` / `claude-sonnet-4-6[200k]`.
    // Without the suffix-strip these fall through to the Sonnet 4.6
    // fallback and the session's `agent_cost_usd` under-reports by
    // the Opus/Sonnet pricing delta (≈5×).
    assert_eq!(resolve_model_api_id("claude-opus-4-8[1m]"), ModelId::Opus48);
    assert_eq!(
        resolve_model_api_id("claude-sonnet-4-6[200k]"),
        ModelId::Sonnet46
    );
    assert_eq!(resolve_model_api_id("claude-opus-4-8"), ModelId::Opus48);
    assert_eq!(resolve_model_api_id("claude-sonnet-4-6"), ModelId::Sonnet46);
    assert_eq!(
        resolve_model_api_id("claude-haiku-4-5-20251001"),
        ModelId::Haiku45
    );
    // Genuinely unknown model still falls back (preserves existing
    // "under-report rather than over-report new models" policy).
    assert_eq!(resolve_model_api_id("claude-future-v9"), ModelId::Sonnet46);
}

/// G.1 — `session_token_budget` surfaces the configured ceiling the
/// UI progress bar reads. Semantics must match the tool-loop's
/// `check_budget` helper so operators see the same number the soft-
/// block actually enforces.
///
/// These three tests share the process env; they're serialized via a
/// static Mutex so a parallel `cargo test` run never interleaves them.
/// Restoring the prior value on every exit keeps the rest of the
/// suite deterministic.
mod budget {
    use super::super::read_session_token_budget;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard(Option<String>);
    impl EnvGuard {
        fn capture() -> Self {
            Self(std::env::var("ECAA_SESSION_TOKEN_BUDGET").ok())
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: env access is serialized by ENV_LOCK above.
            match &self.0 {
                Some(v) => unsafe { std::env::set_var("ECAA_SESSION_TOKEN_BUDGET", v) },
                None => unsafe { std::env::remove_var("ECAA_SESSION_TOKEN_BUDGET") },
            }
        }
    }

    #[test]
    fn defaults_to_500k_when_unset() {
        let _g = ENV_LOCK.lock().unwrap();
        let _restore = EnvGuard::capture();
        unsafe { std::env::remove_var("ECAA_SESSION_TOKEN_BUDGET") };
        assert_eq!(read_session_token_budget(), Some(500_000));
    }

    #[test]
    fn explicit_value_wins() {
        let _g = ENV_LOCK.lock().unwrap();
        let _restore = EnvGuard::capture();
        unsafe { std::env::set_var("ECAA_SESSION_TOKEN_BUDGET", "750000") };
        assert_eq!(read_session_token_budget(), Some(750_000));
    }

    #[test]
    fn zero_disables_the_budget() {
        let _g = ENV_LOCK.lock().unwrap();
        let _restore = EnvGuard::capture();
        unsafe { std::env::set_var("ECAA_SESSION_TOKEN_BUDGET", "0") };
        assert_eq!(read_session_token_budget(), None);
    }
}

#[tokio::test]
async fn turn_count_and_token_totals_accumulate() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_turn(
            id,
            Duration::from_millis(100),
            2,
            200,
            50,
            0,
            1500,
            ModelId::Sonnet46,
        )
        .await;
    store
        .record_turn(
            id,
            Duration::from_millis(300),
            1,
            150,
            80,
            1500,
            0,
            ModelId::Opus46,
        )
        .await;
    let m = store.snapshot(id).await.unwrap();
    assert_eq!(m.turn_count, 2);
    assert_eq!(m.tool_call_count, 3);
    assert_eq!(m.total_input_tokens, 350);
    assert_eq!(m.total_output_tokens, 130);
    assert_eq!(m.cache_creation_tokens, 1500);
    assert_eq!(m.cache_read_tokens, 1500);
    assert_eq!(m.sonnet_turns, 1);
    assert_eq!(m.opus_turns, 1);
}

#[tokio::test]
async fn percentiles_handle_realistic_distribution() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Insert 100 durations from 10ms to 1000ms
    for ms in (10..=1000).step_by(10) {
        store
            .record_turn(
                id,
                Duration::from_millis(ms),
                1,
                0,
                0,
                0,
                0,
                ModelId::Sonnet46,
            )
            .await;
    }
    let m = store.snapshot(id).await.unwrap();
    assert_eq!(m.turn_count, 100);
    // p50 of 10..1000 step 10 should be around the median (~500ms)
    assert!(m.p50_turn_ms >= 480 && m.p50_turn_ms <= 520);
    assert!(m.p95_turn_ms >= 940 && m.p95_turn_ms <= 960);
    assert_eq!(m.max_turn_ms, 1000);
}

#[tokio::test]
async fn forget_clears_metrics() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_turn(
            id,
            Duration::from_millis(50),
            0,
            0,
            0,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    assert!(store.snapshot(id).await.is_some());
    store.forget(id).await;
    assert!(store.snapshot(id).await.is_none());
}

#[tokio::test]
async fn instance_seconds_accumulate_across_completed_tasks() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Two tasks on r6i.4xlarge — 60s + 30s = 90s total.
    store
        .record_task_started(id, "alignment_quantification_a", "r6i.4xlarge", 1_000_000)
        .await;
    store
        .record_task_completed(id, "alignment_quantification_a", 1_060_000)
        .await;
    store
        .record_task_started(id, "alignment_quantification_b", "r6i.4xlarge", 1_100_000)
        .await;
    store
        .record_task_completed(id, "alignment_quantification_b", 1_130_000)
        .await;
    // One task on g6.xlarge — 120s.
    store
        .record_task_started(id, "deepvariant_run", "g6.xlarge", 2_000_000)
        .await;
    store
        .record_task_completed(id, "deepvariant_run", 2_120_000)
        .await;

    let snap = store.snapshot(id).await.expect("snapshot");
    assert_eq!(snap.total_instance_seconds, 90 + 120);
    assert_eq!(snap.instance_type_seconds.get("r6i.4xlarge"), Some(&90));
    assert_eq!(snap.instance_type_seconds.get("g6.xlarge"), Some(&120));
}

#[tokio::test]
async fn task_completed_without_start_is_silent_noop() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Bump turn_count so a snapshot exists; otherwise snapshot is None.
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            0,
            0,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    store
        .record_task_completed(id, "untracked_task", 5_000_000)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.total_instance_seconds, 0);
    assert!(snap.instance_type_seconds.is_empty());
}

#[tokio::test]
async fn cost_prices_sonnet_and_opus_turns_separately() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Sonnet turn: 1M input, 200k output, 500k cache read, 100k cache write.
    // Expected: 1*3 + 0.2*15 + 0.5*0.30 + 0.1*3.75 = 3 + 3 + 0.15 + 0.375 = 6.525
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            1_000_000,
            200_000,
            500_000,
            100_000,
            ModelId::Sonnet46,
        )
        .await;
    // Opus turn: 100k input, 20k output, no cache.
    // Expected: 0.1*5 + 0.02*25 = 0.5 + 0.5 = 1.0
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100_000,
            20_000,
            0,
            0,
            ModelId::Opus46,
        )
        .await;
    let m = store.snapshot(id).await.unwrap();
    assert!(
        (m.sonnet_cost_usd - 6.525).abs() < 1e-6,
        "{}",
        m.sonnet_cost_usd
    );
    assert!((m.opus_cost_usd - 1.0).abs() < 1e-6, "{}", m.opus_cost_usd);
    assert!(
        (m.total_cost_usd - 7.525).abs() < 1e-6,
        "{}",
        m.total_cost_usd
    );
    // Totals still sum across both models.
    assert_eq!(m.total_input_tokens, 1_100_000);
    assert_eq!(m.total_output_tokens, 220_000);
    assert_eq!(m.cache_read_tokens, 500_000);
    assert_eq!(m.cache_creation_tokens, 100_000);
}

#[tokio::test]
async fn cache_hit_ratio_reflects_read_vs_billed_input() {
    // A session with 100 uncached + 400 cache-read + 0 cache-write
    // inputs should report ratio 0.8. Empty sessions report 0.0 so
    // the UI can distinguish "no data yet" from "cache is broken".
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    assert_eq!(
        store.snapshot(id).await.map(|m| m.cache_hit_ratio),
        None,
        "empty session should produce no snapshot"
    );
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100,
            50,
            400,
            0,
            ModelId::Sonnet46,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!(
        (snap.cache_hit_ratio - 0.8).abs() < 1e-6,
        "ratio {} != 0.8",
        snap.cache_hit_ratio
    );
}

#[tokio::test]
async fn opus_turns_and_cost_aggregate_across_4_6_and_4_8() {
    // Upgrade invariant: the legacy `opus_turns` / `opus_cost_usd`
    // UI mirrors must aggregate BOTH Opus 4.6 and Opus 4.8 so the
    // Metrics tab row stays continuous across the escalation-target
    // upgrade. A session with mixed Opus spend (legacy sidecar
    // carrying 4.6, new turns on 4.8) should show total counts +
    // cost in those mirrors.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Opus 4.6 legacy turn: 100k input, 20k output.
    // 0.1*5 + 0.02*25 = 0.5 + 0.5 = $1.00
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100_000,
            20_000,
            0,
            0,
            ModelId::Opus46,
        )
        .await;
    // Opus 4.8 current turn: 200k input, 40k output.
    // 0.2*5 + 0.04*25 = 1.0 + 1.0 = $2.00
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            200_000,
            40_000,
            0,
            0,
            ModelId::Opus48,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.opus_turns, 2, "opus_turns should aggregate 4.6 + 4.8");
    assert!(
        (snap.opus_cost_usd - 3.0).abs() < 1e-6,
        "opus_cost_usd should sum $1.00 (4.6) + $2.00 (4.8) = $3.00, got {}",
        snap.opus_cost_usd
    );
    // per_model_cost_usd still breaks out each variant for the
    // extra-models row (now filtered out of the table — see
    // MetricsTab.tsx KNOWN_FLAT).
    assert!(snap.per_model_cost_usd.contains_key("opus_4_6"));
    assert!(snap.per_model_cost_usd.contains_key("opus_4_8"));
}

#[tokio::test]
async fn tool_loop_iterations_histogram_accumulates_per_count() {
    // §3.2/§4.3 — exact-bucket histogram so the shape of the
    // iteration distribution is visible (not just a mean). Three
    // turns at 3 iterations each plus one turn at 8 iterations
    // should produce {3: 3, 8: 1}.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    for _ in 0..3 {
        store.record_tool_loop_iterations(id, 3).await;
    }
    store.record_tool_loop_iterations(id, 8).await;
    // Bump turn_count so snapshot() returns something
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            0,
            0,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.tool_loop_iterations_histogram.get(&3), Some(&3));
    assert_eq!(snap.tool_loop_iterations_histogram.get(&8), Some(&1));
}

/// `record_composer_run` aggregates duration +
/// atom-consideration + backtrack + exclusion-hit counters into
/// the session's totals so the Performance tab can surface them.
#[tokio::test]
async fn composer_counters_aggregate_across_runs() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_composer_run(id, 100, 12, 3, 5).await;
    store.record_composer_run(id, 50, 8, 0, 1).await;
    // Bump turn_count so snapshot() has something to render.
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            0,
            0,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.composer_runs, 2);
    assert_eq!(snap.composer_total_duration_ms, 150);
    assert_eq!(snap.composer_atoms_considered, 20);
    assert_eq!(snap.composer_backtracks, 3);
    assert_eq!(snap.composer_exclusion_hits, 6);
}

#[tokio::test]
async fn opus_escalation_reasons_counted_per_cause() {
    // §3.3/§4.2 — attribution lets operators see which trigger
    // drives their Opus spend. Two careful_mode turns and one
    // blocked turn should produce {careful_mode: 2, blocked: 1}.
    use crate::model_policy::EscalationReason;
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_opus_escalation(id, EscalationReason::CarefulMode)
        .await;
    store
        .record_opus_escalation(id, EscalationReason::CarefulMode)
        .await;
    store
        .record_opus_escalation(id, EscalationReason::Blocked)
        .await;
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            0,
            0,
            0,
            0,
            ModelId::Opus46,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.opus_escalation_reasons.get("careful_mode"), Some(&2));
    assert_eq!(snap.opus_escalation_reasons.get("blocked"), Some(&1));
    assert!(!snap.opus_escalation_reasons.contains_key("low_confidence"));
}

#[tokio::test]
async fn cache_hit_ratio_is_zero_when_no_input() {
    // A session with only output tokens (unlikely but possible for
    // edge cases) must not divide by zero.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            0,
            100,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.cache_hit_ratio, 0.0);
}

#[tokio::test]
async fn high_water_exceeded_counter_increments() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_high_water_exceeded(id).await;
    store.record_high_water_exceeded(id).await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.high_water_exceeded_count, 2);
}

#[test]
fn every_model_id_has_pricing() {
    // A new ModelId variant without a pricing entry would silently
    // fall through to zeros inside the per_model_cost_usd map, which
    // would under-report cost with no visible error. This test pins
    // coverage: every variant from ModelId::ALL must yield a non-zero
    // rate for input + output.
    for &m in ModelId::ALL {
        let p = pricing::prices_for(m);
        assert!(
            p.input_per_mtok > 0.0,
            "{:?} has zero input price — did a new variant land without updating the pricing table?",
            m
        );
        assert!(p.output_per_mtok > 0.0, "{:?} has zero output price", m);
    }
}

#[test]
fn published_rates_match_april_2026_list_pricing() {
    // Guardrail against stale pricing constants. These are Anthropic's
    // published list rates (per million tokens, 5-minute ephemeral
    // cache tier). If Anthropic updates the published
    // rates, update this test alongside the constants in the same PR.
    assert_eq!(pricing::SONNET_4_6.input_per_mtok, 3.00);
    assert_eq!(pricing::SONNET_4_6.output_per_mtok, 15.00);
    assert_eq!(pricing::SONNET_4_6.cache_write_per_mtok, 3.75);
    assert_eq!(pricing::SONNET_4_6.cache_read_per_mtok, 0.30);
    // Opus 4.6 + 4.8 rates: the $15/$75 figures from earlier
    // versions (Opus 4.1 and prior) were retired with Opus 4.5's
    // launch. Opus 4.5 / 4.6 / 4.8 all share $5/$25 input/output.
    // Update these alongside the constants in the same PR if
    // Anthropic re-prices.
    assert_eq!(pricing::OPUS_4_6.input_per_mtok, 5.00);
    assert_eq!(pricing::OPUS_4_6.output_per_mtok, 25.00);
    assert_eq!(pricing::OPUS_4_6.cache_write_per_mtok, 6.25);
    assert_eq!(pricing::OPUS_4_6.cache_read_per_mtok, 0.50);
    assert_eq!(pricing::OPUS_4_8.input_per_mtok, 5.00);
    assert_eq!(pricing::OPUS_4_8.output_per_mtok, 25.00);
    assert_eq!(pricing::OPUS_4_8.cache_write_per_mtok, 6.25);
    assert_eq!(pricing::OPUS_4_8.cache_read_per_mtok, 0.50);
    assert_eq!(pricing::HAIKU_4_5.input_per_mtok, 1.00);
    assert_eq!(pricing::HAIKU_4_5.output_per_mtok, 5.00);
    assert_eq!(pricing::HAIKU_4_5.cache_write_per_mtok, 1.25);
    assert_eq!(pricing::HAIKU_4_5.cache_read_per_mtok, 0.10);
}

#[tokio::test]
async fn haiku_turn_records_and_prices_correctly() {
    // End-to-end: a Haiku 4.5 turn flows through record_turn,
    // lands in per_model[Haiku45], and shows up in both the
    // per-model breakdown and (not) in the Sonnet/Opus mirrors.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // 1M input @ $1 + 1M output @ $5 = $6 total
    store
        .record_turn(
            id,
            Duration::from_millis(50),
            0,
            1_000_000,
            1_000_000,
            0,
            0,
            ModelId::Haiku45,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!((snap.total_cost_usd - 6.0).abs() < 1e-6);
    assert_eq!(snap.sonnet_turns, 0);
    assert_eq!(snap.opus_turns, 0);
    assert_eq!(snap.sonnet_cost_usd, 0.0);
    assert_eq!(snap.opus_cost_usd, 0.0);
    assert_eq!(snap.per_model_turns.get("haiku_4_5"), Some(&1));
    assert!((snap.per_model_cost_usd.get("haiku_4_5").copied().unwrap() - 6.0).abs() < 1e-6);
}

#[test]
fn legacy_sidecar_shape_deserializes_into_per_model_map() {
    // Sidecars written before Bundle C carry flat sonnet_* / opus_*
    // fields. The custom Deserialize impl must fold them into the
    // per_model map so a rolling upgrade reads old data correctly.
    let legacy = r#"{
        "turn_count": 3,
        "tool_call_count": 5,
        "turn_durations_ms": [100, 200, 300],
        "sonnet_count": 2,
        "opus_count": 1,
        "sonnet_input_tokens": 1000,
        "sonnet_output_tokens": 500,
        "sonnet_cache_read_tokens": 200,
        "sonnet_cache_creation_tokens": 100,
        "opus_input_tokens": 400,
        "opus_output_tokens": 200,
        "opus_cache_read_tokens": 0,
        "opus_cache_creation_tokens": 50,
        "instance_seconds": {"r6i.4xlarge": 600},
        "running_task_starts": {},
        "high_water_exceeded_count": 0,
        "batch_dropped_events": 0
    }"#;
    let counters: SessionCounters = serde_json::from_str(legacy).unwrap();
    assert_eq!(counters.turn_count, 3);
    let sonnet = counters.per_model.get(&ModelId::Sonnet46).unwrap();
    assert_eq!(sonnet.count, 2);
    assert_eq!(sonnet.input_tokens, 1000);
    assert_eq!(sonnet.output_tokens, 500);
    assert_eq!(sonnet.cache_read_tokens, 200);
    assert_eq!(sonnet.cache_creation_tokens, 100);
    let opus = counters.per_model.get(&ModelId::Opus46).unwrap();
    assert_eq!(opus.count, 1);
    assert_eq!(opus.input_tokens, 400);
    assert_eq!(opus.cache_creation_tokens, 50);
    // Snapshot-level mirrors pick up the legacy fields too.
    let snap = counters.snapshot();
    assert_eq!(snap.sonnet_turns, 2);
    assert_eq!(snap.opus_turns, 1);
    assert!(snap.total_cost_usd > 0.0);
}

#[tokio::test]
async fn record_agent_usage_splits_cost_from_chat_turns() {
    // Bundle D: chat and agent spend land in separate buckets but
    // both price via the ModelId-indexed pricing table. The UI
    // surfaces them as distinct lines so the SME can see where
    // the money went.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Chat turn: 100k input, 20k output on Sonnet 4.6.
    // 0.1 * 3 + 0.02 * 15 = 0.3 + 0.3 = 0.6
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100_000,
            20_000,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    // Agent task: 2M input, 500k output on Sonnet 4.6.
    // 2 * 3 + 0.5 * 15 = 6 + 7.5 = 13.5
    store
        .record_agent_usage(id, "claude-sonnet-4-6", 2_000_000, 500_000, 0, 0)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!(
        (snap.chat_cost_usd - 0.6).abs() < 1e-6,
        "chat {}",
        snap.chat_cost_usd
    );
    assert!(
        (snap.agent_cost_usd - 13.5).abs() < 1e-6,
        "agent {}",
        snap.agent_cost_usd
    );
    assert!(
        (snap.total_cost_usd - 14.1).abs() < 1e-6,
        "total {}",
        snap.total_cost_usd
    );
    // per_model_cost_usd shows chat only; per_model_agent_cost_usd shows
    // the agent portion. UI keys on these separately.
    assert!((snap.per_model_cost_usd.get("sonnet_4_6").copied().unwrap() - 0.6).abs() < 1e-6);
    assert!(
        (snap
            .per_model_agent_cost_usd
            .get("sonnet_4_6")
            .copied()
            .unwrap()
            - 13.5)
            .abs()
            < 1e-6
    );
    // Total tokens in SessionMetrics aggregate across BOTH sources.
    assert_eq!(snap.total_input_tokens, 2_100_000);
    assert_eq!(snap.total_output_tokens, 520_000);
    // F6: per-surface tokens split the aggregate by bucket. Chat
    // chat has 100k in / 20k out; agent has 2M in / 500k out. The
    // sum across all four surfaces equals the flat totals above.
    assert_eq!(snap.per_surface_tokens.chat.input_tokens, 100_000);
    assert_eq!(snap.per_surface_tokens.chat.output_tokens, 20_000);
    assert_eq!(snap.per_surface_tokens.agent.input_tokens, 2_000_000);
    assert_eq!(snap.per_surface_tokens.agent.output_tokens, 500_000);
    assert_eq!(snap.per_surface_tokens.scorer.input_tokens, 0);
    assert_eq!(snap.per_surface_tokens.side_call.input_tokens, 0);
    let sum_in = snap.per_surface_tokens.chat.input_tokens
        + snap.per_surface_tokens.agent.input_tokens
        + snap.per_surface_tokens.scorer.input_tokens
        + snap.per_surface_tokens.side_call.input_tokens;
    assert_eq!(sum_in, snap.total_input_tokens);
}

#[tokio::test]
async fn per_task_agent_breakdown_sorted_by_cost() {
    // F1 regression: record three agent tasks with distinct cost
    // profiles (different token totals on the same model) and
    // verify the snapshot's per_task_agent vec is sorted descending.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // task A: 500k in, 100k out → ~$3.0 on Sonnet
    store
        .record_agent_usage_for_task(
            id,
            Some("validate_metadata_harmonization"),
            "claude-sonnet-4-6",
            500_000,
            100_000,
            0,
            0,
        )
        .await;
    // task B: 200k in, 50k out → ~$1.35
    store
        .record_agent_usage_for_task(
            id,
            Some("preprocessing"),
            "claude-sonnet-4-6",
            200_000,
            50_000,
            0,
            0,
        )
        .await;
    // task C: 100k in, 20k out → ~$0.6
    store
        .record_agent_usage_for_task(
            id,
            Some("data_acquisition"),
            "claude-sonnet-4-6",
            100_000,
            20_000,
            0,
            0,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.per_task_agent.len(), 3);
    // Sorted by cost desc.
    assert_eq!(
        snap.per_task_agent[0].task_id,
        "validate_metadata_harmonization"
    );
    assert_eq!(snap.per_task_agent[1].task_id, "preprocessing");
    assert_eq!(snap.per_task_agent[2].task_id, "data_acquisition");
    // Per-task tokens preserved.
    assert_eq!(snap.per_task_agent[0].input_tokens, 500_000);
    assert_eq!(snap.per_task_agent[0].output_tokens, 100_000);
    // Aggregate agent_cost still equals sum of per-task costs.
    let sum: f64 = snap.per_task_agent.iter().map(|t| t.cost_usd).sum();
    assert!((snap.agent_cost_usd - sum).abs() < 1e-6);
}

#[tokio::test]
async fn cache_savings_priced_per_surface() {
    // F5: chat turn with 1M cache reads on Sonnet (3 - 0.3 per MTok
    // = 2.7) → $2.70 saved; agent task with 2M cache reads on Sonnet
    // → $5.40 saved. Total $8.10. Per-surface map shows the split.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100, // small uncached input so chat surface registers
            100,
            1_000_000, // 1M cache reads
            0,
            ModelId::Sonnet46,
        )
        .await;
    store
        .record_agent_usage_for_task(
            id,
            Some("preprocessing"),
            "claude-sonnet-4-6",
            100,
            100,
            2_000_000, // 2M cache reads
            0,
        )
        .await;
    let snap = store.snapshot(id).await.unwrap();
    // Chat: 1M × 2.7 = 2.7
    // Agent: 2M × 2.7 = 5.4
    // Total: 8.1
    assert!(
        (snap.cache_savings_usd - 8.1).abs() < 1e-6,
        "savings {}",
        snap.cache_savings_usd
    );
    assert!(
        (snap
            .per_surface_cache_savings_usd
            .get("chat")
            .copied()
            .unwrap()
            - 2.7)
            .abs()
            < 1e-6,
    );
    assert!(
        (snap
            .per_surface_cache_savings_usd
            .get("agent")
            .copied()
            .unwrap()
            - 5.4)
            .abs()
            < 1e-6,
    );
    assert!(!snap.per_surface_cache_savings_usd.contains_key("scorer"),);
}

#[tokio::test]
async fn tool_call_breakdown_buckets_by_name() {
    // F2 regression: each invocation of `record_tool_call` increments
    // the per-name bucket. Empty buckets stay absent.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_tool_call(id, "append_intake_prose").await;
    store.record_tool_call(id, "append_intake_prose").await;
    store.record_tool_call(id, "classify_intake").await;
    store.record_tool_call(id, "emit_package").await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.tool_calls_by_name.get("append_intake_prose"), Some(&2));
    assert_eq!(snap.tool_calls_by_name.get("classify_intake"), Some(&1));
    assert_eq!(snap.tool_calls_by_name.get("emit_package"), Some(&1));
    assert_eq!(snap.tool_calls_by_name.get("nonexistent_tool"), None);
}

#[tokio::test]
async fn record_agent_usage_legacy_signature_still_works() {
    // Backward compat: callers using `record_agent_usage` (no
    // task_id) must still increment the aggregate counters and
    // produce a valid snapshot — they just won't appear in the
    // per_task_agent breakdown.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_agent_usage(id, "claude-sonnet-4-6", 100_000, 20_000, 0, 0)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!(snap.agent_cost_usd > 0.0);
    assert_eq!(snap.per_task_agent.len(), 0);
}

#[tokio::test]
async fn scorer_usage_bucketed_separately_from_chat_and_agent() {
    // Three-way split: chat / agent / scorer each price independently
    // and sum to total_cost_usd. Scorer spend is NOT folded into
    // chat_cost_usd — it's its own row in the Performance tab.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Chat turn on Sonnet: 100k input, 20k output.
    // 0.1 * 3 + 0.02 * 15 = 0.3 + 0.3 = 0.6
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100_000,
            20_000,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    // Agent task on Sonnet: 1M input, 200k output.
    // 1 * 3 + 0.2 * 15 = 3 + 3 = 6
    store
        .record_agent_usage(id, "claude-sonnet-4-6", 1_000_000, 200_000, 0, 0)
        .await;
    // Scorer call on Sonnet: 500k input, 100k output.
    // 0.5 * 3 + 0.1 * 15 = 1.5 + 1.5 = 3.0
    store
        .record_scorer_usage(id, ModelId::Sonnet46, 500_000, 100_000, 0, 0)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!(
        (snap.chat_cost_usd - 0.6).abs() < 1e-6,
        "chat {}",
        snap.chat_cost_usd
    );
    assert!(
        (snap.agent_cost_usd - 6.0).abs() < 1e-6,
        "agent {}",
        snap.agent_cost_usd
    );
    assert!(
        (snap.scorer_cost_usd - 3.0).abs() < 1e-6,
        "scorer {}",
        snap.scorer_cost_usd
    );
    assert!(
        (snap.total_cost_usd - 9.6).abs() < 1e-6,
        "total {}",
        snap.total_cost_usd
    );
    assert!(
        (snap
            .per_model_scorer_cost_usd
            .get("sonnet_4_6")
            .copied()
            .unwrap()
            - 3.0)
            .abs()
            < 1e-6
    );
    // Top-level token totals aggregate across all three sources.
    assert_eq!(snap.total_input_tokens, 1_600_000);
    assert_eq!(snap.total_output_tokens, 320_000);
}

#[tokio::test]
async fn side_call_usage_bucketed_separately_and_sums_into_total() {
    // Fourth cost bucket: cheap Haiku-powered side calls
    // (auto-title is the first one) route through
    // `record_side_call_usage` rather than the chat / agent /
    // scorer paths. `side_call_cost_usd` is its own line; the
    // four-bucket sum invariant (chat + agent + scorer + side_call
    // == total_cost_usd) holds.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // Chat turn on Sonnet: 100k input, 20k output → $0.60
    store
        .record_turn(
            id,
            Duration::from_millis(10),
            0,
            100_000,
            20_000,
            0,
            0,
            ModelId::Sonnet46,
        )
        .await;
    // Side call on Haiku 4.5: 2k input, 30 output
    // 0.002 * 1 + 0.00003 * 5 = 0.002 + 0.00015 = 0.00215
    store
        .record_side_call_usage(id, ModelId::Haiku45, 2_000, 30, 0, 0)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    assert!(
        (snap.chat_cost_usd - 0.6).abs() < 1e-6,
        "chat {}",
        snap.chat_cost_usd
    );
    assert!(
        (snap.side_call_cost_usd - 0.00215).abs() < 1e-6,
        "side_call {}",
        snap.side_call_cost_usd
    );
    // Four-bucket invariant: chat + agent + scorer + side_call == total.
    let expected_total =
        snap.chat_cost_usd + snap.agent_cost_usd + snap.scorer_cost_usd + snap.side_call_cost_usd;
    assert!(
        (snap.total_cost_usd - expected_total).abs() < 1e-9,
        "four-bucket sum diverged: total={} sum={}",
        snap.total_cost_usd,
        expected_total
    );
    // Side-call spend is bucketed under haiku_4_5 (first and likely
    // only key for side calls).
    assert!(
        (snap
            .per_model_side_call_cost_usd
            .get("haiku_4_5")
            .copied()
            .unwrap_or(0.0)
            - 0.00215)
            .abs()
            < 1e-6
    );
    // Side-call tokens fold into the top-level input / output
    // counters so "every billable token is counted" stays true.
    assert_eq!(snap.total_input_tokens, 102_000);
    assert_eq!(snap.total_output_tokens, 20_030);
}

#[tokio::test]
async fn unknown_agent_model_falls_back_to_sonnet_with_warning() {
    // When the agent reports a model string that predates the
    // pricing table (e.g. a future release the server hasn't
    // learned yet), `record_agent_usage` under-reports rather
    // than crash. A stderr warning is emitted so operators notice.
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_agent_usage(id, "claude-future-v9", 1_000_000, 1_000_000, 0, 0)
        .await;
    let snap = store.snapshot(id).await.unwrap();
    // Priced at Sonnet 4.6 rate: 1M * $3 + 1M * $15 = $18
    assert!((snap.agent_cost_usd - 18.0).abs() < 1e-6);
    assert_eq!(snap.per_model_agent_cost_usd.get("sonnet_4_6"), Some(&18.0));
}

/// `record_iteration_run` accumulates total + duration.
#[tokio::test]
async fn record_iteration_run_accumulates() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_iteration_run(id, 100).await;
    store.record_iteration_run(id, 150).await;
    store.record_iteration_run(id, 200).await;
    let counters = store.inner.read().await.get(&id).cloned().unwrap();
    assert_eq!(counters.iterations_run_total, 3);
    assert_eq!(counters.iteration_total_duration_ms, 450);
}

/// Convergence histogram bucketing matches
/// ITERATIONS_HISTOGRAM_BUCKETS [1, 2, 4, 8, 16, 32, 64].
#[tokio::test]
async fn record_iteration_converged_buckets_correctly() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // 1 → bucket 0; 2 → bucket 1; 5 → bucket 2 (4..<8); 12 →
    // bucket 3 (8..<16); 50 → bucket 5 (32..<64); 100 → bucket 6 (64+).
    for n in [1, 2, 2, 5, 12, 50, 100] {
        store.record_iteration_converged(id, n).await;
    }
    let counters = store.inner.read().await.get(&id).cloned().unwrap();
    // [1×bucket0, 2×bucket1, 1×bucket2, 1×bucket3, 0×bucket4, 1×bucket5, 1×bucket6]
    assert_eq!(counters.converged_at_iter, vec![1, 2, 1, 1, 0, 1, 1]);
}

/// `record_iteration_max_hit` increments.
#[tokio::test]
async fn record_iteration_max_hit_counts() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_iteration_max_hit(id).await;
    store.record_iteration_max_hit(id).await;
    let counters = store.inner.read().await.get(&id).cloned().unwrap();
    assert_eq!(counters.max_iterations_hit_count, 2);
}

#[test]
fn new_sidecar_shape_round_trips() {
    // New sidecars written by the refactored code should also
    // deserialize via the same Deserialize impl — the new `per_model`
    // field has precedence over the legacy flat fields.
    let counters = {
        let mut c = SessionCounters {
            turn_count: 1,
            tool_call_count: 0,
            turn_durations_ms: vec![42],
            ..SessionCounters::default()
        };
        c.per_model.insert(
            ModelId::Haiku45,
            ModelCounters {
                count: 1,
                input_tokens: 100,
                output_tokens: 200,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
            },
        );
        c
    };
    let json = serde_json::to_string(&counters).unwrap();
    let parsed: SessionCounters = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.turn_count, 1);
    assert_eq!(
        parsed
            .per_model
            .get(&ModelId::Haiku45)
            .unwrap()
            .input_tokens,
        100
    );
}

/// Verify `write_cost_ledger_row` appends a parseable
/// JSONL row with the four cost buckets and their sum.
#[test]
fn write_cost_ledger_row_appends_jsonl() {
    let tmp = tempfile::tempdir().unwrap();
    let runtime = tmp.path().join("runtime");
    let id = Uuid::new_v4();
    let mut metrics = empty_session_metrics();
    metrics.chat_cost_usd = 0.5;
    metrics.agent_cost_usd = 1.25;
    metrics.scorer_cost_usd = 0.0;
    metrics.side_call_cost_usd = 0.01;
    // First write creates the file.
    write_cost_ledger_row(&runtime, id, &metrics).unwrap();
    // Second write appends a row (idempotent across re-emits).
    let mut metrics2 = SessionCounters::default().snapshot();
    metrics2.chat_cost_usd = 2.0;
    write_cost_ledger_row(&runtime, id, &metrics2).unwrap();

    let contents = std::fs::read_to_string(runtime.join("cost-ledger.jsonl")).unwrap();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected two appended rows");

    let row1: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row1["session_id"].as_str().unwrap(), id.to_string());
    assert!((row1["chat_cost_usd"].as_f64().unwrap() - 0.5).abs() < 1e-9);
    assert!((row1["agent_cost_usd"].as_f64().unwrap() - 1.25).abs() < 1e-9);
    assert!((row1["side_call_cost_usd"].as_f64().unwrap() - 0.01).abs() < 1e-9);
    // total_cost_usd == sum of the 4 buckets, not metrics.total_cost_usd
    // (which is the chat-side snapshot field).
    assert!((row1["total_cost_usd"].as_f64().unwrap() - 1.76).abs() < 1e-9);
    assert!(row1["emitted_at"].as_str().unwrap().contains('T'));

    let row2: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert!((row2["chat_cost_usd"].as_f64().unwrap() - 2.0).abs() < 1e-9);
}

// ---------------------------------------------------------------
// SME-experience counter round-trip tests.
// Scaffolding tests: verify the new fields serialize/deserialize
// correctly and the record_* methods update counters as expected.
// Full-lifecycle integration (service layer) is tested in
// service/tests.rs; these unit tests cover the MetricsStore layer.
// ---------------------------------------------------------------

/// record_intake_followup_turn increments followup_count per call.
#[tokio::test]
async fn followup_count_increments() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    assert_eq!(
        store.snapshot(id).await.map(|s| s.followup_count),
        None,
        "no snapshot before first record"
    );
    store.record_intake_followup_turn(id).await;
    store.record_intake_followup_turn(id).await;
    store.record_intake_followup_turn(id).await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.followup_count, 3);
}

/// record_amendment increments amendment_count per call.
#[tokio::test]
async fn amendment_count_increments() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store.record_amendment(id).await;
    store.record_amendment(id).await;
    let snap = store.snapshot(id).await.unwrap();
    assert_eq!(snap.amendment_count, 2);
}

/// record_blocker_entered pushes one entry; record_blocker_recovered
/// flips the last unrecovered entry.
#[tokio::test]
async fn blocker_events_record_and_recover() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    store
        .record_blocker_entered(id, "ToolError".to_string())
        .await;
    {
        let snap = store.snapshot(id).await.unwrap();
        assert_eq!(snap.blockers_encountered.len(), 1);
        assert_eq!(snap.blockers_encountered[0].blocker_kind, "ToolError");
        assert!(!snap.blockers_encountered[0].recovered);
    }
    store
        .record_blocker_recovered(id, Some("rerun_task".to_string()))
        .await;
    {
        let snap = store.snapshot(id).await.unwrap();
        assert!(snap.blockers_encountered[0].recovered);
        assert_eq!(
            snap.blockers_encountered[0].recovery_path.as_deref(),
            Some("rerun_task")
        );
    }
}

/// record_classification_confidence sets is_ambiguous correctly.
#[tokio::test]
async fn classification_confidence_sets_is_ambiguous() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();
    // No classification yet → is_ambiguous is None on first snapshot.
    store.record_intake_followup_turn(id).await; // seed session entry
    let snap0 = store.snapshot(id).await.unwrap();
    assert_eq!(snap0.is_ambiguous, None);

    // High-confidence call → Some(false).
    store.record_classification_confidence(id, 0.9).await;
    let snap1 = store.snapshot(id).await.unwrap();
    assert_eq!(snap1.is_ambiguous, Some(false));

    // Low-confidence call → sticky Some(true).
    store.record_classification_confidence(id, 0.4).await;
    let snap2 = store.snapshot(id).await.unwrap();
    assert_eq!(snap2.is_ambiguous, Some(true));

    // Another high-confidence call does NOT reset to false (sticky-true).
    store.record_classification_confidence(id, 0.95).await;
    let snap3 = store.snapshot(id).await.unwrap();
    assert_eq!(snap3.is_ambiguous, Some(true));
}

/// write_session_metrics_row serializes the four new fields and
/// produces a row that is round-trip-parseable.
#[test]
fn session_metrics_row_round_trip() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let runtime = tmp.path();
    let id = Uuid::new_v4();
    let mut metrics = empty_session_metrics();
    metrics.followup_count = 3;
    metrics.amendment_count = 1;
    metrics.blockers_encountered = vec![BlockerEventRecord {
        blocker_kind: "ToolError".to_string(),
        recovered: true,
        recovery_path: Some("rerun_task".to_string()),
    }];
    metrics.is_ambiguous = Some(true);
    write_session_metrics_row(runtime, id, 1_000_000, &metrics).unwrap();

    let path = runtime.join("session-metrics.jsonl");
    let line = std::fs::read_to_string(&path).unwrap();
    let row: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(row["session_id"].as_str().unwrap(), id.to_string());
    assert_eq!(row["followup_count"].as_u64().unwrap(), 3);
    assert_eq!(row["amendment_count"].as_u64().unwrap(), 1);
    assert_eq!(row["is_ambiguous"].as_bool(), Some(true));
    assert_eq!(row["schema_version"].as_u64().unwrap(), 1);
    let blockers = row["blockers_encountered"].as_array().unwrap();
    assert_eq!(blockers.len(), 1);
    assert_eq!(blockers[0]["blocker_kind"].as_str().unwrap(), "ToolError");
    assert_eq!(blockers[0]["recovered"].as_bool(), Some(true));
}

/// Two appended rows in session-metrics.jsonl (re-emit pattern).
#[test]
fn session_metrics_row_appends() {
    use tempfile::tempdir;
    let tmp = tempdir().unwrap();
    let runtime = tmp.path();
    let id = Uuid::new_v4();
    let metrics = empty_session_metrics();
    write_session_metrics_row(runtime, id, 0, &metrics).unwrap();
    write_session_metrics_row(runtime, id, 0, &metrics).unwrap();
    let contents = std::fs::read_to_string(runtime.join("session-metrics.jsonl")).unwrap();
    let lines: Vec<&str> = contents.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "expected two appended rows on re-emit");
}

/// Cost newtype wiring — verifies that `snapshot()` accumulates
/// per-bucket costs via `Cost::saturating_add` and that the resulting
/// `total_cost_usd` equals the sum of the four bucket f64 values.
#[tokio::test]
async fn snapshot_total_cost_equals_sum_of_buckets() {
    let store = MetricsStore::new();
    let id = Uuid::new_v4();

    // Record one chat turn (duration, tool_calls, input, output, cache_read,
    // cache_creation, model) and one side-call on the same session.
    store
        .record_turn(
            id,
            Duration::from_millis(200),
            /*tool_calls=*/ 0,
            /*input=*/ 1_000,
            /*output=*/ 500,
            /*cache_read=*/ 0,
            /*cache_creation=*/ 0,
            ModelId::Sonnet46,
        )
        .await;
    store
        .record_side_call_usage(
            id,
            ModelId::Haiku45,
            /*input=*/ 200,
            /*output=*/ 50,
            /*cache_read=*/ 0,
            /*cache_creation=*/ 0,
        )
        .await;

    let snap = store.snapshot(id).await.expect("snapshot present");
    // The four buckets must sum to total exactly (within floating-point
    // rounding — both sides go through the same `as_usd()` conversion).
    let expected =
        snap.chat_cost_usd + snap.agent_cost_usd + snap.scorer_cost_usd + snap.side_call_cost_usd;
    assert!(
        (snap.total_cost_usd - expected).abs() < 1e-9,
        "total_cost_usd ({}) != sum of buckets ({})",
        snap.total_cost_usd,
        expected
    );
    // At least one bucket must be non-zero (chat had 1000 Sonnet tokens).
    assert!(
        snap.chat_cost_usd > 0.0,
        "chat_cost_usd must be positive after a Sonnet turn"
    );
    // Side-call bucket must be non-zero (Haiku 200 input tokens).
    assert!(
        snap.side_call_cost_usd > 0.0,
        "side_call_cost_usd must be positive after a Haiku side-call"
    );
}
