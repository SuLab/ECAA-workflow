//! Time helpers. Consolidates 16+ scattered `Utc::now().to_rfc3339()` and
//! `SystemTime` patterns across the workspace.
//!
//! Existing call sites can migrate to this module incrementally.
//!
//! Determinism note: every helper here returns a value derived from the
//! wall clock and is therefore intentionally NOT used in any code path
//! that participates in byte-reproducible package emission. The compiler
//! contract in CLAUDE.md forbids timestamps in generated artifacts
//! (except intentional `output-*-YYYY-MM-DD` paths). These helpers serve
//! the conversation crate's session timestamps, harness progress events,
//! decision-log entries, and other audit-trail uses — none of which
//! enter the emitted package's reproducibility hash.

use chrono::Utc;
use std::time::{SystemTime, UNIX_EPOCH};

/// RFC-3339 (ISO-8601-with-timezone) string for "now" in UTC.
///
/// Format: `2026-05-15T12:34:56.789012345+00:00`.
pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

/// Unix epoch millis. Returns `0` if the system clock is somehow before
/// the epoch (impossible on every sane host but the `duration_since`
/// signature requires the fallback).
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Compact UTC timestamp suitable for filenames: `YYYYMMDDTHHMMSSZ`.
/// No separators, no fractional seconds.
pub fn now_compact_filename() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_rfc3339_parses_as_rfc3339() {
        let s = now_rfc3339();
        let parsed = chrono::DateTime::parse_from_rfc3339(&s);
        assert!(parsed.is_ok(), "expected RFC3339, got: {s}");
    }

    #[test]
    fn now_unix_ms_within_sane_range() {
        let v = now_unix_ms();
        // Sanity bracket: after Y2024 and before Y2200. Encodes
        // "the clock is roughly correct" without being brittle.
        const Y2024_MS: u64 = 1_704_067_200_000;
        const Y2200_MS: u64 = 7_258_118_400_000;
        assert!(v > Y2024_MS, "clock before 2024? got {v}");
        assert!(v < Y2200_MS, "clock after 2200? got {v}");
    }

    #[test]
    fn now_unix_ms_monotonic_within_resolution() {
        let a = now_unix_ms();
        // Don't sleep — just assert the second sample is >= the first.
        // Wall-clock millis can be equal on a fast machine; cannot be less.
        let b = now_unix_ms();
        assert!(b >= a, "{b} < {a}");
    }

    #[test]
    fn now_compact_filename_has_expected_shape() {
        let s = now_compact_filename();
        // YYYYMMDDTHHMMSSZ — exactly 16 chars.
        assert_eq!(s.len(), 16, "got: {s}");
        assert!(s.ends_with('Z'), "got: {s}");
        assert!(s.chars().nth(8) == Some('T'), "got: {s}");
        // All non-T/Z chars are digits.
        for (i, c) in s.chars().enumerate() {
            if i == 8 || i == 15 {
                continue;
            }
            assert!(c.is_ascii_digit(), "non-digit at pos {i} in {s}");
        }
    }

    #[test]
    fn now_compact_filename_starts_with_current_year_prefix() {
        let s = now_compact_filename();
        let year_prefix = chrono::Utc::now().format("%Y").to_string();
        assert!(s.starts_with(&year_prefix), "got: {s}");
    }
}
