import os
import stat
from pathlib import Path
import pytest
from scripts.eval.services.agent_runner import run_bare


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
