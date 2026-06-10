import pytest

from scripts.eval.services import agent_runner


def test_eval_model_default_claude(monkeypatch):
    monkeypatch.delenv("ECAA_EVAL_MODEL", raising=False)
    monkeypatch.delenv("ECAA_AGENT_BACKEND", raising=False)
    assert agent_runner.eval_model() == "claude-sonnet-4-6"


def test_eval_model_rejects_claude_id_on_codex_backend(monkeypatch):
    monkeypatch.setenv("ECAA_AGENT_BACKEND", "codex")
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    with pytest.raises(ValueError, match="claude-.*codex"):
        agent_runner.eval_model()


def test_eval_model_rejects_gpt_id_on_claude_backend(monkeypatch):
    monkeypatch.delenv("ECAA_AGENT_BACKEND", raising=False)  # default = claude
    monkeypatch.setenv("ECAA_EVAL_MODEL", "gpt-5.5-codex")
    with pytest.raises(ValueError, match="gpt-.*claude"):
        agent_runner.eval_model()


def test_eval_model_rejects_o_series_id_on_claude_backend(monkeypatch):
    monkeypatch.delenv("ECAA_AGENT_BACKEND", raising=False)
    monkeypatch.setenv("ECAA_EVAL_MODEL", "o3-mini")
    with pytest.raises(ValueError, match="claude"):
        agent_runner.eval_model()


def test_eval_model_accepts_matching_codex_id(monkeypatch):
    monkeypatch.setenv("ECAA_AGENT_BACKEND", "codex")
    monkeypatch.setenv("ECAA_EVAL_MODEL", "gpt-5.5-codex")
    assert agent_runner.eval_model() == "gpt-5.5-codex"


def test_eval_model_accepts_matching_claude_id(monkeypatch):
    monkeypatch.delenv("ECAA_AGENT_BACKEND", raising=False)
    monkeypatch.setenv("ECAA_EVAL_MODEL", "claude-opus-4-8")
    assert agent_runner.eval_model() == "claude-opus-4-8"
