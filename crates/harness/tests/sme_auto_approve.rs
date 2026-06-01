//! `ECAA_SME_AUTO_APPROVE_ALL=1` uniformly bypasses both SME gates for an
//! unattended (no-SME) run — the discover-review gate
//! (`scheduler::filter_picks_respecting_sme_gate`) and the post-completion
//! re-block guard (`sme_skip::detect_intent`).
//!
//! This lives in its OWN test binary (own process) because it mutates a
//! process-global env var; the in-crate unit tests for `detect_intent` /
//! `filter_picks_respecting_sme_gate` run in a separate process and are not
//! affected.

use ecaa_workflow_harness::sme_skip::{detect_intent, sme_auto_approve_all_env, SmeSkipIntent};
use std::path::Path;

#[test]
fn env_flag_bypasses_sme_gates() {
    // Start from a known-clean state.
    std::env::remove_var("ECAA_SME_AUTO_APPROVE_ALL");
    assert!(!sme_auto_approve_all_env(), "unset → no bypass");
    // No sme-decisions.json present anywhere → without the bypass this is None.
    assert_eq!(
        detect_intent(Path::new("/nonexistent/pkg"), "any_task"),
        SmeSkipIntent::None,
        "absent marker without bypass must be None"
    );

    // The flag short-circuits BEFORE any disk read.
    std::env::set_var("ECAA_SME_AUTO_APPROVE_ALL", "1");
    assert!(sme_auto_approve_all_env(), "=1 → bypass");
    assert_eq!(
        detect_intent(Path::new("/nonexistent/pkg"), "any_task"),
        SmeSkipIntent::EmitSentinel,
        "bypass must treat every task as a documented skip"
    );

    // Only the literal "1" enables the bypass.
    std::env::set_var("ECAA_SME_AUTO_APPROVE_ALL", "0");
    assert!(!sme_auto_approve_all_env(), "=0 → no bypass");
    std::env::set_var("ECAA_SME_AUTO_APPROVE_ALL", "true");
    assert!(
        !sme_auto_approve_all_env(),
        "=true → no bypass (must be \"1\")"
    );

    std::env::remove_var("ECAA_SME_AUTO_APPROVE_ALL");
}
