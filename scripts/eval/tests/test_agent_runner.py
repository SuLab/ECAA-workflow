import json
import os
import stat
from pathlib import Path
import pytest
from scripts.eval.services import agent_runner
from scripts.eval.services.agent_runner import run_bare, run_ecaa_package


def _make_stub(directory: Path, body: str) -> Path:
    """Write an executable stub shell script and return its path."""
    stub = directory / "stub-agent.sh"
    stub.write_text("#!/usr/bin/env bash\n" + body)
    stub.chmod(stub.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)
    return stub


def test_run_bare_writes_prompt_and_invokes_agent_script(tmp_path, monkeypatch):
    """run_bare writes PROMPT.md and delegates to the agent script stub."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    # Stub: emit some stdout, write trace.md + answer.txt into $1 (workdir).
    stub = _make_stub(
        bindir,
        'echo \'{"result":"ok"}\'\n'
        'printf "narr" > "$1/trace.md"\n'
        'printf "ans"  > "$1/answer.txt"\n',
    )

    wd = tmp_path / "wd"
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))

    res = run_bare(wd, "do the analysis")

    # PROMPT.md must contain the instruction.
    assert (wd / "PROMPT.md").read_text() == "do the analysis"
    # Stub must have created trace.md and answer.txt inside workdir.
    assert (wd / "trace.md").exists(), "stub should create trace.md"
    assert (wd / "answer.txt").exists(), "stub should create answer.txt"
    # RunResult fields.
    assert res.exit_ok is True
    assert res.run_dir == wd
    assert "ok" in res.stdout


def test_run_bare_forwards_env(tmp_path, monkeypatch):
    """run_bare threads the caller-provided env into the agent subprocess."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    stub = _make_stub(
        bindir,
        'echo "$EVAL_MARKER" > "$1/marker.txt"\n',
    )

    wd = tmp_path / "wd"
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))

    custom_env = os.environ.copy()
    custom_env["EVAL_MARKER"] = "hello_from_env_injection"

    res = run_bare(wd, "do the analysis", env=custom_env)

    assert res.exit_ok is True, "stub agent should exit 0"
    marker_file = wd / "marker.txt"
    assert marker_file.exists(), "stub should write marker.txt"
    assert marker_file.read_text().strip() == "hello_from_env_injection", (
        f"subprocess did not see injected env var; got: {marker_file.read_text()!r}"
    )


class _FakeProc:
    """Minimal subprocess.CompletedProcess stand-in: exit 0, no captured output."""
    returncode = 0
    stdout = None
    stderr = None


def test_run_ecaa_package_capture_default_is_off(tmp_path, monkeypatch):
    """Base runs must leave capture OFF so live console/session streaming stays
    intact: capture_output is falsy and RunResult.stdout is empty."""
    seen = {}

    def fake_run(cmd, **kw):
        seen["capture_output"] = kw.get("capture_output")
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)

    res = run_ecaa_package(tmp_path)

    assert not seen["capture_output"], "default run must not capture (live stream)"
    assert res.exit_ok is True
    assert res.stdout == "", "no captured output when capture is off"


def test_run_ecaa_package_capture_true_enables_capture(tmp_path, monkeypatch):
    """CONTRAST: error-matrix cells pass capture=True so the harness stdout/stderr
    is available as the reference exec.log — capture_output must flip True."""
    seen = {}

    def fake_run(cmd, **kw):
        seen["capture_output"] = kw.get("capture_output")
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)

    res = run_ecaa_package(tmp_path, capture=True)

    assert seen["capture_output"] is True, "capture=True must set capture_output"
    assert res.exit_ok is True


def test_blocked_guard_tasks_detects_blocked(tmp_path):
    """_blocked_guard_tasks returns (task_id, reason) for every WORKFLOW.json task
    the harness left in state.status=='blocked'; completed/ready tasks are ignored."""
    (tmp_path / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}},
        "survey_method_landscape": {"state": {"status": "blocked",
            "record": {"reason": "[validation_failed] 4/10 passed"}}},
    }}))
    assert agent_runner._blocked_guard_tasks(tmp_path) == [
        ("survey_method_landscape", "[validation_failed] 4/10 passed")]


def test_run_ecaa_package_single_shot_by_default(tmp_path, monkeypatch):
    """Default (ECAA_EVAL_MAX_RELAUNCH unset) stays single-shot even with a blocked
    task — preserves current behavior + the guard-outcome scoring."""
    monkeypatch.delenv("ECAA_EVAL_MAX_RELAUNCH", raising=False)
    (tmp_path / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "t": {"state": {"status": "blocked", "record": {"reason": "[validation_failed] x"}}}}}))
    calls = {"n": 0}

    def fake_run(cmd, **kw):
        calls["n"] += 1
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)
    res = run_ecaa_package(tmp_path)
    assert calls["n"] == 1, "default must not relaunch"
    assert not (tmp_path / "runtime/outputs/t/sme-decisions.json").exists()
    assert res.resolved_blocks == []


def test_run_ecaa_package_relaunches_and_resolves_guard_block(tmp_path, monkeypatch):
    """ECAA_EVAL_MAX_RELAUNCH>=1: after the harness exits with a guard-blocked task,
    write the skip sme-decisions.json, flip blocked->ready, and relaunch (bounded)."""
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "1")
    (tmp_path / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "survey_method_landscape": {"state": {"status": "blocked",
            "record": {"reason": "[validation_failed] 4/10 passed"}}}}}))
    calls = {"n": 0}

    def fake_run(cmd, **kw):
        calls["n"] += 1
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)
    res = run_ecaa_package(tmp_path)
    assert calls["n"] == 2, "should relaunch once after resolving the block"
    dec = json.loads((tmp_path / "runtime/outputs/survey_method_landscape/sme-decisions.json").read_text())
    assert dec["decisions"][0]["chosen"] == "emit_skip_sentinel_row"
    wf = json.loads((tmp_path / "WORKFLOW.json").read_text())
    assert wf["tasks"]["survey_method_landscape"]["state"]["status"] == "ready"
    assert res.resolved_blocks == ["survey_method_landscape"]


def test_run_ecaa_package_continues_incomplete_unblocked_dag(tmp_path, monkeypatch):
    """A harness that exits early (e.g. --max-iterations) with unblocked pending
    work — no blocked tasks — is relaunched to CONTINUE the DAG, as long as each
    launch makes forward progress (completes >=1 task)."""
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "8")
    wf = tmp_path / "WORKFLOW.json"
    wf.write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "pending"}},
        "b": {"state": {"status": "pending"}},
        "c": {"state": {"status": "pending"}},
    }}))
    calls = {"n": 0}
    order = ["a", "b", "c"]

    def fake_run(cmd, **kw):
        data = json.loads(wf.read_text())
        for tid in order:  # complete the next pending task each launch (progress)
            if data["tasks"][tid]["state"]["status"] == "pending":
                data["tasks"][tid]["state"] = {"status": "completed"}
                break
        wf.write_text(json.dumps(data))
        calls["n"] += 1
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)
    run_ecaa_package(tmp_path)
    assert calls["n"] == 3, "should relaunch to continue an incomplete unblocked DAG"


def test_run_ecaa_package_stops_when_no_progress_and_unblocked(tmp_path, monkeypatch):
    """An incomplete DAG with no blocked tasks and NO forward progress between
    launches is treated as wedged: stop after the second launch rather than
    spinning to the relaunch cap."""
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "8")
    wf = tmp_path / "WORKFLOW.json"
    wf.write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}},
        "b": {"state": {"status": "pending"}},  # never advances
    }}))
    calls = {"n": 0}

    def fake_run(cmd, **kw):  # never mutates state -> no progress
        calls["n"] += 1
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)
    run_ecaa_package(tmp_path)
    assert calls["n"] == 2, "no-progress unblocked DAG must stop, not spin"


def test_run_bare_sets_default_container_image(tmp_path, monkeypatch):
    """run_bare supplies ECAA_DEFAULT_CONTAINER_IMAGE=bio-min:local when not set."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    stub = _make_stub(
        bindir,
        'echo "$ECAA_DEFAULT_CONTAINER_IMAGE" > "$1/image.txt"\n',
    )

    wd = tmp_path / "wd"
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))
    # Ensure the var is absent from the environment the stub inherits.
    monkeypatch.delenv("ECAA_DEFAULT_CONTAINER_IMAGE", raising=False)

    # Pass env=None so run_bare copies os.environ (which now lacks the var).
    res = run_bare(wd, "do the analysis", env=None)

    assert res.exit_ok is True
    image_file = wd / "image.txt"
    assert image_file.exists(), "stub should write image.txt"
    assert image_file.read_text().strip() == "bio-min:local", (
        f"expected bio-min:local default; got: {image_file.read_text()!r}"
    )
