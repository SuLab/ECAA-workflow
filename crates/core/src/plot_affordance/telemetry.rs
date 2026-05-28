//! Catalog-gap telemetry for the flexible plotting upgrade plan.
//!
//! `AffordanceFallbackCounter` is a session-scoped in-memory aggregator that
//! counts how often the affordance resolver fell back to a structural
//! primitive for a given `(semantic_type, primitive)` pair. The counter is
//! write-once-read-many: `record` is the only mutation path; `gaps_above` and
//! `all_gaps_sorted_by_count_desc` are the read paths consumed by the metrics
//! endpoint and the catalog-gaps UI card.
//!
//! `AffordanceFallbackRecord` is the JSONL-serializable shape written to
//! `runtime/affordance_fallbacks.jsonl` alongside the affordance sidecar.
//! The backend emitter sorts by `(task_id, port_name)` before writing to
//! preserve byte-determinism across runs (same discipline as
//! `runtime/plot_affordances.jsonl`).

use crate::ids::TaskId;
use std::collections::BTreeMap;

/// Session-scoped counter that accumulates structural-fallback hot spots.
///
/// Keyed by `(semantic_type, primitive)` so the caller can ask "how many
/// times did `counts_matrix` fall back to `heatmap`?" without scanning the
/// full record log. The inner `BTreeMap` guarantees deterministic iteration
/// order; all derived `Vec` outputs are additionally sorted by the methods
/// that produce them.
#[derive(Default, Clone, Debug)]
pub struct AffordanceFallbackCounter {
    by_key: BTreeMap<(String, String), u32>,
}

impl AffordanceFallbackCounter {
    /// Record one structural fallback for the given `(semantic_type, primitive)` pair.
    pub fn record(&mut self, semantic_type: &str, primitive: &str) {
        *self
            .by_key
            .entry((semantic_type.into(), primitive.into()))
            .or_default() += 1;
    }

    /// Return all `(semantic_type, primitive, count)` triples whose count
    /// is at or above `threshold`. Sorted by count descending, then
    /// `semantic_type` ascending, then `primitive` ascending.
    pub fn gaps_above(&self, threshold: u32) -> Vec<(String, String, u32)> {
        let mut v: Vec<_> = self
            .by_key
            .iter()
            .filter(|(_, &n)| n >= threshold)
            .map(|((s, p), &n)| (s.clone(), p.clone(), n))
            .collect();
        // Stable sort: descending count, then ascending semantic_type, then ascending primitive.
        v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        v
    }

    /// Return all recorded gaps sorted by count descending, then
    /// `semantic_type` ascending, then `primitive` ascending.
    ///
    /// A threshold of 1 (include everything) is the caller's
    /// responsibility via `gaps_above(1)`; this method always includes
    /// every key so the metrics endpoint can expose the full list and let
    /// the UI filter by threshold.
    pub fn all_gaps_sorted_by_count_desc(&self) -> Vec<(String, String, u32)> {
        let mut v: Vec<_> = self
            .by_key
            .iter()
            .map(|((s, p), &n)| (s.clone(), p.clone(), n))
            .collect();
        // Stable: descending count, then ascending semantic_type, then ascending primitive.
        v.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)).then(a.1.cmp(&b.1)));
        v
    }
}

/// Append-only JSONL record persisted to `runtime/affordance_fallbacks.jsonl`.
///
/// One record per affordance fallback event. The backend emitter sorts by
/// `(task_id, port_name)` before writing to preserve byte-determinism. The
/// sidecar is excluded from the BagIt manifest's byte-diff check (same
/// discipline as `runtime/plot_affordances.jsonl` and the audit logs) —
/// its presence in `emitter.rs::walk_for_manifest`'s exclusion list was
/// added when the affordance sidecar was introduced.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct AffordanceFallbackRecord {
    /// Task id.
    pub task_id: TaskId,
    /// Port name.
    pub port_name: String,
    /// Semantic type.
    pub semantic_type: String,
    /// Primitive.
    pub primitive: String,
    /// Fallback reason.
    pub fallback_reason: String,
}
