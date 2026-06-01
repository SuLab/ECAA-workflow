//! Grant v19 §Authentication of Key Resources + §Aim 3A Arm B′ —
//! capture the determinism-relevant environment in effect at package
//! emit time. Emitted as `runtime/determinism-shim.json` by the
//! conversation crate's `emit::sidecars::write_determinism_shim`.
//!
//! The shim records:
//! - which `TZ`/`LANG`/`LC_ALL`/`PYTHONHASHSEED`/`SOURCE_DATE_EPOCH`
//!   env vars are set at emit time (values not captured for privacy,
//!   just presence + locale/timezone resolution);
//! - which secret env vars are present and redacted from the capture
//!   (recorded by name only — never values);
//! - the seed policy (SOURCE_DATE_EPOCH if set, else "process-default");
//! - the temp-path strategy + a DETERMINISTIC symbolic root. The root
//!   deliberately does NOT capture the host `$TMPDIR` (e.g.
//!   `/tmp/claude-1000`): that value is host-specific and would leak
//!   host state into the package, defeating the byte-reproducibility
//!   intent. The harness always places per-task scratch under the
//!   package-relative `runtime/scratch/<task_id>` regardless of host
//!   `$TMPDIR`, so the symbolic root is both accurate and deterministic;
//! - the active locale + timezone;
//! - the `ablation_engaged` flag mirroring
//!   [`crate::ablation::AblationFlag::ReexecutionClass`].

use crate::ablation::{AblationFlag, AblationFlagExt};
use serde::{Deserialize, Serialize};
use std::env;

/// Top-level payload for `runtime/determinism-shim.json`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeterminismShimSidecar {
    /// Schema version.
    pub schema_version: String,
    /// Env capture.
    pub env_capture: EnvCapture,
    /// Seed policy.
    pub seed_policy: SeedPolicy,
    /// Temp path policy.
    pub temp_path_policy: TempPathPolicy,
    /// Locale.
    pub locale: String,
    /// Timezone.
    pub timezone: String,
    /// Mirrors `ECAA_ABLATE_REEXECUTION_CLASS` per Subsystem B6.
    ///
    /// When `true`, the deterministic re-execution class is suppressed on
    /// the emit side (Arm B′ control). This bool flip is retained for
    /// backwards-compatibility and historical-session readers; the load-bearing
    /// suppression that empties `per_artifact` lives in
    /// `crates/conversation::emit::sidecars::write_reexecution_sidecar`.
    pub ablation_engaged: bool,
}

/// Determinism-relevant env vars: presence-captured (never value-captured)
/// + secret env vars marked as "redacted".
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EnvCapture {
    /// Captured env vars.
    pub captured_env_vars: Vec<String>,
    /// Redacted env vars.
    pub redacted_env_vars: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// SeedPolicy data.
pub struct SeedPolicy {
    /// Random seed.
    pub random_seed: Option<u64>,
    /// Seed source.
    pub seed_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// TempPathPolicy data.
pub struct TempPathPolicy {
    /// Strategy.
    pub strategy: String,
    /// Root.
    pub root: String,
}

const CAPTURED_ENV_VARS: &[&str] = &[
    "TZ",
    "LANG",
    "LC_ALL",
    "PYTHONHASHSEED",
    "SOURCE_DATE_EPOCH",
];

const REDACTED_ENV_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ECAA_ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
];

/// Snapshot the active determinism environment. Reads from `std::env`
/// — never opens a file or makes a network call. Pure with respect to
/// its env-var inputs.
pub fn serialize_active_settings() -> DeterminismShimSidecar {
    DeterminismShimSidecar {
        schema_version: "1".into(),
        env_capture: EnvCapture {
            captured_env_vars: CAPTURED_ENV_VARS
                .iter()
                .filter(|k| env::var(k).is_ok())
                .map(|k| (*k).to_string())
                .collect(),
            redacted_env_vars: REDACTED_ENV_VARS
                .iter()
                .filter(|k| env::var(k).is_ok())
                .map(|k| (*k).to_string())
                .collect(),
        },
        seed_policy: SeedPolicy {
            random_seed: env::var("SOURCE_DATE_EPOCH")
                .ok()
                .and_then(|v| v.parse().ok()),
            seed_source: if env::var("SOURCE_DATE_EPOCH").is_ok() {
                "SOURCE_DATE_EPOCH".into()
            } else {
                "process-default".into()
            },
        },
        temp_path_policy: TempPathPolicy {
            strategy: "stable-by-task-id".into(),
            // DETERMINISTIC by design: the harness scratch root is the
            // package-relative `runtime/scratch/<task_id>`, never the
            // host `$TMPDIR`. Capturing the literal `$TMPDIR`
            // (e.g. `/tmp/claude-1000`) would bake host state into the
            // sidecar and break byte-reproducibility, so we record the
            // symbolic package-relative root instead.
            root: "runtime/scratch".into(),
        },
        locale: env::var("LC_ALL")
            .or_else(|_| env::var("LANG"))
            .unwrap_or_else(|_| "C".into()),
        timezone: env::var("TZ").unwrap_or_else(|_| "UTC".into()),
        ablation_engaged: AblationFlag::ReexecutionClass.is_active(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shim_has_schema_v1() {
        let s = serialize_active_settings();
        assert_eq!(s.schema_version, "1");
    }

    #[test]
    fn temp_path_policy_is_stable_by_task_id() {
        let s = serialize_active_settings();
        assert_eq!(s.temp_path_policy.strategy, "stable-by-task-id");
    }

    #[test]
    #[serial_test::serial]
    fn temp_path_root_is_deterministic_and_does_not_leak_host_tmpdir() {
        // det-07 — the temp-path root must be a deterministic,
        // package-relative symbolic value, never the host `$TMPDIR`.
        // Set a bogus TMPDIR and assert the sidecar root ignores it.
        // `serial_test::serial` keeps this off the other env-mutating
        // suites; restore the prior value before returning.
        let prev = env::var("TMPDIR").ok();
        env::set_var("TMPDIR", "/tmp/claude-99999");
        let s = serialize_active_settings();
        assert_eq!(
            s.temp_path_policy.root, "runtime/scratch",
            "temp_path_policy.root must be the deterministic package-relative \
             scratch root, not the host $TMPDIR"
        );
        assert!(
            !s.temp_path_policy.root.contains("/tmp/"),
            "host $TMPDIR leaked into determinism-shim: {}",
            s.temp_path_policy.root
        );
        match prev {
            Some(v) => env::set_var("TMPDIR", v),
            None => env::remove_var("TMPDIR"),
        }
    }

    #[test]
    fn seed_source_defaults_to_process_default_when_source_date_epoch_unset() {
        // Pre-existing SOURCE_DATE_EPOCH would invalidate the
        // assertion — skip in that case rather than mutating env in a
        // non-serial test.
        if env::var("SOURCE_DATE_EPOCH").is_ok() {
            return;
        }
        let s = serialize_active_settings();
        assert_eq!(s.seed_policy.seed_source, "process-default");
        assert!(s.seed_policy.random_seed.is_none());
    }
}
