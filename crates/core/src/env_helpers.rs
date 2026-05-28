//! Workspace-wide env-var parsing helpers. Consolidates the 85+ scattered
//! `env::var(KEY).ok().and_then(|s| s.parse().ok()).unwrap_or(default)` sites.
//!
//! Pattern source: `crates/core/src/ablation.rs::read_flag` (the model citizen).
//!
//! Existing sites can migrate onto these helpers incrementally.

use std::env;
use std::fmt::Debug;
use std::str::FromStr;

/// Returns `true` if `v` (case-insensitive) is one of the canonical truthy
/// spellings: `1`, `true`, `yes`, `on`, `t`, `y`.
///
/// Mirrors the spelling table in `ablation::is_truthy` plus `t`/`y` (which
/// several site-local helpers around the workspace also accept).
pub fn is_truthy(v: &str) -> bool {
    matches!(
        v.to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on" | "t" | "y"
    )
}

/// `true` iff `key` is set in the environment to a truthy spelling.
///
/// Unset or empty-string is `false`.
pub fn env_bool(key: &str) -> bool {
    env::var(key).ok().map(|v| is_truthy(&v)).unwrap_or(false)
}

/// Like [`env_bool`] but returns `default` when the variable is unset.
///
/// Useful for flags whose "missing" semantics differ from "false".
pub fn env_bool_or(key: &str, default: bool) -> bool {
    env::var(key).ok().map(|v| is_truthy(&v)).unwrap_or(default)
}

/// W3.2 — strict env-bool parser. Like [`env_bool`] but emits a
/// `tracing::warn!` when the value is neither canonical truthy nor
/// canonical falsy. Unknown values fall back to `false`.
///
/// Use this for boolean env vars whose typos should surface in logs
/// (e.g. user-facing toggles). Reserve [`env_bool`] for cases where
/// any non-truthy value should silently fall back to false.
///
/// Canonical truthy: `1`, `true`, `yes`, `on`, `t`, `y` (case-insensitive).
/// Canonical falsy: `0`, `false`, `no`, `off`, `f`, `n`, `""` (case-insensitive).
pub fn env_bool_strict(key: &str) -> bool {
    let raw = match env::var(key) {
        Ok(v) => v,
        Err(_) => return false,
    };
    let lower = raw.to_ascii_lowercase();
    match lower.as_str() {
        "1" | "true" | "yes" | "on" | "t" | "y" => true,
        "0" | "false" | "no" | "off" | "f" | "n" | "" => false,
        _ => {
            tracing::warn!(
                env = key,
                value = %raw,
                "env_bool_strict: unrecognised boolean spelling; treating as false"
            );
            false
        }
    }
}

/// W3.2 — strict literal-`"1"` env-bool parser. Several harness flags
/// intentionally require the literal string `"1"` to opt in (e.g.
/// `SWFC_HARNESS_DEBUG_ALLOW_MULTI_PROCESS`, `SWFC_DISABLE_ENV_CLEAR`)
/// because they are debug bypasses where a typo silently disabling
/// safety would be worse than a typo silently NOT disabling it.
///
/// This helper exists so the literal-`"1"` semantics are named at the
/// call site instead of inlined as `matches!(..., Ok("1"))` ad hoc.
///
/// Unlike [`env_bool_strict`], non-`"1"` values do NOT warn — the
/// canonical falsy spelling for these debug flags is "anything but 1".
pub fn env_bool_literal_one(key: &str) -> bool {
    matches!(env::var(key).as_deref(), Ok("1"))
}

/// Parses `key` into `T` via [`FromStr`]; returns `default` if unset or parse
/// fails.
pub fn env_parse<T: FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

/// Parses `key` into `T` via [`FromStr`] and clamps to `[min, max]`.
///
/// Out-of-range values emit a `tracing::warn!` and are clamped to the nearest
/// endpoint; unset / unparseable inputs use `default` (which is *not*
/// clamp-checked — callers are expected to pass a `default` already in
/// range).
pub fn env_parse_clamped<T>(key: &str, default: T, min: T, max: T) -> T
where
    T: FromStr + PartialOrd + Copy + Debug,
{
    let v = env_parse(key, default);
    if v < min {
        tracing::warn!(key = %key, value = ?v, min = ?min, "env var below minimum; clamping");
        min
    } else if v > max {
        tracing::warn!(key = %key, value = ?v, max = ?max, "env var above maximum; clamping");
        max
    } else {
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::sync::Mutex;

    // `env::set_var` / `env::remove_var` are global state. `serial_test`
    // serializes these tests so they don't race with each other or with
    // the `ablation::tests` suite (which also mutates env vars under the
    // same serial guard).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_env<F: FnOnce()>(f: F) {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        f();
    }

    #[test]
    fn is_truthy_accepts_canonical_spellings() {
        for s in ["1", "true", "yes", "on", "t", "y"] {
            assert!(is_truthy(s), "expected truthy: {s}");
            assert!(is_truthy(&s.to_ascii_uppercase()), "expected truthy: {s}");
        }
        for s in ["0", "false", "no", "off", "", " ", "maybe", "2"] {
            assert!(!is_truthy(s), "expected falsy: {s}");
        }
    }

    #[test]
    #[serial]
    fn env_bool_false_when_unset() {
        with_env(|| {
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_BOOL_UNSET");
            assert!(!env_bool("SWFC_TEST_ENV_HELPERS_BOOL_UNSET"));
        });
    }

    #[test]
    #[serial]
    fn env_bool_true_when_set_to_one() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_BOOL_ON", "1");
            assert!(env_bool("SWFC_TEST_ENV_HELPERS_BOOL_ON"));
            std::env::set_var("SWFC_TEST_ENV_HELPERS_BOOL_ON", "true");
            assert!(env_bool("SWFC_TEST_ENV_HELPERS_BOOL_ON"));
            std::env::set_var("SWFC_TEST_ENV_HELPERS_BOOL_ON", "0");
            assert!(!env_bool("SWFC_TEST_ENV_HELPERS_BOOL_ON"));
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_BOOL_ON");
        });
    }

    #[test]
    #[serial]
    fn env_bool_or_returns_default_when_unset() {
        with_env(|| {
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_BOOL_OR");
            assert!(env_bool_or("SWFC_TEST_ENV_HELPERS_BOOL_OR", true));
            assert!(!env_bool_or("SWFC_TEST_ENV_HELPERS_BOOL_OR", false));
            std::env::set_var("SWFC_TEST_ENV_HELPERS_BOOL_OR", "no");
            assert!(!env_bool_or("SWFC_TEST_ENV_HELPERS_BOOL_OR", true));
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_BOOL_OR");
        });
    }

    #[test]
    #[serial]
    fn env_parse_falls_back_on_parse_failure() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_PARSE", "not-a-number");
            assert_eq!(env_parse::<u32>("SWFC_TEST_ENV_HELPERS_PARSE", 42), 42);
            std::env::set_var("SWFC_TEST_ENV_HELPERS_PARSE", "7");
            assert_eq!(env_parse::<u32>("SWFC_TEST_ENV_HELPERS_PARSE", 42), 7);
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_PARSE");
            assert_eq!(env_parse::<u32>("SWFC_TEST_ENV_HELPERS_PARSE", 42), 42);
        });
    }

    #[test]
    #[serial]
    fn env_parse_clamped_below_min() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_CLAMP", "1");
            let v = env_parse_clamped::<u32>("SWFC_TEST_ENV_HELPERS_CLAMP", 5, 10, 100);
            assert_eq!(v, 10);
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_CLAMP");
        });
    }

    #[test]
    #[serial]
    fn env_parse_clamped_above_max() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_CLAMP", "9999");
            let v = env_parse_clamped::<u32>("SWFC_TEST_ENV_HELPERS_CLAMP", 50, 10, 100);
            assert_eq!(v, 100);
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_CLAMP");
        });
    }

    #[test]
    #[serial]
    fn env_parse_clamped_within_range() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_CLAMP", "42");
            let v = env_parse_clamped::<u32>("SWFC_TEST_ENV_HELPERS_CLAMP", 50, 10, 100);
            assert_eq!(v, 42);
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_CLAMP");
        });
    }

    #[test]
    #[serial]
    fn env_parse_clamped_falls_back_to_default_when_unset() {
        with_env(|| {
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_CLAMP_UNSET");
            let v = env_parse_clamped::<u32>("SWFC_TEST_ENV_HELPERS_CLAMP_UNSET", 50, 10, 100);
            assert_eq!(v, 50);
        });
    }

    /// W3.2 — env_bool_strict accepts canonical truthy + falsy
    /// spellings; unknown spellings warn + fall back to false.
    #[test]
    #[serial]
    fn env_bool_strict_recognises_canonical_spellings() {
        with_env(|| {
            for truthy in ["1", "true", "yes", "on", "t", "y", "TRUE", "Yes"] {
                std::env::set_var("SWFC_TEST_ENV_HELPERS_STRICT", truthy);
                assert!(
                    env_bool_strict("SWFC_TEST_ENV_HELPERS_STRICT"),
                    "expected truthy: {truthy}"
                );
            }
            for falsy in ["0", "false", "no", "off", "f", "n", ""] {
                std::env::set_var("SWFC_TEST_ENV_HELPERS_STRICT", falsy);
                assert!(
                    !env_bool_strict("SWFC_TEST_ENV_HELPERS_STRICT"),
                    "expected falsy: {falsy}"
                );
            }
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_STRICT");
        });
    }

    #[test]
    #[serial]
    fn env_bool_strict_unrecognised_falls_back_false() {
        with_env(|| {
            for bad in ["maybe", "2", "tru", "Y3s", "enabled"] {
                std::env::set_var("SWFC_TEST_ENV_HELPERS_STRICT", bad);
                assert!(
                    !env_bool_strict("SWFC_TEST_ENV_HELPERS_STRICT"),
                    "unrecognised must fall back to false: {bad}"
                );
            }
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_STRICT");
        });
    }

    /// W3.2 — env_bool_literal_one accepts the literal string "1" and
    /// rejects everything else (including canonical truthy spellings
    /// like "true" and "yes").
    #[test]
    #[serial]
    fn env_bool_literal_one_only_accepts_1() {
        with_env(|| {
            std::env::set_var("SWFC_TEST_ENV_HELPERS_ONE", "1");
            assert!(env_bool_literal_one("SWFC_TEST_ENV_HELPERS_ONE"));
            for not_one in ["0", "true", "yes", "y", "01", " 1", "", "on"] {
                std::env::set_var("SWFC_TEST_ENV_HELPERS_ONE", not_one);
                assert!(
                    !env_bool_literal_one("SWFC_TEST_ENV_HELPERS_ONE"),
                    "literal-1 helper must reject {not_one:?}"
                );
            }
            std::env::remove_var("SWFC_TEST_ENV_HELPERS_ONE");
        });
    }
}
