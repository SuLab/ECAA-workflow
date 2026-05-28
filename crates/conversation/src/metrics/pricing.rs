//! Anthropic list pricing in USD per million tokens, using the 5-minute
//! ephemeral cache tier (`cache_control: {"type": "ephemeral"}` with the
//! default TTL, which is what `anthropic/client.rs` attaches to the
//! cacheable system-prompt blocks). If we ever switch to the 1-hour TTL
//! we'll need a second column here — the write multiplier jumps from
//! 1.25× to 2× base input.

use crate::model_policy::ModelId;

/// Anthropic list prices (USD per million tokens) for one model.
pub struct ModelPrices {
    /// Input token price (USD per million tokens).
    pub input_per_mtok: f64,
    /// Output token price (USD per million tokens).
    pub output_per_mtok: f64,
    /// Prompt-cache write price (USD per million tokens).
    pub cache_write_per_mtok: f64,
    /// Prompt-cache read price (USD per million tokens).
    pub cache_read_per_mtok: f64,
}

/// Claude Sonnet 4.6 list pricing.
pub const SONNET_4_6: ModelPrices = ModelPrices {
    input_per_mtok: 3.00,
    output_per_mtok: 15.00,
    cache_write_per_mtok: 3.75,
    cache_read_per_mtok: 0.30,
};

/// Opus 4.6 list pricing. Retained so legacy sidecars written while
/// Opus 4.6 was the escalation target continue to price accurately
/// on rehydration. Rates are identical to Opus 4.7. Verified against
/// `docs.anthropic.com/en/docs/about-claude/pricing` and the
/// prompt-caching table.
pub const OPUS_4_6: ModelPrices = ModelPrices {
    input_per_mtok: 5.00,
    output_per_mtok: 25.00,
    cache_write_per_mtok: 6.25,
    cache_read_per_mtok: 0.50,
};

/// Opus 4.7 list pricing. Current Opus escalation target. Same
/// rate card as 4.6; the upgrade is capability-only. Uses a newer
/// tokenizer than 4.6 — the per-MTok rate is unchanged but
/// effective cost per request may shift slightly because the same
/// text tokenizes to a different count. Verified against
/// `docs.claude.com/en/docs/about-claude/models/overview`.
pub const OPUS_4_7: ModelPrices = ModelPrices {
    input_per_mtok: 5.00,
    output_per_mtok: 25.00,
    cache_write_per_mtok: 6.25,
    cache_read_per_mtok: 0.50,
};

/// Claude Haiku 4.5 list pricing. Cache write / read multipliers
/// follow the standard 1.25× / 0.1× ephemeral-tier ratios.
pub const HAIKU_4_5: ModelPrices = ModelPrices {
    input_per_mtok: 1.00,
    output_per_mtok: 5.00,
    cache_write_per_mtok: 1.25,
    cache_read_per_mtok: 0.10,
};

/// Dispatch the per-model pricing table. Every `ModelId` variant must
/// land here — `model_policy::tests::all_variants_exhaustive` and
/// `metrics::tests::every_model_id_has_pricing` together guard against
/// silent misrouting when a new model is added.
pub fn prices_for(model: ModelId) -> &'static ModelPrices {
    match model {
        ModelId::Sonnet46 => &SONNET_4_6,
        ModelId::Opus46 => &OPUS_4_6,
        ModelId::Opus47 => &OPUS_4_7,
        ModelId::Haiku45 => &HAIKU_4_5,
    }
}

/// Compute USD cost from token counts and a `ModelPrices` table.
pub fn cost_usd(
    prices: &ModelPrices,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_creation: u64,
) -> f64 {
    let per_tok = |n: u64, rate: f64| (n as f64) * rate / 1_000_000.0;
    per_tok(input, prices.input_per_mtok)
        + per_tok(output, prices.output_per_mtok)
        + per_tok(cache_read, prices.cache_read_per_mtok)
        + per_tok(cache_creation, prices.cache_write_per_mtok)
}
