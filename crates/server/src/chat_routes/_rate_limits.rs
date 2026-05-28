//! Per-LLM-endpoint per-minute rate-limit caps.
//!
//! Centralises the literal limits consumed by seven route call sites
//! (`turns.rs`, `sessions.rs`, `explain.rs`, `summary.rs`,
//! `remediation.rs`, `execution/start.rs`, `branches.rs`) so they
//! don't drift between sites. Tuning is via env-var overrides.
//!
//! ## Env overrides
//!
//! `ECAA_LLM_RATE_LIMIT_TURN`, `_SCORE`, `_EXPLAIN`, `_SUMMARY`,
//! `_REMEDIATION`, `_START_EXEC`, `_BRANCH` — each accepts a non-negative
//! integer (requests per minute per session). Unset = built-in default.

#[derive(Debug, Clone, Copy)]
pub struct LlmEndpointRateLimits {
    pub turn: u32,
    pub score: u32,
    pub explain: u32,
    pub summary: u32,
    pub remediation: u32,
    pub start_exec: u32,
    pub branch: u32,
}

impl LlmEndpointRateLimits {
    pub fn from_env() -> Self {
        Self {
            turn: env_u32("ECAA_LLM_RATE_LIMIT_TURN", 30),
            score: env_u32("ECAA_LLM_RATE_LIMIT_SCORE", 6),
            explain: env_u32("ECAA_LLM_RATE_LIMIT_EXPLAIN", 30),
            summary: env_u32("ECAA_LLM_RATE_LIMIT_SUMMARY", 6),
            remediation: env_u32("ECAA_LLM_RATE_LIMIT_REMEDIATION", 6),
            start_exec: env_u32("ECAA_LLM_RATE_LIMIT_START_EXEC", 12),
            branch: env_u32("ECAA_LLM_RATE_LIMIT_BRANCH", 6),
        }
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    ecaa_workflow_core::env_helpers::env_parse(key, default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_audit_baseline() {
        // Lock-in: these were the literals before the centralization.
        // Changing requires explicit operator decision.
        let limits = LlmEndpointRateLimits {
            turn: 30,
            score: 6,
            explain: 30,
            summary: 6,
            remediation: 6,
            start_exec: 12,
            branch: 6,
        };
        let from_env = {
            // Ensure no env vars are set in test env.
            for k in [
                "ECAA_LLM_RATE_LIMIT_TURN",
                "ECAA_LLM_RATE_LIMIT_SCORE",
                "ECAA_LLM_RATE_LIMIT_EXPLAIN",
                "ECAA_LLM_RATE_LIMIT_SUMMARY",
                "ECAA_LLM_RATE_LIMIT_REMEDIATION",
                "ECAA_LLM_RATE_LIMIT_START_EXEC",
                "ECAA_LLM_RATE_LIMIT_BRANCH",
            ] {
                std::env::remove_var(k);
            }
            LlmEndpointRateLimits::from_env()
        };
        assert_eq!(from_env.turn, limits.turn);
        assert_eq!(from_env.score, limits.score);
        assert_eq!(from_env.explain, limits.explain);
        assert_eq!(from_env.summary, limits.summary);
        assert_eq!(from_env.remediation, limits.remediation);
        assert_eq!(from_env.start_exec, limits.start_exec);
        assert_eq!(from_env.branch, limits.branch);
    }
}
