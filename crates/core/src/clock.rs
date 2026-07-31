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

/// Lower bound of the genuine run window: `2026-01-01T00:00:00Z`. The
/// project's deterministic epoch base. A run epoch (or a root
/// `dateCreated`) below this is not a plausible run time.
pub const RUN_EPOCH_BASE: i64 = 1_767_225_600; // 2026-01-01T00:00:00Z

/// Upper bound of the genuine run window: `2031-01-01T00:00:00Z` — five
/// years past the epoch base. The run epoch a deterministic clock anchors
/// to must fall inside `[RUN_EPOCH_BASE, RUN_WINDOW_END)`; a
/// `SOURCE_DATE_EPOCH` outside this window (e.g. the 2061 values the hash
/// projection can emit) is not a real run date and is clamped to the floor
/// by [`run_epoch_clock_from`].
pub const RUN_WINDOW_END: i64 = 1_924_992_000; // 2031-01-01T00:00:00Z

/// Deterministic `Clock` anchored to the genuine RUN epoch
/// (`SOURCE_DATE_EPOCH`), NOT to the opaque hash projection.
///
/// `SOURCE_DATE_EPOCH` is the run-level Unix timestamp the harness
/// captures once at startup and threads identically to every task (see
/// [`crate::determinism_seeds`]); `bagit.rs` already pins `Bagging-Date`
/// to this run epoch rather than to [`deterministic_emit_time`], which
/// can map uniformly into `[2026, 2076)` and land decades in the future
/// (e.g. 2061). Anchoring `ro-crate-metadata.json::dateCreated` here too
/// keeps the root date CONSISTENT with `Bagging-Date` and inside the run
/// window.
///
/// Resolution order:
/// - `SOURCE_DATE_EPOCH` set to an in-window integer → that instant.
/// - unset, unparseable, or out of `[RUN_EPOCH_BASE, RUN_WINDOW_END)` →
///   the [`RUN_EPOCH_BASE`] floor (the same `2026-01-01` base
///   `FrozenClock::default()` / the emit `Bagging-Date` pin use), so the
///   emit byte-baseline is unchanged when no run epoch is present.
///
/// Determinism is preserved: the value is a pure function of the run
/// epoch (or the constant floor), so two emits of the same run are
/// byte-identical.
pub fn run_epoch_clock() -> FrozenClock {
    run_epoch_clock_from(std::env::var("SOURCE_DATE_EPOCH").ok().as_deref())
}

/// Core of [`run_epoch_clock`], parameterized on the raw
/// `SOURCE_DATE_EPOCH` value so it is testable without mutating process
/// env. See [`run_epoch_clock`] for the resolution order.
pub fn run_epoch_clock_from(source_date_epoch: Option<&str>) -> FrozenClock {
    let secs = source_date_epoch
        .and_then(|s| s.trim().parse::<i64>().ok())
        .filter(|&s| (RUN_EPOCH_BASE..RUN_WINDOW_END).contains(&s))
        .unwrap_or(RUN_EPOCH_BASE);
    FrozenClock {
        at: Utc
            .timestamp_opt(secs, 0)
            .single()
            .expect("RUN_EPOCH_BASE..RUN_WINDOW_END is a valid timestamp range"),
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
