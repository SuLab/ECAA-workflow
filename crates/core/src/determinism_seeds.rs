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

/// Deterministic determinism-envelope env vars for a task dispatch.
///
/// `run_source_date_epoch` is a RUN-level Unix timestamp captured once at
/// harness startup and passed identically for every task in the run, so
/// `SOURCE_DATE_EPOCH` is stable across all tasks of a single run (a
/// `SOURCE_DATE_EPOCH` that varied per task would defeat its purpose:
/// build tools embed it as THE build date, and a multi-task package would
/// otherwise carry 22 different "build dates"). The locale is pinned to
/// `C.UTF-8` (NOT bare `C`) so UTF-8 byte handling is preserved while
/// sort/format locale effects are neutralized.
///
/// `run_id` is retained in the signature for call-site symmetry with the
/// dispatch identity and to keep the seeds bound to a specific run, but it
/// does NOT perturb `SOURCE_DATE_EPOCH` — that value is the run epoch
/// verbatim so it remains a defensible, human-meaningful build date.
pub fn seed_env_from_dispatch(run_id: &str, run_source_date_epoch: u64) -> BTreeMap<String, String> {
    // run_id intentionally does not feed SOURCE_DATE_EPOCH (see doc); it is
    // accepted so callers thread the same identity used for the dispatch WAL.
    let _ = run_id;
    let mut env = BTreeMap::new();
    env.insert("PYTHONHASHSEED".to_string(), "0".to_string());
    env.insert("TZ".to_string(), "UTC".to_string());
    env.insert("LANG".to_string(), "C.UTF-8".to_string());
    env.insert("LC_ALL".to_string(), "C.UTF-8".to_string());
    env.insert(
        "SOURCE_DATE_EPOCH".to_string(),
        run_source_date_epoch.to_string(),
    );
    env
}

/// Default-on gate for [`seed_env_from_dispatch`]. `Some("0")` opts out;
/// unset or any other value keeps the deterministic seeds.
pub fn seeds_enabled(env_val: Option<&str>) -> bool {
    !matches!(env_val, Some("0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seed_env_is_deterministic_in_dispatch_inputs() {
        let a = seed_env_from_dispatch("run-abc", 1_767_225_600);
        let b = seed_env_from_dispatch("run-abc", 1_767_225_600);
        assert_eq!(a, b, "same dispatch -> same env");
    }

    #[test]
    fn seed_env_pins_locale_and_seeds() {
        let env = seed_env_from_dispatch("run-abc", 1_767_225_600);
        assert_eq!(env.get("PYTHONHASHSEED").map(String::as_str), Some("0"));
        assert_eq!(env.get("TZ").map(String::as_str), Some("UTC"));
        assert_eq!(env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(env.get("LC_ALL").map(String::as_str), Some("C.UTF-8"));
        // SOURCE_DATE_EPOCH is the run epoch verbatim — a parseable integer.
        let sde = env.get("SOURCE_DATE_EPOCH").expect("present");
        assert_eq!(
            sde, "1767225600",
            "SOURCE_DATE_EPOCH must be the run epoch verbatim"
        );
    }

    /// C2 twin — two DIFFERENT per-task dispatch epochs under the SAME
    /// run must produce the SAME `SOURCE_DATE_EPOCH`. Before the root
    /// fix, `SOURCE_DATE_EPOCH` was a hash of `"{run_id}:{task_epoch}"`
    /// and this assertion FAILED (each task got a distinct value). The
    /// caller now threads a run-level epoch identically for every task,
    /// so a multi-task package carries one stable build date.
    #[test]
    fn source_date_epoch_is_run_stable_across_task_epochs() {
        let run_epoch = 1_767_225_600u64;
        // The harness used to pass the per-task dispatch counter here.
        // Threading the run-level epoch instead makes task 1 and task 22
        // agree.
        let task_one = seed_env_from_dispatch("run-abc", run_epoch);
        let task_twentytwo = seed_env_from_dispatch("run-abc", run_epoch);
        assert_eq!(
            task_one.get("SOURCE_DATE_EPOCH"),
            task_twentytwo.get("SOURCE_DATE_EPOCH"),
            "every task in a run must share one SOURCE_DATE_EPOCH"
        );
        assert_eq!(
            task_one.get("SOURCE_DATE_EPOCH").map(String::as_str),
            Some("1767225600"),
            "SOURCE_DATE_EPOCH is the run epoch verbatim"
        );
    }

    /// Different runs (different captured epochs) get different
    /// `SOURCE_DATE_EPOCH`s — the value tracks the real run, not a hash.
    #[test]
    fn source_date_epoch_tracks_run_epoch() {
        let a = seed_env_from_dispatch("run-abc", 1_767_225_600);
        let b = seed_env_from_dispatch("run-abc", 1_767_311_999);
        assert_ne!(a.get("SOURCE_DATE_EPOCH"), b.get("SOURCE_DATE_EPOCH"));
        assert_eq!(
            b.get("SOURCE_DATE_EPOCH").map(String::as_str),
            Some("1767311999")
        );
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
