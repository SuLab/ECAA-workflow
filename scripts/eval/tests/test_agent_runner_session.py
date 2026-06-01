"""run_ecaa_package: --session-id/--server-url wiring + SME auto-approve env."""
from __future__ import annotations

from scripts.eval.services import agent_runner


class _FakeProc:
    returncode = 0


def _capture(monkeypatch):
    captured = {}

    def fake_run(cmd, **kw):
        captured["cmd"] = cmd
        captured["env"] = kw.get("env")
        return _FakeProc()

    monkeypatch.setattr(agent_runner.subprocess, "run", fake_run)
    return captured


def test_session_and_server_url_appended_when_both_set(monkeypatch, tmp_path):
    cap = _capture(monkeypatch)
    agent_runner.run_ecaa_package(tmp_path, session_id="sid-9",
                                  server_url="http://127.0.0.1:8123")
    cmd = cap["cmd"]
    assert "--session-id" in cmd and cmd[cmd.index("--session-id") + 1] == "sid-9"
    assert ("--server-url" in cmd
            and cmd[cmd.index("--server-url") + 1] == "http://127.0.0.1:8123")


def test_no_session_flags_for_offline_cell(monkeypatch, tmp_path):
    cap = _capture(monkeypatch)
    agent_runner.run_ecaa_package(tmp_path, session_id=None, server_url=None)
    cmd = cap["cmd"]
    assert "--session-id" not in cmd
    assert "--server-url" not in cmd


def test_session_id_without_server_url_is_not_wired(monkeypatch, tmp_path):
    # A session-id with no server-url has nowhere to post; require BOTH.
    cap = _capture(monkeypatch)
    agent_runner.run_ecaa_package(tmp_path, session_id="sid-9", server_url=None)
    assert "--session-id" not in cap["cmd"]


def test_sme_auto_approve_all_env_not_injected(monkeypatch, tmp_path):
    """eval-02: the runner must NOT set ECAA_SME_AUTO_APPROVE_ALL. That flag is
    an all-or-nothing bypass that disables the silent-completion / missing-
    artifact / validation / claim guards we keep ACTIVE; the discovery review
    gate is cleared narrowly via per-package marker files instead."""
    cap = _capture(monkeypatch)
    agent_runner.run_ecaa_package(tmp_path, env={"PATH": "/usr/bin"})
    assert "ECAA_SME_AUTO_APPROVE_ALL" not in cap["env"]


def test_sme_auto_approve_all_env_caller_value_preserved(monkeypatch, tmp_path):
    """A debugging operator may still force the flag via the caller env; the
    runner does not clobber it (and the value flows through unchanged)."""
    cap = _capture(monkeypatch)
    agent_runner.run_ecaa_package(
        tmp_path, env={"PATH": "/usr/bin", "ECAA_SME_AUTO_APPROVE_ALL": "1"})
    assert cap["env"]["ECAA_SME_AUTO_APPROVE_ALL"] == "1"
