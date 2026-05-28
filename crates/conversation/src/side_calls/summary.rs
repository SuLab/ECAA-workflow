//! Narrative dashboard summary side-call.
//!
//! Given the completed `final_reporting` task's narrative + key
//! figures + decision log, produce a 3-paragraph executive summary.
//! Haiku 4.5, billed to side_call_cost_usd. Cached by
//! (session_id → summary + source_fingerprint); flushed when the
//! Fingerprint changes (final_reporting reran or amendment).

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use anyhow::{anyhow, Context, Result};
use moka::sync::Cache;
use once_cell::sync::Lazy;
use std::sync::Arc;
use std::time::Duration;

const SUMMARY_PROMPT: &str = "You are writing a 3-paragraph executive summary of a \
     completed single-cell / bulk / variant / clinical bioinformatics \
     analysis for a working bioinformatician. Paragraph 1: cohort \
     composition. Paragraph 2: top findings (cite concrete numbers). \
     Paragraph 3: caveats and recommendations. Plain English — no \
     jargon like `per-cell Wilcoxon`, `BH-adjusted`. Reply with ONLY \
     the three paragraphs.";

const SUMMARY_MAX_OUTPUT_TOKENS: u32 = 900;
const SUMMARY_TEMPERATURE: f32 = 0.2;

/// R-39 — moka TTL cache: SessionId → (summary, model, fingerprint).
/// The size cap + TTL are enforced atomically by moka; entries past
/// the 1-hour TTL are evicted on read with eager background sweeps.
/// The fingerprint is checked at read time so a stale entry from a
/// pre-amendment run never overwrites a post-amendment summary.
const CACHE_MAX_ENTRIES: u64 = 256;
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
struct CacheEntry {
    summary: String,
    model: String,
    fingerprint: String,
}

static CACHE: Lazy<Cache<SessionId, CacheEntry>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(CACHE_MAX_ENTRIES)
        .time_to_live(CACHE_TTL)
        .build()
});

/// Clear the summary cache. For test use only — concurrent integration
/// tests share the same module-scope cache and would otherwise see
/// each other's cached summaries. Production code never calls this.
#[cfg(test)]
pub fn _clear_cache_for_test() {
    CACHE.invalidate_all();
    CACHE.run_pending_tasks();
}

fn read_cache(id: SessionId, fingerprint: &str) -> Option<(String, String)> {
    let e = CACHE.get(&id)?;
    if e.fingerprint == fingerprint {
        Some((e.summary, e.model))
    } else {
        None
    }
}

fn write_cache(id: SessionId, summary: String, model: String, fingerprint: String) {
    CACHE.insert(
        id,
        CacheEntry {
            summary,
            model,
            fingerprint,
        },
    );
}

/// Result of a `summarize` side-call.
pub struct SummaryResult {
    /// Generated executive summary text.
    pub summary: String,
    /// Anthropic model id used (e.g. `"claude-haiku-4-5-20251001"`).
    pub model: String,
    /// `true` when the result was served from the in-process cache.
    pub cached: bool,
}

/// Generate a 3-paragraph executive summary of the session's results.
///
/// `source` is the text the LLM should summarise (narrative artifact +
/// key tables + decision highlights — the caller assembles this so the
/// side-call module stays infrastructure-free). `fingerprint` is a hash
/// of the source so the cache can detect staleness.
pub async fn generate_dashboard_summary(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    source: &str,
    fingerprint: &str,
) -> Result<SummaryResult> {
    if source.trim().is_empty() {
        return Err(anyhow!("summary: empty source"));
    }
    if let Some((s, m)) = read_cache(session_id, fingerprint) {
        return Ok(SummaryResult {
            summary: s,
            model: m,
            cached: true,
        });
    }
    let user_prompt = format!("SOURCE MATERIAL:\n\n{}\n\nWrite the summary now.", source);
    let model = ModelPolicy::for_side_call();
    let req = TurnRequest {
        system_prompt: vec![SystemPromptBlock {
            text: SUMMARY_PROMPT.to_string(),
            cache: false,
        }],
        conversation: Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: SUMMARY_TEMPERATURE,
        max_tokens: SUMMARY_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };
    let resp = backend
        .send_turn(req)
        .await
        .context("summary side-call failed")?;
    if resp.stop_reason != StopReason::EndTurn && resp.stop_reason != StopReason::MaxTokens {
        return Err(anyhow!(
            "summary expected end_turn, got {:?}",
            resp.stop_reason
        ));
    }
    metrics
        .record_side_call_usage(
            session_id,
            model,
            resp.usage.input_tokens as u64,
            resp.usage.output_tokens as u64,
            resp.usage.cache_read_input_tokens as u64,
            resp.usage.cache_creation_input_tokens as u64,
        )
        .await;
    let summary = resp.assistant_content.trim().to_string();
    if summary.is_empty() {
        return Err(anyhow!("summary produced empty output"));
    }
    let model_str = crate::model_policy::model_serde_name(model);
    write_cache(
        session_id,
        summary.clone(),
        model_str.clone(),
        fingerprint.to_string(),
    );
    Ok(SummaryResult {
        summary,
        model: model_str,
        cached: false,
    })
}

/// Test-only cache flush.
#[cfg(test)]
pub fn __reset_cache_for_tests() {
    CACHE.invalidate_all();
    CACHE.run_pending_tasks();
}
