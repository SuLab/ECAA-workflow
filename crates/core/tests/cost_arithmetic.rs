use scripps_workflow_core::cost::{Cost, MAX_CALL_MICRO_USD};

#[test]
fn realistic_anthropic_input_token_pricing() {
    // Sonnet 4.6: $3/million input tokens = $3e-6 per token
    let per_token = Cost::from_usd_f64(3.0e-6).unwrap();
    let typical_turn_tokens = 8_000u64;
    let turn_cost = Cost::from_token_count(typical_turn_tokens, per_token);
    let usd = turn_cost.as_usd();
    assert!(
        (usd - 0.024).abs() < 0.001,
        "expected ~$0.024 for 8k tokens, got {usd}"
    );
}

#[test]
fn malicious_token_report_capped() {
    let per_token = Cost::from_usd_f64(3.0e-6).unwrap();
    let cost = Cost::from_token_count(u64::MAX, per_token);
    assert_eq!(cost.as_micro_usd(), MAX_CALL_MICRO_USD);
    assert!(cost.is_at_call_cap());
}

#[test]
fn null_pricing_yields_zero_not_panic() {
    let cost = Cost::from_token_count(1000, Cost::ZERO);
    assert_eq!(cost, Cost::ZERO);
}

#[test]
fn aggregate_total_saturates() {
    let many: Vec<Cost> = (0..100)
        .map(|_| Cost::from_micro_usd(u64::MAX / 50))
        .collect();
    let total: Cost = many.into_iter().sum();
    assert_eq!(total, Cost::from_micro_usd(u64::MAX));
}
