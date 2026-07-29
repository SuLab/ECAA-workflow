import json
import subprocess
from pathlib import Path


def run_common_helper(script: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", "-lc", f"source scripts/agent-claude-common.sh\n{script}"],
        cwd=".",
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_success_log(path):
    payload = {
        "type": "result",
        "subtype": "success",
        "is_error": False,
        "terminal_reason": "completed",
        "num_turns": 29,
        "total_cost_usd": 0.5,
        "modelUsage": {
            "claude-haiku-4-5-20251001": {
                "inputTokens": 100,
                "outputTokens": 2,
                "cacheReadInputTokens": 0,
                "cacheCreationInputTokens": 0,
                "costUSD": 0.01,
            },
            "claude-sonnet-4-6[200k]": {
                "inputTokens": 27,
                "outputTokens": 11162,
                "cacheReadInputTokens": 996931,
                "cacheCreationInputTokens": 34896,
                "costUSD": 0.49,
            },
        },
    }
    path.write_text(json.dumps(payload) + "\n")


def write_error_log(path, message):
    payload = {
        "type": "result",
        "subtype": "success",
        "is_error": True,
        "terminal_reason": "completed",
        "result": message,
    }
    path.write_text(json.dumps(payload) + "\n")


def test_success_result_with_state_patch_normalizes_nonzero_exit(tmp_path):
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "data_acquisition"
    task_dir.mkdir(parents=True)
    (task_dir / "state.patch.json").write_text('{"from":"running","to":{"status":"completed"}}')
    log = task_dir / "agent-claude.log"
    write_success_log(log)

    result = run_common_helper(
        f'normalize_claude_exit_status 2 "{log}" "{package}" data_acquisition'
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "0"


def test_success_result_without_state_patch_keeps_nonzero_exit(tmp_path):
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "data_acquisition"
    task_dir.mkdir(parents=True)
    log = task_dir / "agent-claude.log"
    write_success_log(log)

    result = run_common_helper(
        f'normalize_claude_exit_status 2 "{log}" "{package}" data_acquisition'
    )

    assert result.returncode == 0, result.stderr
    assert result.stdout.strip() == "2"


def test_transient_socket_api_error_is_retryable(tmp_path):
    log = tmp_path / "agent-claude.log"
    write_error_log(
        log,
        "API Error: The socket connection was closed unexpectedly. "
        "For more information, pass `verbose: true` in the second argument to fetch()",
    )

    result = run_common_helper(f'claude_terminal_result_transient_error "{log}"')

    assert result.returncode == 0, result.stderr


def test_non_transport_agent_error_is_not_retryable(tmp_path):
    log = tmp_path / "agent-claude.log"
    write_error_log(log, "Python validation failed: row_count mismatch")

    result = run_common_helper(f'claude_terminal_result_transient_error "{log}"')

    assert result.returncode == 1


def test_agent_usage_sidecar_preserves_num_turns(tmp_path):
    log = tmp_path / "agent-claude.log"
    write_success_log(log)

    result = run_common_helper(
        f'grep -E "^{{" "{log}" | tail -1 | agent_usage_json_from_claude_result'
    )

    assert result.returncode == 0, result.stderr
    usage = json.loads(result.stdout)
    assert usage["model"] == "claude-sonnet-4-6"
    assert usage["num_turns"] == 29
    assert usage["cache_read_tokens"] == 996931


def test_task_execution_prompt_renders_turn_budget_placeholders():
    result = run_common_helper(
        "PACKAGE=/tmp/pkg ECAA_TASK_ID=data_acquisition "
        "MAX_TURNS_PER_TASK=25 "
        "load_task_execution_prompt scripts/agent-prompts/task-execution.md"
    )

    assert result.returncode == 0, result.stderr
    assert "budget of 25 turns per task" in result.stdout
    assert "going past\n20" in result.stdout
    assert "{{MAX_TURNS_PER_TASK}}" not in result.stdout
    assert "{{SOFT_TURNS_PER_TASK}}" not in result.stdout


def test_turn_budget_enforcement_respects_completed_state_patch(tmp_path):
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "data_acquisition"
    task_dir.mkdir(parents=True)
    (task_dir / "agent-usage.json").write_text(
        json.dumps({"num_turns": 42}) + "\n"
    )
    (task_dir / "result.json").write_text(
        json.dumps({"task_id": "data_acquisition", "status": "completed"})
    )
    (task_dir / "state.patch.json").write_text(
        json.dumps({"from": "running", "to": {"status": "completed"}})
    )

    result = run_common_helper(
        "ECAA_HARNESS_RUN_ID=run123 ECAA_DISPATCH_EPOCH=7 "
        f'enforce_turn_budget_limit "{package}" data_acquisition 25'
    )

    assert result.returncode == 0, result.stderr
    task_result = json.loads((task_dir / "result.json").read_text())
    patch = json.loads((task_dir / "state.patch.json").read_text())
    assert task_result["status"] == "completed"
    assert patch["to"]["status"] == "completed"


def test_turn_budget_respects_passing_validation_report_without_patch(tmp_path):
    # Budget cut hit AFTER a validate task wrote a passing validation_report.json
    # but BEFORE it wrote a terminal state.patch.json. The work passed (28/28),
    # so enforcement must complete it, not block it on TurnBudgetExceeded.
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "validate_pathway_enrichment"
    task_dir.mkdir(parents=True)
    (task_dir / "agent-usage.json").write_text(json.dumps({"num_turns": 29}) + "\n")
    (task_dir / "validation_report.json").write_text(
        json.dumps({"overall_result": "PASS", "n_checks": 28, "n_pass": 28, "n_fail": 0})
    )
    # No state.patch.json, no result.json — the budget cut them off.

    result = run_common_helper(
        "ECAA_HARNESS_RUN_ID=run9 ECAA_DISPATCH_EPOCH=13 "
        f'enforce_turn_budget_limit "{package}" validate_pathway_enrichment 25'
    )

    assert result.returncode == 0, result.stderr
    patch = json.loads((task_dir / "state.patch.json").read_text())
    assert patch["to"]["status"] == "completed", patch
    assert patch.get("dispatch_epoch") == 13
    task_result = json.loads((task_dir / "result.json").read_text())
    assert task_result["status"] == "completed", task_result


def test_turn_budget_respects_completed_result_json_without_patch(tmp_path):
    # Agent wrote result.json status=completed but ran out of budget before the
    # state.patch.json. Enforcement should synthesize a completed patch and keep
    # the agent's result, not overwrite it with a blocked one.
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "normalisation"
    task_dir.mkdir(parents=True)
    (task_dir / "agent-usage.json").write_text(json.dumps({"num_turns": 63}) + "\n")
    (task_dir / "result.json").write_text(
        json.dumps({"task_id": "normalisation", "status": "completed",
                    "method": "deseq2_vst", "figures": ["figures/mean_variance.png"]})
    )

    result = run_common_helper(
        f'enforce_turn_budget_limit "{package}" normalisation 25'
    )

    assert result.returncode == 0, result.stderr
    patch = json.loads((task_dir / "state.patch.json").read_text())
    assert patch["to"]["status"] == "completed", patch
    task_result = json.loads((task_dir / "result.json").read_text())
    assert task_result["status"] == "completed"
    # the agent's own result.json must be preserved, not clobbered to a stub
    assert task_result.get("method") == "deseq2_vst", task_result


def test_turn_budget_blocks_when_incomplete_and_no_success_signal(tmp_path):
    # Genuinely incomplete: over cap, no completed patch, no passing result or
    # validation report. The safety net must still block.
    package = tmp_path / "pkg"
    task_dir = package / "runtime" / "outputs" / "differential_expression"
    task_dir.mkdir(parents=True)
    (task_dir / "agent-usage.json").write_text(json.dumps({"num_turns": 60}) + "\n")
    (task_dir / "result.json").write_text(
        json.dumps({"task_id": "differential_expression", "status": "running"})
    )

    result = run_common_helper(
        f'enforce_turn_budget_limit "{package}" differential_expression 25'
    )

    assert result.returncode == 0, result.stderr
    patch = json.loads((task_dir / "state.patch.json").read_text())
    assert patch["to"]["status"] == "blocked", patch
    task_result = json.loads((task_dir / "result.json").read_text())
    assert task_result["status"] == "blocked"
    assert task_result["blocker_kind"] == "TurnBudgetExceeded"


def test_docker_api_key_not_embedded_in_process_argv():
    script = Path("scripts/agent-claude.sh").read_text()

    assert "--env-file" in script
    assert 'ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY' not in script
    assert '-e "ANTHROPIC_API_KEY=' not in script


def test_uploaded_inputs_are_previewed_and_bound_for_ingestion():
    script = Path("scripts/agent-claude.sh").read_text()
    both_input_kinds = (
        'select(.kind == "local_path" or .kind == "uploaded_files")'
    )

    assert script.count(both_input_kinds) == 2
    assert "DOCKER_INPUT_BIND_ARGS" in script
    assert "Registered input tables (header row = schema)" in script


def test_render_container_uses_writable_plot_cache_paths():
    script = Path("scripts/agent-claude-common.sh").read_text()

    assert 'local container_render_home="/tmp"' in script
    assert 'local container_render_xdg_cache="/tmp/ecaa-render-$task_id-xdg"' in script
    assert (
        'local container_render_mpl_config="/tmp/ecaa-render-$task_id-matplotlib"'
        in script
    )
    assert '-e "HOME=$container_render_home"' in script
    assert '-e "XDG_CACHE_HOME=$container_render_xdg_cache"' in script
    assert '-e "MPLCONFIGDIR=$container_render_mpl_config"' in script
