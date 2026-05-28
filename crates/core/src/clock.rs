//! Deterministic clocks for byte-reproducible emit paths.
//!
//! Why a separate module from `time_helpers`?
//!
//! - `time_helpers::*` returns wall-clock values; calling those from
//!   any emit code path breaks the byte-reproducibility contract
//!   (CLAUDE.md: emitted packages must hash identically across runs).
//! - This module exposes a `Clock` trait that lets callers EITHER take
//!   `&WallClock` (for audit logs, harness progress events, decision
//!   timestamps where wall-clock is fine) OR `&FrozenClock` (mandatory
//!   for ro-crate-metadata.json::dateCreated, amendment-lineage.json,
//!   ChainOfCustody, and other artifacts that enter the BagIt manifest).
//! - `deterministic_emit_time(intake_hash)` derives a stable timestamp
//!   from the intake content hash so two emits of identical intake
//!   produce identical timestamps.
//!
//! Replaces direct wall-clock reads in ro_crate.rs::dateCreated,
//! amendment-lineage.json::created_at, and ChainOfCustody::new.

use chrono::{DateTime, TimeZone, Utc};

/// Trait for sources of "now" — implemented by `WallClock` (wall-clock-bound)
/// and `FrozenClock` (deterministic, used in emit paths).
///
/// Take `&dyn Clock` in any function whose output enters a
/// byte-reproducible artifact.
pub trait Clock: Send + Sync {
    /// Returns the current time according to this clock.
    fn now(&self) -> DateTime<Utc>;

    /// Returns the current time as an RFC-3339 string (e.g.,
    /// `"2026-05-16T12:34:56+00:00"`).
    fn now_rfc3339(&self) -> String {
        self.now().to_rfc3339()
    }
}

/// Wall-clock-backed `Clock` implementation. Use for non-emit callers
/// (audit logs, harness progress events, decision-record timestamps).
#[derive(Debug, Clone, Copy, Default)]
pub struct WallClock;

impl Clock for WallClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Pinned `Clock` returning a fixed timestamp. Use for emit-pipeline
/// callers; construct via `FrozenClock { at: deterministic_emit_time(&hash) }`.
///
/// `FrozenClock::default()` pins to `2026-01-01T00:00:00Z` (the lower
/// bound of `deterministic_emit_time`'s output range); it's intended for
/// tests that need *some* `Clock` to satisfy the signature but don't
/// assert on the timestamp value.
#[derive(Debug, Clone, Copy)]
pub struct FrozenClock {
    /// At.
    pub at: DateTime<Utc>,
}

impl Default for FrozenClock {
    fn default() -> Self {
        Self {
            at: Utc
                .timestamp_opt(1_767_225_600, 0) // 2026-01-01T00:00:00Z
                .single()
                .expect("in-range"),
        }
    }
}

impl Clock for FrozenClock {
    fn now(&self) -> DateTime<Utc> {
        self.at
    }
}

/// Derive a deterministic emit-time from a 32-byte content hash.
///
/// The output is a valid RFC-3339 timestamp in the range
/// `[2026-01-01T00:00:00Z, 2076-01-01T00:00:00Z)`. Two callers with
/// identical `hash` get identical output. Schema-wise indistinguishable
/// from a wall-clock timestamp.
pub fn deterministic_emit_time(hash: &[u8; 32]) -> DateTime<Utc> {
    const EPOCH_2026: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z
    const SPAN_50_YEARS: i64 = 1_577_847_600; // ~50y in seconds (no leap precision needed)
    let raw = u64::from_be_bytes([
        hash[0], hash[1], hash[2], hash[3], hash[4], hash[5], hash[6], hash[7],
    ]);
    let offset = (raw % (SPAN_50_YEARS as u64)) as i64;
    Utc.timestamp_opt(EPOCH_2026 + offset, 0)
        .single()
        .expect("in-range")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_advances() {
        let clock = WallClock;
        let a = clock.now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = clock.now();
        assert!(b > a, "wall clock must advance between successive calls");
    }

    #[test]
    fn frozen_clock_returns_fixed_value() {
        let pinned: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let clock = FrozenClock { at: pinned };
        assert_eq!(clock.now(), pinned);
        assert_eq!(clock.now(), pinned, "repeated calls return same value");
    }

    #[test]
    fn deterministic_emit_time_is_stable() {
        let hash = [0xABu8; 32];
        let a = deterministic_emit_time(&hash);
        let b = deterministic_emit_time(&hash);
        assert_eq!(a, b);
    }

    #[test]
    fn deterministic_emit_time_varies_with_hash() {
        let h1 = [0x01u8; 32];
        let h2 = [0x02u8; 32];
        assert_ne!(deterministic_emit_time(&h1), deterministic_emit_time(&h2));
    }

    #[test]
    fn deterministic_emit_time_in_documented_range() {
        let lower: DateTime<Utc> = "2026-01-01T00:00:00Z".parse().unwrap();
        let upper: DateTime<Utc> = "2076-01-01T00:00:00Z".parse().unwrap();
        for byte in 0u8..=255 {
            let hash = [byte; 32];
            let t = deterministic_emit_time(&hash);
            assert!(t >= lower && t < upper, "byte {byte} → {t} out of range");
        }
    }

    #[test]
    fn rfc3339_round_trips_for_frozen() {
        let pinned: DateTime<Utc> = "2026-05-16T12:34:56+00:00".parse().unwrap();
        let clock = FrozenClock { at: pinned };
        let s = clock.now_rfc3339();
        let parsed: DateTime<Utc> = s.parse().unwrap();
        assert_eq!(parsed, pinned);
    }
}
