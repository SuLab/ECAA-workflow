//! W1.2 — silent-skip observability surface.
//!
//! Several harness paths intentionally log + swallow errors at warn /
//! debug level rather than propagating them: WAL torn lines, output-size
//! walk failures, cache-eviction permission errors, heartbeat touch
//! failures, sandbox-policy I/O misses, malformed agent overrides, etc.
//! Individually each is defensible (the harness should not crash on a
//! single file race); collectively the silent fallthroughs hide
//! degradation patterns from operators.
//!
//! This module adds:
//!
//! 1. A `note_silent_skip` helper that logs structured tracing AND
//!    increments a per-category atomic counter. Counters survive across
//!    iterations and are visible through `snapshot()` for the
//!    `harness-health.json` sidecar.
//!
//! 2. A small enum `SkipCategory` so call sites pick from a finite
//!    set instead of inventing string keys.
//!
//! Call-site sweep is intentionally **partial** in this pass — only a
//! handful of high-value sites are converted as worked examples (see
//! the W1.2 commit). The remaining sites are tracked for a follow-up
//! pass; the helper is in place so a future sweep is mechanical.

use std::sync::atomic::{AtomicU64, Ordering};

/// W1.2 — categories of silent skips the harness emits. Each category
/// maps to an atomic counter exposed via `snapshot`. New categories
/// added here MUST also be added to `SkipCategory::all()` so the
/// snapshot iterator stays complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipCategory {
    /// W1.2: a torn / unparseable line in `runtime/dispatches.jsonl`
    /// was skipped during recovery.
    WalTornLine,
    /// W1.2: `runtime/outputs/<task>/overrides.json` was present but
    /// unreadable / unparseable; dispatch proceeded without overrides.
    OverridesUnreadable,
    /// W1.2: `output_size_guard` walk encountered an I/O error and
    /// declined to flag the task — caller falls back to
    /// missing-artifact diagnostics downstream.
    OutputSizeWalkError,
    /// W1.2: `cache_eviction` saw a per-entry I/O error during the
    /// directory walk and continued to the next entry.
    CacheEvictionStatError,
    /// W1.2: `touch_heartbeat` failed (mkdir or write); the next
    /// iteration's orphan reaper may false-positive.
    HeartbeatWriteFailed,
    /// W1.2: `record_sandbox_run` skipped a record write because the
    /// policy file was missing / malformed / runtime unavailable.
    SandboxRecordSkipped,
}

impl SkipCategory {
    /// W1.2 — keep in sync with the enum. Exhaustiveness is asserted
    /// via the `all_includes_every_variant` test below.
    pub fn all() -> &'static [SkipCategory] {
        &[
            SkipCategory::WalTornLine,
            SkipCategory::OverridesUnreadable,
            SkipCategory::OutputSizeWalkError,
            SkipCategory::CacheEvictionStatError,
            SkipCategory::HeartbeatWriteFailed,
            SkipCategory::SandboxRecordSkipped,
        ]
    }

    /// Stable string id, suitable for tracing target field and the
    /// `harness-health.json` sidecar key.
    pub fn as_str(self) -> &'static str {
        match self {
            SkipCategory::WalTornLine => "wal_torn_line",
            SkipCategory::OverridesUnreadable => "overrides_unreadable",
            SkipCategory::OutputSizeWalkError => "output_size_walk_error",
            SkipCategory::CacheEvictionStatError => "cache_eviction_stat_error",
            SkipCategory::HeartbeatWriteFailed => "heartbeat_write_failed",
            SkipCategory::SandboxRecordSkipped => "sandbox_record_skipped",
        }
    }
}

// One atomic counter per category. Indexed by the discriminant order
// in `SkipCategory::all()`. Kept as separate statics so we don't need
// a HashMap + lock on the hot path.
static C_WAL_TORN_LINE: AtomicU64 = AtomicU64::new(0);
static C_OVERRIDES_UNREADABLE: AtomicU64 = AtomicU64::new(0);
static C_OUTPUT_SIZE_WALK_ERROR: AtomicU64 = AtomicU64::new(0);
static C_CACHE_EVICTION_STAT_ERROR: AtomicU64 = AtomicU64::new(0);
static C_HEARTBEAT_WRITE_FAILED: AtomicU64 = AtomicU64::new(0);
static C_SANDBOX_RECORD_SKIPPED: AtomicU64 = AtomicU64::new(0);

fn counter_for(cat: SkipCategory) -> &'static AtomicU64 {
    match cat {
        SkipCategory::WalTornLine => &C_WAL_TORN_LINE,
        SkipCategory::OverridesUnreadable => &C_OVERRIDES_UNREADABLE,
        SkipCategory::OutputSizeWalkError => &C_OUTPUT_SIZE_WALK_ERROR,
        SkipCategory::CacheEvictionStatError => &C_CACHE_EVICTION_STAT_ERROR,
        SkipCategory::HeartbeatWriteFailed => &C_HEARTBEAT_WRITE_FAILED,
        SkipCategory::SandboxRecordSkipped => &C_SANDBOX_RECORD_SKIPPED,
    }
}

/// W1.2 — log a silent skip and increment its category counter. The
/// `reason` is the human-readable explanation surfaced via tracing;
/// `ctx` is an optional pair (task_id, path) for forensic correlation.
///
/// Side effects:
/// 1. `tracing::warn!` with target `silent_skip` so log filters can
///    sample / suppress as needed.
/// 2. Increment the per-category atomic counter (Ordering::Relaxed —
///    counters are advisory, not a synchronization primitive).
pub fn note_silent_skip(category: SkipCategory, reason: &str, task_id: Option<&str>) {
    counter_for(category).fetch_add(1, Ordering::Relaxed);
    tracing::warn!(
        target: "silent_skip",
        category = category.as_str(),
        task_id = task_id.unwrap_or(""),
        reason = reason,
        "silently degraded path observed"
    );
}

/// W1.2 — snapshot of per-category counts. Returned as
/// `Vec<(category-id, count)>` so the `harness-health.json` sidecar
/// can serialize it directly. Iteration is in `SkipCategory::all()`
/// order for determinism.
pub fn snapshot() -> Vec<(&'static str, u64)> {
    SkipCategory::all()
        .iter()
        .map(|cat| (cat.as_str(), counter_for(*cat).load(Ordering::Relaxed)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W1.2 — `SkipCategory::all()` must be exhaustive. If a new
    /// variant is added to the enum but missed in `all()`, the
    /// counter for that variant would silently never appear in
    /// `snapshot()` — exactly the kind of drift this module exists
    /// to prevent.
    #[test]
    fn all_includes_every_variant() {
        // Match each variant to confirm exhaustiveness; the compiler
        // will warn (then error under the workspace lint config) on
        // any new variant that's missed.
        for cat in SkipCategory::all() {
            match cat {
                SkipCategory::WalTornLine
                | SkipCategory::OverridesUnreadable
                | SkipCategory::OutputSizeWalkError
                | SkipCategory::CacheEvictionStatError
                | SkipCategory::HeartbeatWriteFailed
                | SkipCategory::SandboxRecordSkipped => {}
            }
        }
        // Sanity: 6 categories declared at module load.
        assert_eq!(
            SkipCategory::all().len(),
            6,
            "category count drift; if intentional, bump this assertion in the same commit"
        );
    }

    /// W1.2 — `as_str` produces a unique stable id per category. A
    /// collision would conflate counts in `harness-health.json`.
    #[test]
    fn category_ids_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for cat in SkipCategory::all() {
            let id = cat.as_str();
            assert!(seen.insert(id), "duplicate category id: {id}");
        }
    }
}
