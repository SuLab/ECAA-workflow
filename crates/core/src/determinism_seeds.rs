//! Deterministic seed/locale env derived from the stamped dispatch
//! identity. The harness calls [`seed_env_from_dispatch`] at the
//! per-task env-stamp seam so the execution container runs with a
//! controlled, run-id-derived determinism envelope.
//!
//! NO `SystemTime::now()` — `SOURCE_DATE_EPOCH` is a deterministic
//! function of the run id + dispatch epoch, so re-stamping the same
//! dispatch yields byte-identical env. The harness is the source of
//! truth; the agent wrapper never invents these.

use std::collections::BTreeMap;

/// Deterministic determinism-envelope env vars for a dispatch.
///
/// `SOURCE_DATE_EPOCH` is derived from a stable FNV-1a hash of
/// `"{run_id}:{epoch}"`, mapped into a fixed window (so it stays a
/// plausible Unix timestamp but is fully reproducible from inputs and
/// never reads the wall clock). The locale is pinned to `C.UTF-8`
/// (NOT bare `C`) so UTF-8 byte handling is preserved while sort/format
/// locale effects are neutralized.
pub fn seed_env_from_dispatch(run_id: &str, epoch: u64) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PYTHONHASHSEED".to_string(), "0".to_string());
    env.insert("TZ".to_string(), "UTC".to_string());
    env.insert("LANG".to_string(), "C.UTF-8".to_string());
    env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
    let seed = fnv1a(format!("{run_id}:{epoch}").as_bytes());
    // Map into a stable window anchored at a fixed epoch base so the
    // value is a plausible timestamp without ever touching SystemTime.
    const BASE: u64 = 1_700_000_000; // fixed anchor, not wall-clock
    let sde = BASE + (seed % 100_000_000);
    env.insert("SOURCE_DATE_EPOCH".to_string(), sde.to_string());
    env
}

/// Default-on gate for [`seed_env_from_dispatch`]. `Some("0")` opts out;
/// unset or any other value keeps the deterministic seeds.
pub fn seeds_enabled(env_val: Option<&str>) -> bool {
    !matches!(env_val, Some("0"))
}

/// FNV-1a 64-bit — small, dependency-free, deterministic.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_env_is_deterministic_in_dispatch_inputs() {
        let a = seed_env_from_dispatch("run-abc", 7);
        let b = seed_env_from_dispatch("run-abc", 7);
        assert_eq!(a, b, "same dispatch -> same env");
    }

    #[test]
    fn seed_env_pins_locale_and_seeds() {
        let env = seed_env_from_dispatch("run-abc", 7);
        assert_eq!(env.get("PYTHONHASHSEED").map(String::as_str), Some("0"));
        assert_eq!(env.get("TZ").map(String::as_str), Some("UTC"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(env.get("LC_ALL").map(String::as_str), Some("C.UTF-8"));
        // SOURCE_DATE_EPOCH is a parseable integer derived from the run id.
        let sde = env.get("SOURCE_DATE_EPOCH").expect("present");
        assert!(
            sde.parse::<u64>().is_ok(),
            "SOURCE_DATE_EPOCH must be an integer"
        );
    }

    #[test]
    fn source_date_epoch_changes_with_run_id() {
        let a = seed_env_from_dispatch("run-abc", 7);
        let b = seed_env_from_dispatch("run-xyz", 7);
        assert_ne!(a.get("SOURCE_DATE_EPOCH"), b.get("SOURCE_DATE_EPOCH"));
    }

    #[test]
    fn enabled_by_default_and_off_only_on_zero() {
        // default-on knob: unset / "1" / "true" => enabled; "0" => off.
        assert!(seeds_enabled(None));
        assert!(seeds_enabled(Some("1")));
        assert!(seeds_enabled(Some("true")));
        assert!(!seeds_enabled(Some("0")));
    }
}
