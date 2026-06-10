import subprocess

from scripts.eval.services import agent_runner


def _fake_run_capturing(captured: dict):
    def fake_run(cmd, **kw):
        captured["timeout"] = kw.get("timeout")

        class P:
            returncode = 0
            stdout = "{}"
            stderr = ""

        return P()

    return fake_run


def test_bare_arm_uses_shared_harness_timeout(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_HARNESS_TIMEOUT", "7200")
    captured: dict = {}

    # Stub the container runner so no docker/claude is invoked.
    stub = tmp_path / "stub.sh"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))
    monkeypatch.setattr(subprocess, "run", _fake_run_capturing(captured))

    agent_runner.run_bare(tmp_path / "wd", "do the task")
    assert captured["timeout"] == 7200


def test_bare_arm_default_matches_ecaa_arm_default(tmp_path, monkeypatch):
    # Neither arm-specific nor the shared env var set: the bare arm falls back
    # to the SAME 7200s default the ECAA arm uses (no separate 3600s ceiling).
    monkeypatch.delenv("ECAA_EVAL_HARNESS_TIMEOUT", raising=False)
    captured: dict = {}

    stub = tmp_path / "stub.sh"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))
    monkeypatch.setattr(subprocess, "run", _fake_run_capturing(captured))

    agent_runner.run_bare(tmp_path / "wd", "do the task")
    assert captured["timeout"] == 7200


def test_bare_arm_explicit_timeout_still_wins(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_HARNESS_TIMEOUT", "7200")
    captured: dict = {}

    stub = tmp_path / "stub.sh"
    stub.write_text("#!/bin/sh\nexit 0\n")
    stub.chmod(0o755)
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", str(stub))
    monkeypatch.setattr(subprocess, "run", _fake_run_capturing(captured))

    agent_runner.run_bare(tmp_path / "wd", "do the task", timeout=1234)
    assert captured["timeout"] == 1234
