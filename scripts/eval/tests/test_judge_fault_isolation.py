"""One judge provider failing (e.g. Gemini out of credits) must not block the
other, must not raise, and must not cache the failed provider (so --resume
re-attempts only the missing one)."""
from scripts.eval.services import judge as J
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.benchmark import Arm, Output, Task

_RUBRIC = {"criteria": [{"id": "c1", "dimension": "method", "points": 4,
                         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}}]}


def _raise(*a, **k):
    raise RuntimeError("provider out of credits")


def _reqs():
    return [
        {"key": "0:headline", "judge_id": "gemini-3.1-pro",
         "rubric": _RUBRIC, "trace": "t", "answer": "a"},
        {"key": "0:cross", "judge_id": "anthropic-opus",
         "rubric": _RUBRIC, "trace": "t", "answer": "a"},
    ]


def test_gemini_failure_does_not_block_anthropic(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    monkeypatch.setattr(J, "_gemini_call", _raise)
    monkeypatch.setattr(J, "_gemini_batch", _raise)
    monkeypatch.setattr(J, "_anthropic_call", lambda prompt: ("c1: A", 10, 2))

    results = J.judge_batch(_reqs())  # must NOT raise

    assert "0:cross" in results and results["0:cross"]["overall"] == 100.0
    assert "0:headline" not in results          # gemini unscored
    # failed provider left un-cached -> resume retries it
    assert list((tmp_path / "judge").glob("gemini-3.1-pro-*")) == []


def test_anthropic_failure_does_not_block_gemini(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    monkeypatch.setattr(J, "_anthropic_call", _raise)
    monkeypatch.setattr(J, "_anthropic_batch", _raise)
    monkeypatch.setattr(J, "_gemini_call", lambda prompt: ("c1: B", 10, 2))

    results = J.judge_batch(_reqs())

    assert "0:headline" in results and results["0:headline"]["overall"] == 50.0
    assert "0:cross" not in results
    assert list((tmp_path / "judge").glob("anthropic-opus-*")) == []


def test_gemini_batch_deadline_raises_and_is_fault_isolated(monkeypatch, tmp_path):
    """A Gemini batch that never reaches a terminal state (e.g. a 503 outage
    leaving it PENDING forever) must time out on ECAA_EVAL_JUDGE_BATCH_MAX_WAIT
    and RAISE — converting an infinite hang into a clean, fault-isolated skip
    (judge_batch's per-provider try/except logs it + leaves rows un-cached)."""
    import time as _time
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    monkeypatch.setenv("GEMINI_API_KEY", "test-key")
    # Force the batch path (>=1 miss) and a zero-second deadline so the very
    # first deadline check after the (no-op) sleep trips.
    monkeypatch.setenv("ECAA_EVAL_JUDGE_BATCH_MIN", "1")
    monkeypatch.setenv("ECAA_EVAL_JUDGE_BATCH_MAX_WAIT", "0")
    monkeypatch.setattr(_time, "sleep", lambda *_a, **_k: None)

    class _Resp:
        def __init__(self, payload):
            self._payload = payload

        def raise_for_status(self):
            return None

        def json(self):
            return self._payload

    # POST submits the batch (returns a name); GET always reports PENDING.
    monkeypatch.setattr(J.requests, "post",
                        lambda *a, **k: _Resp({"name": "batches/stuck"}))
    monkeypatch.setattr(J.requests, "get",
                        lambda *a, **k: _Resp({"state": "JOB_STATE_PENDING"}))

    reqs = [{"key": "0:headline", "judge_id": "gemini-3.1-pro",
             "rubric": _RUBRIC, "trace": "t", "answer": "a"}]
    results = J.judge_batch(reqs)  # must NOT raise out of judge_batch

    assert "0:headline" not in results                     # left unscored
    assert list((tmp_path / "judge").glob("gemini-3.1-pro-*")) == []  # un-cached


def test_assemble_score_partial_cross_only():
    """Gemini down -> only the Opus cross verdict present -> usable partial score."""
    bb = BiomniBench()
    task = Task("da-1-1", "q", {}, rubric=None, answer_key=None)
    out = Output("trace", "ans", {}, True, 0.0)
    vd = {"cross": {"overall": 80.0, "dimensions": {"method": 80.0},
                    "levels": {"c1": "A"}, "cost_usd": 0.2}}
    s = bb.assemble_score(task, Arm.ECAA_WORKFLOW, out, 0, vd)
    assert s.overall == 80.0
    assert s.judge_id == "anthropic-opus"
    assert s.extra.get("partial_judging") is True
    assert s.dimensions == {"method": 80.0}


def test_assemble_score_both_present_keeps_agreement():
    bb = BiomniBench()
    task = Task("da-1-1", "q", {}, rubric=None, answer_key=None)
    out = Output("trace", "ans", {}, True, 0.0)
    vd = {"headline": {"overall": 90.0, "dimensions": {"method": 90.0},
                       "levels": {"c1": "A"}, "cost_usd": 0.01},
          "cross": {"overall": 85.0, "dimensions": {"method": 85.0},
                    "levels": {"c1": "A"}, "cost_usd": 0.2}}
    s = bb.assemble_score(task, Arm.ECAA_WORKFLOW, out, 0, vd)
    assert s.overall == 90.0 and s.judge_id == "gemini-3.1-pro"
    assert s.extra["cross_check"] == 85.0
    assert "judge_exact" in s.extra and "judge_kappa" in s.extra
    assert "partial_judging" not in s.extra
