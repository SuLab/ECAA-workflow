//! Reads the `SWFC_AWS_SPOT` env var and returns whether
//! `AwsExecutor::provision` should request spot capacity with
//! `CapacityRebalance: true`. Defaults to on-demand (safe default)
//! when unset.

use std::env;

/// Returns `true` when `SWFC_AWS_SPOT` is set to a truthy value,
/// `false` when unset or set to a falsy value. Unrecognized values
/// log a warning and fall back to on-demand so operators don't
/// accidentally start a spot run from a typo.
pub fn is_spot_requested() -> bool {
    match env::var("SWFC_AWS_SPOT") {
        Ok(raw) => parse(&raw).unwrap_or(false),
        Err(_) => false,
    }
}

/// P0-43 — value passed to `aws ec2 run-instances
/// --instance-market-options` when `SWFC_AWS_SPOT=1`. Encodes the
/// full spot envelope the harness wants:
///
/// * `SpotInstanceType=persistent` — AWS keeps the spot request open
///   after interruption; the same logical request is re-fulfilled
///   when capacity returns. The legacy default (`one-time`) drops
///   the request on first reclaim, which is wrong for the
///   single-instance-per-task model.
/// * `InstanceInterruptionBehavior=stop` — instead of terminating
///   the host on reclaim, AWS stops it. The EBS root volume and any
///   cached container images survive, so the next `ensure_alive`
///   start-instance reattaches the same state. Saves ~5 min per
///   reclaim on container-heavy workloads.
///
/// CapacityRebalance is monitored client-side in
/// `provisioning::do_ensure_alive` because single-instance
/// `run-instances` cannot subscribe to AWS's proactive rebalance
/// notification stream — that surface is gated behind ASG / EC2
/// Fleet. The describe-instances poll watches for
/// `instance-rebalance-recommendation` events and triggers
/// release+reprovision pre-emptively.
pub fn spot_market_options_arg() -> &'static str {
    "MarketType=spot,SpotOptions={SpotInstanceType=persistent,InstanceInterruptionBehavior=stop}"
}

fn parse(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "t" | "yes" | "y" => Some(true),
        "0" | "false" | "f" | "no" | "n" | "" => Some(false),
        other => {
            eprintln!(
                "[spot_policy] SWFC_AWS_SPOT='{}' not recognized (expected true/false); falling back to on-demand",
                other
            );
            None
        }
    }
}

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
    fn parse_accepts_truthy_forms() {
        for v in ["1", "true", "TRUE", "True", "yes", "Y"] {
            assert_eq!(parse(v), Some(true), "failed for {}", v);
        }
    }

    #[test]
    fn parse_accepts_falsy_forms() {
        for v in ["0", "false", "FALSE", "no", "N", ""] {
            assert_eq!(parse(v), Some(false), "failed for {}", v);
        }
    }

    #[test]
    fn parse_rejects_unknown_token() {
        assert_eq!(parse("maybe"), None);
        assert_eq!(parse("on"), None); // intentionally not accepted — keeps the surface small
    }

    fn with_spot<T>(value: Option<&str>, body: impl FnOnce() -> T) -> T {
        let prior = env::var("SWFC_AWS_SPOT").ok();
        match value {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SPOT", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SPOT") },
        }
        let out = body();
        match prior {
            Some(v) => unsafe { env::set_var("SWFC_AWS_SPOT", v) },
            None => unsafe { env::remove_var("SWFC_AWS_SPOT") },
        }
        out
    }

    #[test]
    fn is_spot_requested_unset_is_false() {
        with_spot(None, || {
            assert!(!is_spot_requested());
        });
    }

    #[test]
    fn is_spot_requested_true_is_true() {
        with_spot(Some("true"), || {
            assert!(is_spot_requested());
        });
    }

    #[test]
    fn is_spot_requested_unknown_value_is_false() {
        with_spot(Some("bogus"), || {
            assert!(!is_spot_requested());
        });
    }

    #[test]
    fn spot_market_options_arg_carries_persistent_and_stop() {
        let arg = spot_market_options_arg();
        assert!(arg.contains("MarketType=spot"), "arg must request spot");
        assert!(
            arg.contains("SpotInstanceType=persistent"),
            "spot request must be persistent so AWS re-fulfills after reclaim"
        );
        assert!(
            arg.contains("InstanceInterruptionBehavior=stop"),
            "interruption must stop (not terminate) so EBS + container cache survive"
        );
    }
}
