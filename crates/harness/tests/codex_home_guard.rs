//! Regression guard for critical-analysis C1: the codex agent HOME default
//! must NOT live inside the emitted package ($PACKAGE), because the ChatGPT
//! OAuth token is copied into $AGENT_HOME_DIR/.codex/auth.json. Keeping it
//! out of the package means it is never served by the artifact route nor
//! staged by the provenance `git add -A`.
//!
//! This is a script-shape test (grep the wrapper), mirroring the style of
//! `tests/misc/model_tier_routing.rs::script_uses_task_id_not_peek`.

fn codex_script() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../scripts/agent-codex.sh"
    ))
    .expect("read agent-codex.sh")
}

#[test]
fn codex_agent_home_default_is_outside_package_root() {
    let script = codex_script();
    // The old, vulnerable default. Must be gone.
    assert!(
        !script.contains("$PACKAGE/runtime/agent-home"),
        "codex AGENT_HOME_DIR must not default under the package root \
         ($PACKAGE/runtime/agent-home)"
    );
    // The new default must reference an out-of-package cache location.
    assert!(
        script.contains("agent-codex-home"),
        "codex AGENT_HOME_DIR should default to an out-of-package cache dir \
         (agent-codex-home)"
    );
}

#[test]
fn codex_agent_home_honors_explicit_override() {
    let script = codex_script();
    // The ECAA_AGENT_HOME_DIR override must still be honored.
    assert!(
        script.contains("ECAA_AGENT_HOME_DIR"),
        "codex AGENT_HOME_DIR must still honor the ECAA_AGENT_HOME_DIR override"
    );
}

#[test]
fn codex_agent_home_falls_back_to_session_or_agent_cache() {
    let script = codex_script();
    // Mirrors agent-claude.sh's placement: session-cache → agent-cache fallback.
    assert!(
        script.contains("ECAA_SESSION_CACHE_DIR"),
        "codex AGENT_HOME_DIR should prefer the per-session cache dir when set"
    );
    assert!(
        script.contains("ECAA_AGENT_CACHE_DIR"),
        "codex AGENT_HOME_DIR should fall back to the agent cache dir"
    );
}
