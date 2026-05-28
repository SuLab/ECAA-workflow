//! Cross-cutting tests for `Config::from_env_map`.
//!
//! The inline parser-level tests live in `crates/core/src/config.rs`;
//! this file exercises the public surface — defaults, scheme
//! validation, NaN/INF rejection, bounds enforcement, secret redaction
//! in `Debug`.

use std::collections::HashMap;

use ecaa_workflow_core::config::{ChatMode, Config, LitSourceScope, ModalityDriftMode};

fn empty() -> HashMap<&'static str, &'static str> {
    HashMap::new()
}

#[test]
fn defaults_when_env_empty() {
    let env = empty();
    let cfg = Config::from_env_map(&env).expect("defaults valid");
    assert_eq!(cfg.chat_mode, ChatMode::Online);
    assert_eq!(
        cfg.anthropic_base_url.as_str(),
        "https://api.anthropic.com/"
    );
    assert_eq!(cfg.aws_pricing_region_mult, 1.0);
    assert_eq!(cfg.harness_batch_window_secs, 10);
    assert_eq!(cfg.task_heartbeat_stall_secs, 300);
    assert_eq!(cfg.literature.source_scope, LitSourceScope::PmcOa);
    assert_eq!(cfg.literature.evidence_max_mb, 200);
    assert_eq!(cfg.upload_disk_reserve_gb, 50);
    assert_eq!(cfg.bind_addr, "127.0.0.1");
    assert_eq!(cfg.port, 3000);
    assert!(cfg.anthropic_api_key.is_none());
    assert!(cfg.server_auth_token.is_none());
    assert!(cfg.literature.ncbi_api_key.is_none());
    assert!(cfg.aws_pricing_overrides.is_empty());
    assert!(cfg.input_roots.is_empty());
    assert!(
        cfg.git_enabled,
        "git enabled by default unless ECAA_GIT_ENABLED=0"
    );
    assert_eq!(cfg.composer, "semantic");
}

#[test]
fn rejects_anthropic_base_url_without_https_for_non_loopback() {
    let mut env = empty();
    env.insert("ANTHROPIC_BASE_URL", "http://api.anthropic.com");
    let err = Config::from_env_map(&env)
        .expect_err("non-loopback http should be rejected")
        .to_string();
    assert!(err.contains("https://"), "got: {err}");
}

#[test]
fn accepts_loopback_http_base_url() {
    for h in [
        "http://localhost:8080",
        "http://127.0.0.1:3000",
        "http://[::1]:8080",
    ] {
        let mut env = empty();
        env.insert("ANTHROPIC_BASE_URL", h);
        assert!(
            Config::from_env_map(&env).is_ok(),
            "should accept loopback http: {h}"
        );
    }
}

#[test]
fn rejects_cost_ceiling_nan_or_inf() {
    for bad in ["inf", "-inf", "Infinity", "NaN", "nan", "+inf"] {
        let mut env = empty();
        env.insert("ECAA_AWS_COST_CEILING_USD", bad);
        let result = Config::from_env_map(&env);
        assert!(
            result.is_err(),
            "should reject ECAA_AWS_COST_CEILING_USD={bad}"
        );
    }
}

#[test]
fn rejects_cost_ceiling_negative() {
    let mut env = empty();
    env.insert("ECAA_AWS_COST_CEILING_USD", "-0.01");
    let err = Config::from_env_map(&env)
        .expect_err("negative cost ceiling rejected")
        .to_string();
    assert!(
        err.to_lowercase().contains("non-negative") || err.contains("-0.01"),
        "got: {err}"
    );
}

#[test]
fn accepts_finite_cost_ceiling() {
    let mut env = empty();
    env.insert("ECAA_AWS_COST_CEILING_USD", "250.50");
    let cfg = Config::from_env_map(&env).expect("finite cost ceiling valid");
    assert_eq!(cfg.aws_cost_ceiling_usd, Some(250.50));
}

#[test]
fn rejects_region_mult_out_of_range() {
    for bad in ["0.1", "10", "-1", "0.4", "5.01"] {
        let mut env = empty();
        env.insert("ECAA_AWS_PRICING_REGION_MULT", bad);
        assert!(
            Config::from_env_map(&env).is_err(),
            "should reject ECAA_AWS_PRICING_REGION_MULT={bad}"
        );
    }
}

#[test]
fn accepts_region_mult_in_range() {
    for ok in ["0.5", "1.0", "1.10", "2.5", "5.0"] {
        let mut env = empty();
        env.insert("ECAA_AWS_PRICING_REGION_MULT", ok);
        let cfg = Config::from_env_map(&env).unwrap_or_else(|e| panic!("should accept {ok}: {e}"));
        assert_eq!(cfg.aws_pricing_region_mult, ok.parse::<f64>().unwrap());
    }
}

#[test]
fn rejects_region_mult_nan() {
    let mut env = empty();
    env.insert("ECAA_AWS_PRICING_REGION_MULT", "NaN");
    assert!(Config::from_env_map(&env).is_err());
}

#[test]
fn parses_lit_source_scope_enum() {
    for (raw, expected) in [
        ("pmc_oa", LitSourceScope::PmcOa),
        ("pmc_oa_plus_abstracts", LitSourceScope::PmcOaPlusAbstracts),
        (
            "all_sources_local_only",
            LitSourceScope::AllSourcesLocalOnly,
        ),
    ] {
        let mut env = empty();
        env.insert("ECAA_LIT_SOURCE_SCOPE", raw);
        let cfg = Config::from_env_map(&env).unwrap();
        assert_eq!(cfg.literature.source_scope, expected);
    }
}

#[test]
fn lit_source_scope_invalid_falls_back_to_default() {
    let mut env = empty();
    env.insert("ECAA_LIT_SOURCE_SCOPE", "made_up_tier");
    let cfg =
        Config::from_env_map(&env).expect("invalid lit scope warns + falls back, not fail-stop");
    assert_eq!(cfg.literature.source_scope, LitSourceScope::PmcOa);
}

#[test]
fn parses_chat_mode_offline() {
    let mut env = empty();
    env.insert("ECAA_CHAT_MODE", "offline");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.chat_mode, ChatMode::Offline);
}

#[test]
fn chat_mode_defaults_to_online_for_other_values() {
    for raw in ["online", "live", "", "no"] {
        let mut env = empty();
        env.insert("ECAA_CHAT_MODE", raw);
        let cfg = Config::from_env_map(&env).unwrap();
        assert_eq!(cfg.chat_mode, ChatMode::Online, "raw: {raw:?}");
    }
}

#[test]
fn rejects_harness_batch_window_above_max() {
    let mut env = empty();
    env.insert("ECAA_HARNESS_BATCH_WINDOW_SECS", "9999");
    let err = Config::from_env_map(&env)
        .expect_err("9999 > 600 max")
        .to_string();
    assert!(
        err.contains("max") || err.contains("600"),
        "expected max/600 in error, got: {err}"
    );
}

#[test]
fn harness_batch_window_accepts_zero_and_max() {
    for v in ["0", "600", "10"] {
        let mut env = empty();
        env.insert("ECAA_HARNESS_BATCH_WINDOW_SECS", v);
        let cfg = Config::from_env_map(&env).unwrap_or_else(|e| panic!("should accept {v}: {e}"));
        assert_eq!(cfg.harness_batch_window_secs, v.parse::<u64>().unwrap());
    }
}

#[test]
fn harness_batch_window_invalid_falls_back_to_default() {
    // Docs say "out-of-range / 0 / non-numeric values fall back to
    // default with a tracing warning"; "out-of-range" we intercept
    // *above* the cap, but non-numeric is warn-fall-back.
    let mut env = empty();
    env.insert("ECAA_HARNESS_BATCH_WINDOW_SECS", "not-a-number");
    let cfg = Config::from_env_map(&env).expect("non-numeric falls back to default");
    assert_eq!(cfg.harness_batch_window_secs, 10);
}

#[test]
fn rejects_port_out_of_u16_range() {
    let mut env = empty();
    env.insert("ECAA_PORT", "70000");
    assert!(Config::from_env_map(&env).is_err());
}

#[test]
fn git_enabled_default_true() {
    let env = empty();
    let cfg = Config::from_env_map(&env).unwrap();
    assert!(cfg.git_enabled);
}

#[test]
fn git_enabled_only_zero_disables() {
    let mut env = empty();
    env.insert("ECAA_GIT_ENABLED", "0");
    let cfg = Config::from_env_map(&env).unwrap();
    assert!(!cfg.git_enabled, "ECAA_GIT_ENABLED=0 must disable");

    // Per the docs: "Any other value (or absent) = config-driven".
    // From the Config struct's point of view that means "enabled".
    for other in ["1", "true", "no", "off", "yes"] {
        let mut env = empty();
        env.insert("ECAA_GIT_ENABLED", other);
        let cfg = Config::from_env_map(&env).unwrap();
        assert!(
            cfg.git_enabled,
            "only `0` disables git per docs; {other:?} should leave it enabled"
        );
    }
}

#[test]
fn input_roots_split_on_colon_and_comma() {
    let mut env = empty();
    env.insert("ECAA_INPUT_ROOTS", "/srv/a:/srv/b,/srv/c");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(
        cfg.input_roots,
        vec![
            "/srv/a".to_string(),
            "/srv/b".to_string(),
            "/srv/c".to_string()
        ]
    );
}

#[test]
fn input_roots_skips_empty_segments() {
    let mut env = empty();
    env.insert("ECAA_INPUT_ROOTS", ":/srv/a::");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.input_roots, vec!["/srv/a".to_string()]);
}

#[test]
fn pricing_overrides_inline_json() {
    let mut env = empty();
    env.insert(
        "ECAA_AWS_PRICING_OVERRIDES_JSON",
        r#"{"m6i.large": 0.096, "r6i.xlarge": 0.252}"#,
    );
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.aws_pricing_overrides.len(), 2);
    assert_eq!(cfg.aws_pricing_overrides.get("m6i.large"), Some(&0.096));
    assert_eq!(cfg.aws_pricing_overrides.get("r6i.xlarge"), Some(&0.252));
}

#[test]
fn pricing_overrides_rejects_non_positive() {
    let mut env = empty();
    env.insert("ECAA_AWS_PRICING_OVERRIDES_JSON", r#"{"m6i.large": -0.05}"#);
    assert!(Config::from_env_map(&env).is_err());
}

#[test]
fn legacy_anthropic_api_key_fallback() {
    let mut env = empty();
    env.insert("ANTHROPIC_API_KEY", "legacy-XXXX");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.anthropic_api_key, Some("legacy-XXXX".to_string()));
}

#[test]
fn swfc_anthropic_api_key_takes_precedence_over_legacy() {
    let mut env = empty();
    env.insert("ECAA_ANTHROPIC_API_KEY", "primary");
    env.insert("ANTHROPIC_API_KEY", "legacy");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.anthropic_api_key, Some("primary".to_string()));
}

#[test]
fn debug_redacts_api_keys() {
    let cfg = Config::for_test()
        .anthropic_api_key("sk-ant-secret-XXXX")
        .server_auth_token("bearer-secret-YYYY")
        .build();
    let dbg = format!("{cfg:?}");
    assert!(
        !dbg.contains("sk-ant-secret"),
        "anthropic_api_key leaked: {dbg}"
    );
    assert!(
        !dbg.contains("bearer-secret"),
        "server_auth_token leaked: {dbg}"
    );
    assert!(
        dbg.contains("<redacted>"),
        "expected `<redacted>` marker in: {dbg}"
    );
}

#[test]
fn debug_redacts_ncbi_api_key() {
    let mut env = empty();
    env.insert("ECAA_LIT_NCBI_API_KEY", "ncbi-secret-ZZZZ");
    let cfg = Config::from_env_map(&env).unwrap();
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("ncbi-secret"), "ncbi_api_key leaked: {dbg}");
    assert!(dbg.contains("<redacted>"));
}

#[test]
fn config_for_test_builder_produces_runnable_config() {
    // Confirm the builder doesn't panic for the empty default case.
    let cfg = Config::for_test().build();
    assert_eq!(cfg.chat_mode, ChatMode::Online);
    assert!(cfg.anthropic_api_key.is_none());
    assert_eq!(cfg.harness_batch_window_secs, 10);
}

#[test]
fn builder_overrides_propagate() {
    let cfg = Config::for_test()
        .harness_batch_window_secs(5)
        .auto_title(true)
        .composer("proof-carrying")
        .port(8080)
        .build();
    assert_eq!(cfg.harness_batch_window_secs, 5);
    assert!(cfg.auto_title);
    assert_eq!(cfg.composer, "proof-carrying");
    assert_eq!(cfg.port, 8080);
}

#[test]
fn empty_optional_strings_collapse_to_none() {
    // Defense against operator typos where an env var is set to ""
    // (e.g. `ECAA_SERVER_AUTH_TOKEN=` in a stale .env). The loader
    // must treat that as unset, not as "auth token = empty string".
    let mut env = empty();
    env.insert("ECAA_SERVER_AUTH_TOKEN", "");
    env.insert("ECAA_LIT_NCBI_API_KEY", "");
    env.insert("ECAA_UPLOAD_ROOT", "");
    let cfg = Config::from_env_map(&env).unwrap();
    assert!(cfg.server_auth_token.is_none());
    assert!(cfg.literature.ncbi_api_key.is_none());
    assert!(cfg.upload_root.is_none());
}

// C22 / R-7: drift-mode env knob snapshot.
#[test]
fn modality_drift_mode_defaults_to_warn() {
    let cfg = Config::from_env_map(&empty()).unwrap();
    assert_eq!(cfg.modality_drift_mode, ModalityDriftMode::Warn);
}

#[test]
fn modality_drift_mode_fail_parses_case_insensitively() {
    for raw in ["fail", "Fail", "FAIL"] {
        let mut env = empty();
        env.insert("ECAA_MODALITY_DRIFT_MODE", raw);
        let cfg = Config::from_env_map(&env).unwrap();
        assert_eq!(
            cfg.modality_drift_mode,
            ModalityDriftMode::Fail,
            "raw={raw}"
        );
    }
}

#[test]
fn modality_drift_mode_invalid_falls_back_to_warn() {
    let mut env = empty();
    env.insert("ECAA_MODALITY_DRIFT_MODE", "panic-on-everything");
    let cfg = Config::from_env_map(&env).unwrap();
    assert_eq!(cfg.modality_drift_mode, ModalityDriftMode::Warn);
}

#[test]
fn builder_overrides_modality_drift_mode() {
    let cfg = Config::for_test()
        .modality_drift_mode(ModalityDriftMode::Fail)
        .build();
    assert_eq!(cfg.modality_drift_mode, ModalityDriftMode::Fail);
}
