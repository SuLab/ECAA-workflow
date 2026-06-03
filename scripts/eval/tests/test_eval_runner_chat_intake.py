"""eval_runner chat-intake wiring: _chat_intake_or_cli + _ensure_package_for_cells."""
from __future__ import annotations
from pathlib import Path

from scripts.eval import eval_runner
from scripts.eval.benchmark import Arm, RunSpec, Task


class _EcaaPlugin:
    """Minimal plugin that builds an ECAA spec and locks one method."""
    def build_run(self, task, arm, workdir):
        Path(workdir).mkdir(parents=True, exist_ok=True)
        return RunSpec(arm, Path(workdir), "ecaa_package", "do variant calling")

    def locked_methods(self, task, arm):
        return [("alignment", "bwa")] if arm == Arm.ECAA_WORKFLOW else []


def _task():
    return Task("mtdna", "p", inputs={}, rubric=None, answer_key=None, meta={})


def test_chat_intake_sets_session_and_package(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "chat")
    emitted = tmp_path / "emitted-pkg"
    emitted.mkdir()
    seen = {}

    def fake_drive(base_url, instruction, *, locked_methods=None, **kw):
        seen["base_url"] = base_url
        seen["locked"] = locked_methods
        return "sid-42", emitted

    monkeypatch.setattr(eval_runner, "drive_chat_intake", fake_drive)
    # Don't actually stage/auto-approve real files beyond the emitted dir.
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate", lambda *a, **k: None)

    class _Server:
        base_url = "http://127.0.0.1:9999"

    spec = eval_runner._chat_intake_or_cli(
        _EcaaPlugin(), _task(), Arm.ECAA_WORKFLOW, tmp_path / "wd", _Server())
    assert spec.session_id == "sid-42"
    assert spec.package_dir == emitted
    assert seen["base_url"] == "http://127.0.0.1:9999"
    assert seen["locked"] == [("alignment", "bwa")]


def test_chat_intake_passes_proposal_policy(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "chat")
    emitted = tmp_path / "emitted-pkg"
    emitted.mkdir()
    seen = {}

    def fake_drive(base_url, instruction, *, locked_methods=None,
                   proposal_policy="reject", **kw):
        seen["policy"] = proposal_policy
        return "sid-1", emitted

    monkeypatch.setattr(eval_runner, "drive_chat_intake", fake_drive)
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate", lambda *a, **k: None)

    class _Server:
        base_url = "http://127.0.0.1:9999"

    class _PolicyPlugin(_EcaaPlugin):
        def proposal_policy(self, task, arm):
            return "signoff"

    eval_runner._chat_intake_or_cli(
        _PolicyPlugin(), _task(), Arm.ECAA_WORKFLOW, tmp_path / "wd", _Server())
    assert seen["policy"] == "signoff"


def test_cli_fallback_uses_intake_subprocess(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "cli")
    calls = {}

    def fake_run(cmd, **kw):
        calls["cmd"] = cmd
        # Create the output pkg dir the CLI would have produced.
        out = Path(cmd[cmd.index("-o") + 1])
        out.mkdir(parents=True, exist_ok=True)
        class _P:
            returncode = 0
        return _P()

    monkeypatch.setattr(eval_runner.subprocess, "run", fake_run)
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate", lambda *a, **k: None)

    spec = eval_runner._chat_intake_or_cli(
        _EcaaPlugin(), _task(), Arm.ECAA_WORKFLOW, tmp_path / "wd", None)
    assert spec.session_id is None
    assert spec.package_dir == (tmp_path / "wd" / "pkg")
    assert calls["cmd"][:2] == ["ecaa-workflow", "intake"]


def test_ensure_package_for_cells_reuses_existing_dir(monkeypatch, tmp_path):
    existing = tmp_path / "prior-pkg"
    existing.mkdir()
    base_rec = {"package_dir": str(existing), "session_id": "sid-old"}

    # If reuse works, intake must NOT be re-driven.
    monkeypatch.setattr(eval_runner, "drive_chat_intake",
                        lambda *a, **k: (_ for _ in ()).throw(
                            AssertionError("should not re-drive intake")))

    spec = eval_runner._ensure_package_for_cells(
        _EcaaPlugin(), _task(), Arm.ECAA_WORKFLOW, base_rec, tmp_path / "wd", None)
    assert spec.package_dir == existing
    assert spec.session_id == "sid-old"


_SENTINEL = "DO-NOT-FETCH-THE-PUBLISHED-ANSWER-FROM-THE-INTERNET"


class _ContaminationPlugin(_EcaaPlugin):
    """ECAA plugin that opts into a package-wide anti-contamination directive."""
    def contamination_directive(self):
        return _SENTINEL


class _NoDirectivePlugin(_EcaaPlugin):
    """ECAA plugin that opts OUT (the Benchmark default): directive is None."""
    def contamination_directive(self):
        return None


def _drive_emitting_prompt(emitted: Path, base_text: str):
    """Build a fake drive_chat_intake that emits a dir with a seeded PROMPT.md."""
    def _fake_drive(base_url, instruction, *, locked_methods=None, **kw):
        emitted.mkdir(parents=True, exist_ok=True)
        (emitted / "PROMPT.md").write_text(base_text)
        return "sid-directive", emitted
    return _fake_drive


def test_contamination_directive_reaches_emitted_prompt_md(monkeypatch, tmp_path):
    """The ECAA arm's contamination_directive() must be appended to the emitted
    PROMPT.md (agent-claude.sh re-reads PROMPT.md per task), so every task's
    agent invocation sees the integrity control."""
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "chat")
    emitted = tmp_path / "emitted-pkg"
    base_text = "# Task contract\nrun the analysis\n"
    monkeypatch.setattr(eval_runner, "drive_chat_intake",
                        _drive_emitting_prompt(emitted, base_text))
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate",
                        lambda *a, **k: None)

    class _Server:
        base_url = "http://127.0.0.1:9999"

    spec = eval_runner._chat_intake_or_cli(
        _ContaminationPlugin(), _task(), Arm.ECAA_WORKFLOW, tmp_path / "wd",
        _Server())

    prompt_text = (spec.package_dir / "PROMPT.md").read_text()
    # Base text preserved AND the sentinel directive appended.
    assert base_text.strip() in prompt_text
    assert _SENTINEL in prompt_text, (
        "contamination_directive() must reach the emitted PROMPT.md")


def test_no_contamination_directive_leaves_prompt_md_unchanged(monkeypatch, tmp_path):
    """CONTRAST: a plugin whose contamination_directive() returns None must NOT
    mutate PROMPT.md — proves the directive is genuinely wired, not assumed."""
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "chat")
    emitted = tmp_path / "emitted-pkg"
    base_text = "# Task contract\nrun the analysis\n"
    monkeypatch.setattr(eval_runner, "drive_chat_intake",
                        _drive_emitting_prompt(emitted, base_text))
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate",
                        lambda *a, **k: None)

    class _Server:
        base_url = "http://127.0.0.1:9999"

    spec = eval_runner._chat_intake_or_cli(
        _NoDirectivePlugin(), _task(), Arm.ECAA_WORKFLOW, tmp_path / "wd",
        _Server())

    prompt_text = (spec.package_dir / "PROMPT.md").read_text()
    assert prompt_text == base_text, "None directive must leave PROMPT.md untouched"
    assert _SENTINEL not in prompt_text


def test_ensure_package_for_cells_reemits_when_dir_gone(monkeypatch, tmp_path):
    base_rec = {"package_dir": str(tmp_path / "vanished"), "session_id": "sid-x"}
    fresh = tmp_path / "fresh-pkg"
    fresh.mkdir()
    monkeypatch.setenv("ECAA_EVAL_INTAKE", "chat")
    monkeypatch.setattr(eval_runner, "drive_chat_intake",
                        lambda *a, **k: ("sid-new", fresh))
    monkeypatch.setattr(eval_runner, "_stage_inputs", lambda *a, **k: None)
    monkeypatch.setattr(eval_runner, "_write_auto_approve_discovery_gate", lambda *a, **k: None)

    class _Server:
        base_url = "http://127.0.0.1:1"

    spec = eval_runner._ensure_package_for_cells(
        _EcaaPlugin(), _task(), Arm.ECAA_WORKFLOW, base_rec, tmp_path / "wd",
        _Server())
    assert spec.package_dir == fresh
    assert spec.session_id == "sid-new"
