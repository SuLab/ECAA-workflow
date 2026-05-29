//! Regression test for the ECAA_TASK_ID-based model tier selector.
//!
//! `scripts/agent-claude.sh` routes the agent's `claude` invocation to
//! Opus or Sonnet based on the task being dispatched:
//!   - discover_* / validate_* → Opus (high-judgment buckets + QC safety net)
//!   - everything else         → Sonnet (well-bounded code execution)
//!
//! A peek-at-first-ready-task selector would mis-route a
//! `validate_*` invocation to Sonnet when multiple tasks are
//! concurrently dispatchable. The script must instead read
//! ECAA_TASK_ID directly, which the harness sets to the specific
//! task it's dispatching before each agent invocation. This test
//! exercises the script's model-selection logic for the four
//! cases the case statement covers.

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root must be two levels above CARGO_MANIFEST_DIR")
        .to_path_buf()
}

/// Run a bash one-liner that replicates the model-selection block from
/// agent-claude.sh. We can't source agent-claude.sh directly because it
/// runs the full agent pipeline; instead we extract the case statement
/// into an isolated invocation and assert the selected model.
fn pick_model(task_id: &str, tier_enabled: &str) -> String {
    let cmd = format!(
        r#"
ECAA_AGENT_MODEL_TIER={tier} ECAA_TASK_ID='{tid}' bash -c '
MODEL_FLAG_ARGS=()
if [ "${{ECAA_AGENT_MODEL_TIER:-1}}" = "1" ] && [ -n "${{ECAA_TASK_ID:-}}" ]; then
  case "$ECAA_TASK_ID" in
    discover_*|validate_*)
      MODEL_FLAG_ARGS+=(--model claude-opus-4-7)
      ;;
    *)
      MODEL_FLAG_ARGS+=(--model claude-sonnet-4-6)
      ;;
  esac
fi
echo "${{MODEL_FLAG_ARGS[@]:-NONE}}"
'
"#,
        tier = tier_enabled,
        tid = task_id,
    );
    let out = Command::new("bash")
        .arg("-c")
        .arg(&cmd)
        .output()
        .expect("bash -c must run");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn discover_tasks_route_to_opus() {
    assert_eq!(
        pick_model("discover_normalisation", "1"),
        "--model claude-opus-4-7",
        "discover_* must use Opus (method-selection is high-judgment)",
    );
    assert_eq!(
        pick_model("discover_differential_expression", "1"),
        "--model claude-opus-4-7",
    );
    assert_eq!(
        pick_model("discover_pathway_enrichment", "1"),
        "--model claude-opus-4-7",
    );
}

#[test]
fn validate_tasks_route_to_opus() {
    assert_eq!(
        pick_model("validate_qc_preprocessing", "1"),
        "--model claude-opus-4-7",
        "validate_* must use Opus (QC safety net catching Sonnet mistakes)",
    );
    assert_eq!(
        pick_model("validate_normalisation", "1"),
        "--model claude-opus-4-7",
    );
    assert_eq!(
        pick_model("validate_final_reporting", "1"),
        "--model claude-opus-4-7",
    );
}

#[test]
fn analytical_compute_tasks_route_to_sonnet() {
    assert_eq!(
        pick_model("normalisation", "1"),
        "--model claude-sonnet-4-6",
        "compute tasks use Sonnet — method already chosen by upstream discover_*",
    );
    assert_eq!(
        pick_model("differential_expression", "1"),
        "--model claude-sonnet-4-6",
    );
    assert_eq!(pick_model("clustering", "1"), "--model claude-sonnet-4-6",);
    assert_eq!(
        pick_model("data_acquisition", "1"),
        "--model claude-sonnet-4-6",
    );
    assert_eq!(pick_model("reporting", "1"), "--model claude-sonnet-4-6",);
    assert_eq!(
        pick_model("final_reporting", "1"),
        "--model claude-sonnet-4-6",
    );
}

#[test]
fn tier_disabled_emits_no_model_flag() {
    // ECAA_AGENT_MODEL_TIER=0 reverts to the CLI default (Opus). The
    // script must emit no --model flag so Claude Code picks its own
    // default rather than being pinned by our routing.
    assert_eq!(
        pick_model("normalisation", "0"),
        "NONE",
        "tier off must emit zero model flags",
    );
    assert_eq!(pick_model("discover_normalisation", "0"), "NONE",);
    assert_eq!(pick_model("validate_normalisation", "0"), "NONE",);
}

#[test]
fn empty_task_id_is_a_noop() {
    // Empty ECAA_TASK_ID means the harness hasn't actually dispatched
    // a task (e.g. a probe invocation before dispatch). The selector
    // must short-circuit cleanly without injecting a flag.
    assert_eq!(
        pick_model("", "1"),
        "NONE",
        "empty ECAA_TASK_ID must emit zero model flags",
    );
}

/// Belt-and-braces: assert the production agent-claude.sh contains the
/// ECAA_TASK_ID-based selector rather than the older peek-first heuristic.
/// This catches a regression where someone reverts to the broken
/// peek-at-first-ready logic.
#[test]
fn script_uses_task_id_not_peek() {
    let script_path = workspace_root().join("scripts/agent-claude.sh");
    let body = std::fs::read_to_string(&script_path)
        .expect("agent-claude.sh must exist at workspace root");

    assert!(
        body.contains(r#"case "$ECAA_TASK_ID" in"#),
        "model selector must dispatch on ECAA_TASK_ID directly",
    );
    // The old peek heuristic used PEEK_TID as the variable name; if it
    // creeps back into the selector block, fail.
    let selector_block = body
        .split("MODEL_FLAG_ARGS=()")
        .nth(1)
        .expect("MODEL_FLAG_ARGS block must exist")
        .split("# Host-path memory cap")
        .next()
        .expect("selector block must be bounded by Host-path memory cap comment");
    assert!(
        !selector_block.contains("PEEK_TID"),
        "model selector must not peek at WORKFLOW.json — use ECAA_TASK_ID. \
         Found PEEK_TID in selector block.",
    );
}
