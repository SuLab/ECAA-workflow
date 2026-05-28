//! Catalog-gap telemetry tests for `AffordanceFallbackCounter`
//! and `AffordanceFallbackRecord`.
//!
//! Suite execution is deferred to the final integration sweep so
//! the harness isn't blocked on wiring the affordance resolver
//! call-site.

use scripps_workflow_core::plot_affordance::{AffordanceFallbackCounter, AffordanceFallbackRecord};

/// Basic record + aggregation: three events on two distinct keys.
/// - `gaps_above(2)` should return only the key that hit the threshold.
/// - `gaps_above(1)` should return both keys.
#[test]
fn counter_records_and_aggregates() {
    let mut counter = AffordanceFallbackCounter::default();

    // Two events for ("counts_matrix", "heatmap").
    counter.record("counts_matrix", "heatmap");
    counter.record("counts_matrix", "heatmap");
    // One event for ("volcano_result", "scatter").
    counter.record("volcano_result", "scatter");

    let above_two = counter.gaps_above(2);
    assert_eq!(above_two.len(), 1, "only one key hit threshold 2");
    assert_eq!(above_two[0].0, "counts_matrix");
    assert_eq!(above_two[0].1, "heatmap");
    assert_eq!(above_two[0].2, 2);

    let above_one = counter.gaps_above(1);
    assert_eq!(above_one.len(), 2, "both keys are at or above threshold 1");
    // Count-descending: counts_matrix/heatmap (2) first, then volcano_result/scatter (1).
    assert_eq!(above_one[0].2, 2);
    assert_eq!(above_one[1].2, 1);
}

/// Sort stability: two keys with the same count should be ordered by
/// `semantic_type` ascending, then `primitive` ascending.
#[test]
fn gaps_sorted_deterministically() {
    let mut counter = AffordanceFallbackCounter::default();

    // Two events for each key so they tie on count.
    counter.record("zzz_type", "bar");
    counter.record("zzz_type", "bar");
    counter.record("aaa_type", "violin");
    counter.record("aaa_type", "violin");

    let gaps = counter.all_gaps_sorted_by_count_desc();
    assert_eq!(gaps.len(), 2);
    // Same count → semantic_type ascending: "aaa_type" before "zzz_type".
    assert_eq!(
        gaps[0].0, "aaa_type",
        "aaa_type should sort before zzz_type"
    );
    assert_eq!(gaps[1].0, "zzz_type");
}

/// `AffordanceFallbackRecord` must round-trip through JSON unchanged.
#[test]
fn record_serde_round_trip() {
    let original = AffordanceFallbackRecord {
        task_id: "normalize_counts".to_string().into(),
        port_name: "count_matrix_normalized".to_string(),
        semantic_type: "counts_matrix".to_string(),
        primitive: "heatmap".to_string(),
        fallback_reason: "no catalog entry for semantic_type".to_string(),
    };

    let json = serde_json::to_string(&original).expect("serialize");
    let round_tripped: AffordanceFallbackRecord = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(round_tripped.task_id, original.task_id);
    assert_eq!(round_tripped.port_name, original.port_name);
    assert_eq!(round_tripped.semantic_type, original.semantic_type);
    assert_eq!(round_tripped.primitive, original.primitive);
    assert_eq!(round_tripped.fallback_reason, original.fallback_reason);
}
