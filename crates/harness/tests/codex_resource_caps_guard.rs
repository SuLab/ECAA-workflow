//! Regression for critical-analysis M3/M4: the codex docker run must carry the
//! same resource fences as the claude path (memory/CPU), gate behind an
//! explicit experimental opt-in, translate the per-class budget into the
//! container, and wrap the run in a transient-error retry loop.
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
fn codex_run_has_memory_and_cpu_fences() {
    let s = codex_script();
    assert!(
        s.contains("DOCKER_MEMORY_ARGS"),
        "codex must build DOCKER_MEMORY_ARGS"
    );
    assert!(
        s.contains("DOCKER_CPU_ARGS"),
        "codex must build DOCKER_CPU_ARGS"
    );
    assert!(
        s.contains("${DOCKER_MEMORY_ARGS[@]}"),
        "codex docker run must apply DOCKER_MEMORY_ARGS"
    );
    assert!(
        s.contains("${DOCKER_CPU_ARGS[@]}"),
        "codex docker run must apply DOCKER_CPU_ARGS"
    );
}

#[test]
fn codex_requires_experimental_optin() {
    let s = codex_script();
    assert!(
        s.contains("ECAA_AGENT_CODEX_EXPERIMENTAL"),
        "codex must gate behind an explicit experimental opt-in"
    );
    // The gate must actually refuse to run (exit) when the opt-in is absent,
    // not merely mention the var.
    assert!(
        s.contains("ECAA_AGENT_CODEX_EXPERIMENTAL:-0") && s.contains("exit 2"),
        "codex must exit when ECAA_AGENT_CODEX_EXPERIMENTAL is not 1"
    );
}

#[test]
fn codex_wraps_run_in_retry_loop() {
    let s = codex_script();
    assert!(
        s.contains("run_codex_with_retries()"),
        "codex must define a run_codex_with_retries() wrapper"
    );
    assert!(
        s.contains("run_codex_with_retries docker run"),
        "codex docker run must be wrapped by run_codex_with_retries"
    );
}

#[test]
fn codex_translates_per_task_budget() {
    let s = codex_script();
    // Codex has no native --max-budget-usd; the per-class budget is passed in
    // as ECAA_TASK_BUDGET_USD for the task-execution contract to soft-enforce.
    assert!(
        s.contains("ECAA_TASK_BUDGET_USD"),
        "codex must translate the per-class budget into ECAA_TASK_BUDGET_USD"
    );
    assert!(
        s.contains("${CODEX_BUDGET_ENV_ARGS[@]}"),
        "codex docker run must apply the translated budget env"
    );
    // The class buckets must mirror the claude path.
    assert!(
        s.contains("ECAA_AGENT_BUDGET_USD_VALIDATE")
            && s.contains("ECAA_AGENT_BUDGET_USD_DISCOVER"),
        "codex budget must reuse the per-class budget buckets"
    );
}
