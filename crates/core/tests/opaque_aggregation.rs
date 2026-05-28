//! v3 §4 / v4 §4 Round-2 closure (G1 / G13) — opaque-type cross-session
//! aggregator regression suite.
//!
//! Mirrors the `LocalExtension` graduation aggregator's test surface
//! (`crates/conversation/src/session/cross_session_aggregator.rs`). The
//! `OpaqueAggregator` records persistent `Opaque` semantic-type
//! observations across sessions so the registry-improvement pipeline
//! surfaces them as candidates for ontology / extension promotion.

use chrono::Utc;
use scripps_workflow_conversation::session::opaque_aggregator::OpaqueAggregator;
use tempfile::TempDir;

#[test]
fn opaque_aggregator_records_first_observation() {
    let dir = TempDir::new().unwrap();
    let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
    let now = Utc::now().to_rfc3339();
    agg.record_observation("blake3:abc", "session_1", "data_acq", "raw_reads", &now)
        .unwrap();

    let entries = agg.load_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].occurrence_count, 1);
    assert_eq!(entries[0].session_ids, vec!["session_1"]);
}

#[test]
fn opaque_aggregator_increments_on_repeated_observation() {
    let dir = TempDir::new().unwrap();
    let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
    let now = Utc::now().to_rfc3339();
    for sid in &["s1", "s2", "s3"] {
        agg.record_observation("blake3:xyz", sid, "node_a", "port_a", &now)
            .unwrap();
    }
    let entries = agg.load_all().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].occurrence_count, 3);
    assert_eq!(entries[0].session_ids.len(), 3);
}

#[test]
fn opaque_aggregator_flags_registry_improvement_candidate() {
    let dir = TempDir::new().unwrap();
    let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
    let now = Utc::now().to_rfc3339();
    for sid in &["s1", "s2", "s3"] {
        agg.record_observation("blake3:improve_me", sid, "node_a", "port_a", &now)
            .unwrap();
    }
    let candidates =
        agg.registry_improvement_candidates(/* threshold sessions */ 3, /* days */ 30);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].opaque_hash, "blake3:improve_me");
}

#[test]
fn opaque_aggregator_filters_by_days_window() {
    use chrono::{Duration, Utc};
    let dir = TempDir::new().unwrap();
    let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));

    let old_ts = (Utc::now() - Duration::days(45)).to_rfc3339();
    agg.record_observation("blake3:old", "s1", "node_a", "port_a", &old_ts)
        .unwrap();
    agg.record_observation("blake3:old", "s2", "node_a", "port_a", &old_ts)
        .unwrap();
    agg.record_observation("blake3:old", "s3", "node_a", "port_a", &old_ts)
        .unwrap();

    let recent_ts = Utc::now().to_rfc3339();
    agg.record_observation("blake3:recent", "s4", "node_b", "port_b", &recent_ts)
        .unwrap();
    agg.record_observation("blake3:recent", "s5", "node_b", "port_b", &recent_ts)
        .unwrap();
    agg.record_observation("blake3:recent", "s6", "node_b", "port_b", &recent_ts)
        .unwrap();

    let candidates = agg.registry_improvement_candidates(3, 30);
    assert_eq!(
        candidates.len(),
        1,
        "only the recent observation should pass the 30-day window"
    );
    assert_eq!(candidates[0].opaque_hash, "blake3:recent");
}
