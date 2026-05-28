//! Policy enum the AWS executor consults when the realized compute
//! exceeds the high-water baseline computed by `sizing.rs`. Driven by
//! the `ECAA_AWS_HIGH_WATER_POLICY` env var.

use std::env;

/// Action the AWS executor takes when a task's realized resource use exceeds the high-water baseline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HighWaterPolicy {
    /// Halt the task with a Failed state and a clear error so the SME
    /// can decide how to proceed. Conservative default for cost-
    /// sensitive workflows.
    Block,
    /// Upgrade the instance type to the next shape that fits the
    /// realized requirement and resume. The default — surfaces the
    /// bump in the metrics tab via the `high_water_exceeded_count`
    /// counter so the user knows the baseline drifted.
    #[default]
    Resize,
    /// Log a warning and run anyway on the original shape. Useful in
    /// CI / non-cost-sensitive environments where the priority is
    /// completing the task even if it's slow / OOM-risky.
    Continue,
}

impl HighWaterPolicy {
    /// Parse the `ECAA_AWS_HIGH_WATER_POLICY` env var. Returns
    /// `Default::default()` (Resize) when the var is unset, an error
    /// when the value isn't one of the three accepted tokens. The
    /// case-insensitive parse keeps `Block`, `BLOCK`, `block` all
    /// equivalent so operators don't get bitten by shell quoting.
    pub fn from_env() -> Result<Self, HighWaterPolicyParseError> {
        match env::var("ECAA_AWS_HIGH_WATER_POLICY") {
            Ok(raw) => Self::parse(&raw),
            Err(env::VarError::NotPresent) => Ok(Self::default()),
            Err(env::VarError::NotUnicode(raw)) => {
                Err(HighWaterPolicyParseError::NotUtf8(format!("{:?}", raw)))
            }
        }
    }

    /// Parse a `ECAA_AWS_HIGH_WATER_POLICY` token string. Case-insensitive.
    pub fn parse(raw: &str) -> Result<Self, HighWaterPolicyParseError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "block" => Ok(HighWaterPolicy::Block),
            "resize" => Ok(HighWaterPolicy::Resize),
            "continue" => Ok(HighWaterPolicy::Continue),
            other => Err(HighWaterPolicyParseError::UnknownToken(other.to_string())),
        }
    }

    /// Stable wire identifier — used in per-stage policy overrides.
    /// Lowercase to match the env-var token shape.
    pub fn as_str(&self) -> &'static str {
        match self {
            HighWaterPolicy::Block => "block",
            HighWaterPolicy::Resize => "resize",
            HighWaterPolicy::Continue => "continue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Errors returned when parsing a `ECAA_AWS_HIGH_WATER_POLICY` value.
pub enum HighWaterPolicyParseError {
    /// The value was not one of "block", "resize", or "continue".
    UnknownToken(String),
    /// The env var value contained invalid UTF-8.
    NotUtf8(String),
}

impl std::fmt::Display for HighWaterPolicyParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HighWaterPolicyParseError::UnknownToken(t) => write!(
                f,
                "ECAA_AWS_HIGH_WATER_POLICY='{}' is not a valid policy. Valid: block, resize, continue.",
                t
            ),
            HighWaterPolicyParseError::NotUtf8(raw) => write!(
                f,
                "ECAA_AWS_HIGH_WATER_POLICY is not valid UTF-8: {}",
                raw
            ),
        }
    }
}

impl std::error::Error for HighWaterPolicyParseError {}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. Test code uses
    // `unsafe { std::env::set_var / remove_var }` (unsafe in Rust 2024
    // edition because the env table is not thread-safe). All call sites
    // are single-threaded test setup; bounded waiver scoped to this
    // `mod tests` block.
    #![allow(unsafe_code)]
    use super::*;

    #[test]
    fn default_is_resize() {
        assert_eq!(HighWaterPolicy::default(), HighWaterPolicy::Resize);
    }

    #[test]
    fn parse_accepts_canonical_tokens() {
        assert_eq!(
            HighWaterPolicy::parse("block").unwrap(),
            HighWaterPolicy::Block
        );
        assert_eq!(
            HighWaterPolicy::parse("resize").unwrap(),
            HighWaterPolicy::Resize
        );
        assert_eq!(
            HighWaterPolicy::parse("continue").unwrap(),
            HighWaterPolicy::Continue
        );
    }

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!(
            HighWaterPolicy::parse("  BLOCK  ").unwrap(),
            HighWaterPolicy::Block
        );
        assert_eq!(
            HighWaterPolicy::parse("Resize").unwrap(),
            HighWaterPolicy::Resize
        );
        assert_eq!(
            HighWaterPolicy::parse("CONTINUE").unwrap(),
            HighWaterPolicy::Continue
        );
    }

    #[test]
    fn parse_rejects_unknown_token() {
        let err = HighWaterPolicy::parse("yolo").unwrap_err();
        match err {
            HighWaterPolicyParseError::UnknownToken(t) => assert_eq!(t, "yolo"),
            other => panic!("expected UnknownToken, got {:?}", other),
        }
        let msg = HighWaterPolicy::parse("yolo").unwrap_err().to_string();
        assert!(msg.contains("block, resize, continue"));
    }

    #[test]
    fn as_str_round_trips() {
        for p in [
            HighWaterPolicy::Block,
            HighWaterPolicy::Resize,
            HighWaterPolicy::Continue,
        ] {
            assert_eq!(HighWaterPolicy::parse(p.as_str()).unwrap(), p);
        }
    }

    #[test]
    fn from_env_unset_returns_default() {
        // SAFETY: tests in this module are not run in parallel by default;
        // we restore the prior value at the end. Cargo defaults to a
        // shared environment per process.
        let prior = env::var("ECAA_AWS_HIGH_WATER_POLICY").ok();
        unsafe { env::remove_var("ECAA_AWS_HIGH_WATER_POLICY") };
        let got = HighWaterPolicy::from_env().unwrap();
        if let Some(v) = prior {
            unsafe { env::set_var("ECAA_AWS_HIGH_WATER_POLICY", v) };
        }
        assert_eq!(got, HighWaterPolicy::default());
    }
}
