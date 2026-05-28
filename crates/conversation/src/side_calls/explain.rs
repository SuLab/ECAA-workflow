//! "Explain this for me" side-call.
//!
//! Plain-language rewrite of a technical snippet (blocker reason,
//! narrative fragment, stage description). Routed through
//! `ModelPolicy::for_side_call()` (Haiku 4.5) so it bills to the cheap
//! side-call bucket instead of the main Sonnet/Opus conversation cache.
//!
//! An in-memory moka cache (R-39) keys `(text_hash → explanation)` for
//! 60 minutes so a blocker reason the SME clicks Explain on twice
//! doesn't re-bill. The cache enforces both a size cap (256 entries)
//! and a 1-hour TTL atomically — entries past the TTL are evicted on
//! read with eager background sweeps.

use crate::anthropic::{LlmBackend, StopReason, TurnRequest};
use crate::metrics::MetricsStore;
use crate::model_policy::ModelPolicy;
use crate::prompt::SystemPromptBlock;
use crate::session::{SessionId, Turn};
use anyhow::{anyhow, Context, Result};
use moka::sync::Cache;
use once_cell::sync::Lazy;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

const EXPLAIN_PROMPT: &str = "You are helping a working bioinformatician understand a technical \
     snippet from an analysis pipeline. Rewrite the given text so it \
     reads naturally to someone without a deep computational-statistics \
     background. Keep every concrete number (cell counts, p-values, \
     fold changes). Avoid jargon like `per-cell Wilcoxon`, \
     `Benjamini-Hochberg`, `scVI`. Prefer short sentences. 3-5 \
     sentences max. Reply with ONLY the rewrite — no preamble, no \
     quotes around it.";

const EXPLAIN_MAX_OUTPUT_TOKENS: u32 = 400;
const EXPLAIN_TEMPERATURE: f32 = 0.1;
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);

/// R-39 — moka TTL cache: (text, context) hash → (explanation, model).
/// Held at module scope so every backend client shares the cache
/// across sessions. The size cap + TTL are enforced atomically by
/// moka; entries past the 1-hour TTL are evicted on read, and a
/// background expiration sweep collects them eagerly.
const CACHE_MAX_ENTRIES: u64 = 256;

type CacheKey = u64;

#[derive(Clone)]
struct CacheEntry {
    explanation: String,
    model: String,
}

static CACHE: Lazy<Cache<CacheKey, CacheEntry>> = Lazy::new(|| {
    Cache::builder()
        .max_capacity(CACHE_MAX_ENTRIES)
        .time_to_live(CACHE_TTL)
        .build()
});

/// Clear the explain cache. For test use only — concurrent integration
/// tests share the same module-scope cache and would otherwise see
/// each other's cached explanations. Production code never calls this.
#[cfg(test)]
pub fn _clear_cache_for_test() {
    CACHE.invalidate_all();
    CACHE.run_pending_tasks();
}

fn cache_key(text: &str, context: &str) -> CacheKey {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    context.hash(&mut hasher);
    hasher.finish()
}

fn read_cache(key: CacheKey) -> Option<(String, String)> {
    let entry = CACHE.get(&key)?;
    Some((entry.explanation, entry.model))
}

fn write_cache(key: CacheKey, explanation: String, model: String) {
    CACHE.insert(key, CacheEntry { explanation, model });
}

/// Result of an `explain` side-call.
pub struct ExplainResult {
    /// Plain-language rewrite of the technical text.
    pub explanation: String,
    /// Anthropic model id used (e.g. `"claude-haiku-4-5-20251001"`).
    pub model: String,
    /// `true` when the result was served from the in-process cache.
    pub cached: bool,
}

/// Ask Haiku 4.5 for a plain-language rewrite. `context` is optional
/// contextual register hint ("blocker reason", "narrative", "method
/// description") so the model can pick the right tone. Returns the
/// explanation + the Anthropic model id used + a `cached` flag.
pub async fn explain(
    backend: Arc<dyn LlmBackend>,
    metrics: &MetricsStore,
    session_id: SessionId,
    text: &str,
    context: Option<&str>,
) -> Result<ExplainResult> {
    let text = text.trim();
    if text.is_empty() {
        return Err(anyhow!("explain: empty text"));
    }
    let ctx_owned = context.unwrap_or("").to_string();
    let key = cache_key(text, &ctx_owned);
    if let Some((explanation, model)) = read_cache(key) {
        return Ok(ExplainResult {
            explanation,
            model,
            cached: true,
        });
    }

    let user_prompt = match context {
        Some(c) if !c.is_empty() => format!(
            "CONTEXT: {}\n\nTEXT TO REWRITE:\n{}\n\nRewrite now.",
            c, text
        ),
        _ => format!("TEXT TO REWRITE:\n{}\n\nRewrite now.", text),
    };
    let model = ModelPolicy::for_side_call();
    let req = TurnRequest {
        system_prompt: vec![SystemPromptBlock {
            text: EXPLAIN_PROMPT.to_string(),
            cache: false,
        }],
        conversation: Arc::new(vec![Turn::user(user_prompt)]),
        tool_schemas: vec![],
        model,
        temperature: EXPLAIN_TEMPERATURE,
        max_tokens: EXPLAIN_MAX_OUTPUT_TOKENS,
        tool_exchange: vec![],
        tool_choice: None,
    };
    let resp = backend
        .send_turn(req)
        .await
        .context("explain side-call failed")?;
    if resp.stop_reason != StopReason::EndTurn && resp.stop_reason != StopReason::MaxTokens {
        return Err(anyhow!(
            "explain expected end_turn stop reason, got {:?}",
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

    let explanation = resp.assistant_content.trim().to_string();
    if explanation.is_empty() {
        return Err(anyhow!("explain produced empty output"));
    }
    let model_str = crate::model_policy::model_serde_name(model);
    write_cache(key, explanation.clone(), model_str.clone());
    Ok(ExplainResult {
        explanation,
        model: model_str,
        cached: false,
    })
}

/// Test-only: flush the cache.
#[cfg(test)]
pub fn __reset_cache_for_tests() {
    CACHE.invalidate_all();
    CACHE.run_pending_tasks();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anthropic::{TurnResponse, Usage};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingBackend {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl LlmBackend for CountingBackend {
        async fn send_turn(&self, _req: TurnRequest) -> Result<TurnResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(TurnResponse {
                assistant_content: "A plain-language version of the input.".to_string(),
                tool_uses: Vec::new(),
                stop_reason: StopReason::EndTurn,
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 10,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },

                request_metadata: Default::default(),
            })
        }
        async fn send_turn_streaming(
            &self,
            req: TurnRequest,
            _on_delta: crate::anthropic::delta_sink::DeltaSink,
        ) -> Result<TurnResponse> {
            self.send_turn(req).await
        }
    }

    #[tokio::test]
    async fn caches_repeat_calls() {
        __reset_cache_for_tests();
        let backend = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
        });
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let a = explain(backend.clone(), &metrics, id, "some technical text", None)
            .await
            .unwrap();
        assert!(!a.cached);
        let b = explain(backend.clone(), &metrics, id, "some technical text", None)
            .await
            .unwrap();
        assert!(b.cached);
        assert_eq!(backend.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn distinct_context_misses_cache() {
        __reset_cache_for_tests();
        let backend = Arc::new(CountingBackend {
            calls: AtomicUsize::new(0),
        });
        let metrics = MetricsStore::new();
        let id = uuid::Uuid::new_v4();
        let _ = explain(backend.clone(), &metrics, id, "text", Some("blocker"))
            .await
            .unwrap();
        let _ = explain(backend.clone(), &metrics, id, "text", Some("narrative"))
            .await
            .unwrap();
        assert_eq!(backend.calls.load(Ordering::SeqCst), 2);
    }
}
