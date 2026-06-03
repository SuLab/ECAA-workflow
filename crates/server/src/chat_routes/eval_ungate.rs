//! Eval-only schema-gate ungating (E6) — provably inert in production.
//!
//! The empirical eval's third arm (`ecaa-ungated`) isolates the schema gate by
//! disabling the high-impact confirmation gate. That ungating is a TEST-ONLY
//! affordance: it is honored ONLY when BOTH `ECAA_EVAL_LIVE=1` AND the
//! eval-only `ECAA_EVAL_UNGATE_SCHEMA_GATE=1` flag are set. In any production
//! configuration (neither flag, or only one) it is inert — the gate stays on.
//!
//! This lives in `crates/server` and NEVER touches `crates/core`: the compiler
//! has no notion of an ungated mode. It is not a production execution path.

/// Pure decision function: the schema gate is ungated ONLY when BOTH
/// `ECAA_EVAL_LIVE` and `ECAA_EVAL_UNGATE_SCHEMA_GATE` are exactly "1".
/// Split from the env read so the inertness contract is unit-testable without
/// touching process env (which the prior campaign's env_clear trap silently
/// defeated — a mocked env hid the no-op).
fn eval_ungate_enabled_from(eval_live: Option<&str>, ungate: Option<&str>) -> bool {
    eval_live == Some("1") && ungate == Some("1")
}

/// Live entry point: read both flags from the process env. In production
/// neither (or only one) is set, so this returns false and the gate stays on.
/// NEVER call this on a path that can run without ECAA_EVAL_LIVE.
#[allow(dead_code)] // wired only by an operator-gated change; the contract is the test
pub(crate) fn eval_ungate_enabled() -> bool {
    eval_ungate_enabled_from(
        std::env::var("ECAA_EVAL_LIVE").ok().as_deref(),
        std::env::var("ECAA_EVAL_UNGATE_SCHEMA_GATE").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both flags unset -> inert (gate stays ON).
    #[test]
    fn inert_when_no_flags() {
        let enabled = eval_ungate_enabled_from(None, None);
        assert!(!enabled, "ungate must be inert with no flags set");
    }

    /// ECAA_EVAL_UNGATE_SCHEMA_GATE=1 alone (no ECAA_EVAL_LIVE) -> inert.
    /// This is the production-safety property: a stray ungate flag in a prod
    /// env does NOTHING without the live-eval gate.
    #[test]
    fn inert_without_eval_live() {
        let enabled = eval_ungate_enabled_from(None, Some("1"));
        assert!(!enabled, "ungate must be inert without ECAA_EVAL_LIVE");
    }

    /// ECAA_EVAL_LIVE=1 alone (no ungate flag) -> inert (default: gate ON).
    #[test]
    fn inert_with_only_eval_live() {
        let enabled = eval_ungate_enabled_from(Some("1"), None);
        assert!(!enabled, "ECAA_EVAL_LIVE alone must not ungate");
    }

    /// BOTH flags set -> enabled. This is the ONLY combination that ungates,
    /// and it is reachable only in an operator-run live eval. The POSITIVE
    /// assertion guards against the silent-no-op trap (a flag that never takes
    /// effect would make the third arm secretly identical to the gated arm).
    #[test]
    fn enabled_only_when_both_flags_set() {
        let enabled = eval_ungate_enabled_from(Some("1"), Some("1"));
        assert!(enabled, "both flags set must enable ungate (else the arm is vacuous)");
    }

    /// Non-"1" values are not truthy.
    #[test]
    fn rejects_non_one_values() {
        assert!(!eval_ungate_enabled_from(Some("true"), Some("1")));
        assert!(!eval_ungate_enabled_from(Some("1"), Some("0")));
        assert!(!eval_ungate_enabled_from(Some("1"), Some("yes")));
    }
}
