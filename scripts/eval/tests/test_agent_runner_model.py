"""Fairness: both arms must run the SAME model so the ecaa-vs-direct delta
isolates the scaffolding, not model capability."""
from scripts.eval.services import agent_runner as ar


def test_eval_model_honors_env(monkeypatch):
    monkeypatch.delenv("ECAA_EVAL_MODEL", raising=False)
    assert ar.eval_model() == "claude-sonnet-4-6"
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    assert ar.eval_model() == "claude-opus-4-8"


def test_ecaa_arm_pins_model_override(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    captured = {}

    def fake_run(cmd, **kw):
        captured["env"] = kw.get("env")

        class P:
            returncode = 0
        return P()

    monkeypatch.setattr(ar, "_run_in_process_group", fake_run)
    ar.run_ecaa_package(tmp_path)
    assert captured["env"]["ECAA_AGENT_MODEL_OVERRIDE"] == "claude-opus-4-8"


def test_bare_arm_pins_same_model(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    monkeypatch.setenv("ECAA_EVAL_BARE_AGENT_SCRIPT", "/bin/true")
    captured = {}

    def fake_run(cmd, **kw):
        captured["env"] = kw.get("env")

        class P:
            returncode = 0
            stdout = ""
        return P()

    monkeypatch.setattr(ar.subprocess, "run", fake_run)
    ar.run_bare(tmp_path, "instruction")
    assert captured["env"]["ECAA_EVAL_BARE_MODEL"] == "claude-opus-4-8"


def test_explicit_override_wins(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    monkeypatch.setenv("ECAA_AGENT_MODEL_OVERRIDE", "claude-haiku-4-5")
    captured = {}

    def fake_run(cmd, **kw):
        captured["env"] = kw.get("env")

        class P:
            returncode = 0
        return P()

    monkeypatch.setattr(ar, "_run_in_process_group", fake_run)
    ar.run_ecaa_package(tmp_path)
    # an explicit caller/operator override is respected (setdefault)
    assert captured["env"]["ECAA_AGENT_MODEL_OVERRIDE"] == "claude-haiku-4-5"
