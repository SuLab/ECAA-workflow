//! `ServiceError` + the retry-exhausted fallback turn used when the
//! tool loop hits its iteration cap without a terminating `EndTurn`,
//! plus the Anthropic-call retry policy classifier (S2.6).

use crate::session::{AssistantIntent, Turn};
use std::time::Duration;

/// `ServiceError` carries the three failure shapes the
/// service layer surfaces back to its caller. Migrated to
/// `thiserror::Error` per the round-3 stay-on-anyhow-plus-thiserror
/// decision; the manual `Display` + `Error` impls live in
/// hand-rolled boilerplate, which thiserror eliminates.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ServiceError {
    /// The requested session was not found in the store.
    #[error("session not found")]
    SessionNotFound,
    /// The LLM backend returned an unrecoverable error.
    #[error("llm backend error: {0}")]
    Backend(String),
    /// An unexpected server-side error occurred in the service layer.
    #[error("internal error: {0}")]
    Internal(String),
}

pub(super) fn retry_exhausted_turn() -> Turn {
    let mut t = Turn::assistant(
        "I had trouble getting through that last step. Could you rephrase \
         what you just said, or break it into smaller pieces?",
    );
    t.intent = Some(AssistantIntent::Blocker);
    t
}

// ── Anthropic call retry policy (plan S2.6) ─────────────────────────

/// Maximum retries per turn before we surface the error to the
/// caller. 2 keeps the worst-case turn latency bounded (one initial
/// send + two retries with up to ~12s of backoff each = ~25s p99
/// pathological case).
pub(super) const MAX_RETRIES_PER_TURN: u32 = 2;

/// Base backoff for the first retry. Doubles on each subsequent
/// retry. 1s gives Anthropic enough time to recover from a transient
/// connection blip without making the user wait visibly.
const BASE_BACKOFF_SECS: u64 = 1;

/// Cap on the exponential backoff. 30s is our soft ceiling so a
/// pathological retry loop never extends the user-visible turn
/// beyond half a minute. Real-world 429 retry-after values from
/// Anthropic top out around 60s, but those are signaled via the
/// `retry-after` response header which `extract_retry_after_secs`
/// reads explicitly and prefers over our exponential default.
const MAX_BACKOFF_SECS: u64 = 30;

/// True if the error string indicates a retriable failure (transient
/// network error, 429 rate-limit, or 5xx server-side issue).
/// Terminal errors (4xx other than 429 — typically 400 schema
/// violation, 401 auth, 403 forbidden) bubble immediately.
///
/// We string-match on the formatted error message because the
/// anthropic client uses `anyhow::anyhow!` to wrap status codes.
/// A future refactor to typed errors would let this switch on the
/// status code directly; for now the substring match is the cleanest
/// surface since the formatter is stable and tested.
pub(super) fn classify_retriable(err: &str) -> bool {
    // Anthropic request-body timeout (the configured ~180s ceiling
    // fired) — explicitly terminal. The point of the hard timeout is
    // to surface a stuck Anthropic call to the SME quickly; retrying
    // would just burn two more 180s windows against an already-hung
    // backend before the user sees anything. The dedicated marker
    // (see `anthropic::client::REQUEST_BODY_TIMEOUT_MARKER`) is checked
    // BEFORE the generic "timed out" / "connection" substrings below
    // so generic-shape connection blips remain retriable.
    if err.contains(crate::anthropic::client::REQUEST_BODY_TIMEOUT_MARKER) {
        return false;
    }
    // 429 rate-limited → retriable
    if err.contains("HTTP 429") || err.contains("rate-limited") {
        return true;
    }
    // 5xx server-side errors → retriable
    if err.contains("HTTP 500")
        || err.contains("HTTP 502")
        || err.contains("HTTP 503")
        || err.contains("HTTP 504")
        || err.contains("HTTP 529")
    {
        return true;
    }
    // Stream / connection-level errors → retriable
    if err.contains("SSE chunk")
        || err.contains("reading response body")
        || err.contains("dns error")
        || err.contains("connection")
        || err.contains("timed out")
        || err.contains("operation timed out")
    {
        return true;
    }
    // Everything else (4xx schema/auth/forbidden, parse errors,
    // unexpected response shape) is terminal.
    false
}

/// Exponential backoff with ±15% jitter. Jitter prevents thundering-
/// herd retries when many sessions simultaneously hit a 429 (e.g.
/// the org-wide minute boundary on a per-minute rate limit).
///
/// `attempt` is 0-indexed: attempt=0 → 1s, attempt=1 → 2s, attempt=2
/// → 4s, etc. Capped at `MAX_BACKOFF_SECS`. Jitter range is
/// uniformly random in [0.85, 1.15] of the deterministic base.
pub(super) fn backoff_with_jitter(attempt: u32) -> Duration {
    let base = BASE_BACKOFF_SECS.saturating_mul(1_u64 << attempt.min(8));
    let capped = base.min(MAX_BACKOFF_SECS);
    // Hand-rolled jitter without pulling in rand: hash the system
    // nanosecond clock and remap to [85, 115]. Determinism doesn't
    // matter here — jitter's whole point is non-determinism — but we
    // also don't want a fresh dep on rand for one number.
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0) as u64;
    let jitter_pct = 85 + (now_ns % 31); // 85..=115
    let jittered_ms = (capped * 1000) * jitter_pct / 100;
    Duration::from_millis(jittered_ms)
}

/// Read the `Retry-After` header value (in seconds) from an Anthropic
/// 429 response error string when present. Returns `None` if the
/// header wasn't surfaced (older client or non-429 response). When
/// honored, we use this directly instead of the exponential default
/// — Anthropic knows when its quota refills.
pub(super) fn extract_retry_after_secs(err: &str) -> Option<u64> {
    // Look for "retry-after: <n>" in the error body. The anthropic
    // client formats 429 errors as
    // "anthropic API rate-limited (HTTP 429): <body>"
    // and the body sometimes echoes the header value. Parse defensively.
    //
    // To avoid matching substrings of unrelated header names (a stray
    // "x-retry-after-foo" elsewhere in the body would otherwise be
    // detected), require that the matched "retry-after" token begins at
    // the start of the string or is preceded by whitespace/newline/
    // header-list separator. We also require the immediately-following
    // character to be one of the canonical separators (':', '=', ' ',
    // '\t') or end-of-string so we don't latch onto "retry-after-ms"
    // (Anthropic doesn't emit that today but the parser stays correct
    // if they do).
    let lower = err.to_lowercase();
    let key = "retry-after";
    let mut search_from = 0usize;
    let after_str = loop {
        let rel = lower[search_from..].find(key)?;
        let idx = search_from + rel;
        let prev_is_boundary = idx == 0
            || lower.as_bytes().get(idx - 1).is_some_and(|c| {
                matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b',' | b';' | b'{' | b'(')
            });
        let after_ascii = lower.as_bytes().get(idx + key.len()).copied();
        let next_is_separator = after_ascii.is_none()
            || matches!(
                after_ascii,
                Some(b':' | b'=' | b' ' | b'\t' | b'\n' | b'\r')
            );
        if prev_is_boundary && next_is_separator {
            break &err[idx + key.len()..];
        }
        search_from = idx + key.len();
        if search_from >= lower.len() {
            return None;
        }
    };
    // Skip "=" or ":" or whitespace before the number.
    let after = after_str.trim_start_matches([':', '=', ' ', '\t']);
    // Take any Unicode decimal digit — Anthropic emits ASCII today but
    // header values are technically token strings; a defensive parse
    // tolerates whatever upstream sends so long as the value parses as
    // a plausible u64 seconds.
    let digits: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let secs: u64 = digits.parse().ok()?;
    if secs == 0 || secs > 300 {
        // Implausible value; fall back to exponential default.
        return None;
    }
    Some(secs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_marks_429_retriable() {
        assert!(classify_retriable(
            "anthropic API rate-limited (HTTP 429): rps cap"
        ));
        assert!(classify_retriable("HTTP 429"));
    }

    #[test]
    fn classify_marks_5xx_retriable() {
        for code in &["HTTP 500", "HTTP 502", "HTTP 503", "HTTP 504", "HTTP 529"] {
            assert!(classify_retriable(code), "{} should be retriable", code);
        }
    }

    #[test]
    fn classify_marks_4xx_non_429_terminal() {
        for code in &["HTTP 400", "HTTP 401", "HTTP 403", "HTTP 404"] {
            assert!(!classify_retriable(code), "{} should be terminal", code);
        }
    }

    #[test]
    fn classify_marks_stream_errors_retriable() {
        assert!(classify_retriable("reading SSE chunk"));
        assert!(classify_retriable("dns error: nodename nor servname"));
        assert!(classify_retriable("connection reset by peer"));
        assert!(classify_retriable("operation timed out"));
    }

    #[test]
    fn classify_marks_request_body_timeout_terminal() {
        // D8 mitigation — when the Anthropic request-body timeout fires
        // (the configured ~180s ceiling), the call is NOT retried. The
        // sender wraps these in `REQUEST_BODY_TIMEOUT_MARKER`; a generic
        // "timed out" string without the marker stays retriable.
        use crate::anthropic::client::REQUEST_BODY_TIMEOUT_MARKER;
        let err = format!(
            "{} after 180s on POST https://api.anthropic.com/v1/messages: operation timed out",
            REQUEST_BODY_TIMEOUT_MARKER
        );
        assert!(
            !classify_retriable(&err),
            "request-body timeout must NOT be retried (burns API budget on a hung backend)"
        );
        // Bare "timed out" (connect-phase blip from a non-anthropic
        // source) remains retriable so the existing behaviour for
        // transient DNS / TCP flakes isn't regressed.
        assert!(classify_retriable("operation timed out"));
    }

    #[test]
    fn backoff_grows_exponentially_within_cap() {
        // Smoke-check the deterministic part of the curve. Don't lock
        // exact values because of jitter — instead lock the pre-jitter
        // base via the cap behaviour.
        let d0 = backoff_with_jitter(0).as_millis() as u64;
        let d1 = backoff_with_jitter(1).as_millis() as u64;
        let d4 = backoff_with_jitter(4).as_millis() as u64;
        // attempt 0 base is 1s with ±15% jitter → 850–1150ms
        assert!((850..=1150).contains(&d0), "attempt 0 was {} ms", d0);
        // attempt 1 base is 2s → 1700–2300ms
        assert!((1700..=2300).contains(&d1), "attempt 1 was {} ms", d1);
        // attempt 4 base would be 16s but capped at 30s → 13600–18400ms
        assert!((13600..=18400).contains(&d4), "attempt 4 was {} ms", d4);
    }

    #[test]
    fn extract_retry_after_reads_explicit_header() {
        assert_eq!(
            extract_retry_after_secs("anthropic API rate-limited: retry-after: 12"),
            Some(12)
        );
        assert_eq!(
            extract_retry_after_secs("retry-after=45 something else"),
            Some(45)
        );
    }

    #[test]
    fn extract_retry_after_rejects_implausible() {
        // 0 and >300 fall through to exponential default
        assert_eq!(extract_retry_after_secs("retry-after: 0"), None);
        assert_eq!(extract_retry_after_secs("retry-after: 99999"), None);
        assert_eq!(extract_retry_after_secs("no header here"), None);
    }
}
