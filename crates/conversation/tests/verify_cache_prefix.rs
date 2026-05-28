//! Empirically measure the cacheable system-prompt + tools prefix in
//! tokens via Anthropic's `/v1/messages/count_tokens` endpoint. Gated
//! behind `SWFC_LIVE_API=1` + `ANTHROPIC_API_KEY` and `#[ignore]`-
//! annotated, so `cargo test` never runs it automatically. Invoke via
//! `make verify-cache-prefix`.
//!
//! Background: Opus 4.7 requires a minimum cacheable prefix of 4096
//! tokens for the cache to engage at all; Sonnet 4.6's minimum is 2048.
//! Below those thresholds `cache_creation_input_tokens` silently stays
//! at zero on every request — no error, just no caching. This test
//! measures the real prefix we ship (via `build_system_prompt` +
//! `tool_schemas_for_state`, on a bio session with a loaded taxonomy,
//! matching the worst-case shape after the taxonomy-caching change) and
//! asserts it clears the Opus 4.7 threshold. Regressions (e.g. slimming
//! the prompt below 4096 tokens) fail loudly the next time an operator
//! runs this target.

use scripps_workflow_conversation::prompt::build_system_prompt;
use scripps_workflow_conversation::session::Session;
use scripps_workflow_conversation::tool_schemas::tool_schemas_for_state;
use scripps_workflow_core::taxonomy::StageTaxonomy;

const OPUS_47_MIN_TOKENS: u32 = 4096;
const SONNET_46_MIN_TOKENS: u32 = 2048;

#[tokio::test]
#[ignore]
async fn measure_cacheable_prefix_tokens() {
    assert_eq!(
        std::env::var("SWFC_LIVE_API").ok().as_deref(),
        Some("1"),
        "SWFC_LIVE_API=1 required (this test hits the live Anthropic count_tokens endpoint)"
    );
    let api_key = scripps_workflow_conversation::anthropic_api_key()
        .expect("SWFC_ANTHROPIC_API_KEY required (count_tokens is not billed but still needs auth). Legacy ANTHROPIC_API_KEY also accepted.");

    // Worst-case prefix shape: bio session with a loaded taxonomy.
    // Phase B4 — synthesize the taxonomy metadata rather than loading
    // from `config/stage-taxonomies/single-cell.yaml` (deleted). The
    // critical prefix invariant under measurement is the system-prompt
    // + tools layout, which is unaffected by whether the taxonomy
    // metadata came from disk or from a struct literal.
    let tax = StageTaxonomy {
        id: "single_cell".into(),
        domain: "computational biology".into(),
        description: "single-cell RNA-seq composition (synthesized post-B4)".into(),
        ..Default::default()
    };

    let mut session = Session::new(false);
    session.taxonomy = Some(tax);

    let system_blocks = build_system_prompt(&session);
    let tool_schemas = tool_schemas_for_state(&session.state);

    // count_tokens requires a user message; we strip the ~3-token
    // overhead from the reported total to isolate the prefix. The
    // endpoint is not billed.
    let payload = serde_json::json!({
        "model": "claude-opus-4-7",
        "system": system_blocks.iter().map(|b| serde_json::json!({
            "type": "text",
            "text": b.text,
        })).collect::<Vec<_>>(),
        "tools": tool_schemas,
        "messages": [{"role": "user", "content": "_"}],
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("https://api.anthropic.com/v1/messages/count_tokens")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&payload)
        .send()
        .await
        .expect("count_tokens request failed");

    let status = resp.status();
    let body = resp.text().await.expect("read body");
    assert!(
        status.is_success(),
        "count_tokens returned HTTP {}: {}",
        status,
        body
    );

    let parsed: serde_json::Value = serde_json::from_str(&body).expect("parse json response");
    let total_tokens = parsed["input_tokens"]
        .as_u64()
        .unwrap_or_else(|| panic!("no input_tokens in response: {}", body))
        as u32;

    const DUMMY_USER_OVERHEAD: u32 = 3;
    let prefix_tokens = total_tokens.saturating_sub(DUMMY_USER_OVERHEAD);
    let total_prefix_bytes: usize = system_blocks.iter().map(|b| b.text.len()).sum();

    let opus_ok = prefix_tokens >= OPUS_47_MIN_TOKENS;
    let sonnet_ok = prefix_tokens >= SONNET_46_MIN_TOKENS;

    println!();
    println!("── Cacheable prefix measurement ─────────────────────");
    println!("  system blocks          : {}", system_blocks.len());
    println!(
        "    cached blocks        : {}",
        system_blocks.iter().filter(|b| b.cache).count()
    );
    println!("  system text bytes      : {}", total_prefix_bytes);
    println!("  tool schemas           : {}", tool_schemas.len());
    println!(
        "  raw input_tokens       : {} (model=claude-opus-4-7)",
        total_tokens
    );
    println!("  prefix tokens (est)    : {}", prefix_tokens);
    println!(
        "  Opus 4.7 min (4096)    : {}",
        if opus_ok {
            "OK"
        } else {
            "BELOW — cache silently inactive on Opus 4.7 escalation"
        }
    );
    println!(
        "  Sonnet 4.6 min (2048)  : {}",
        if sonnet_ok {
            "OK"
        } else {
            "BELOW — cache silently inactive on Sonnet 4.6 (default model)"
        }
    );
    println!();

    assert!(
        opus_ok,
        "cacheable prefix is {} tokens, below Opus 4.7's {}-token minimum. \
         Opus escalation (CarefulMode / Blocked / LowConfidence) will silently \
         pay full input rate with no caching. See \
         and the \
         the taxonomy cache marker.",
        prefix_tokens, OPUS_47_MIN_TOKENS
    );
    assert!(
        sonnet_ok,
        "cacheable prefix is {} tokens, below Sonnet 4.6's {}-token minimum. \
         The default-model path is silently paying full input rate. This is \
         worse than the Opus 4.7 case — Sonnet is our hot path.",
        prefix_tokens, SONNET_46_MIN_TOKENS
    );
}
