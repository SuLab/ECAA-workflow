"""Offline unit tests for judge.judge_batch.

_gemini_batch and _anthropic_batch are monkeypatched so no live HTTP occurs.
"""
from __future__ import annotations
import json
from pathlib import Path

import pytest

from scripts.eval.services import judge as judge_mod
from scripts.eval.services.judge import judge_batch, _judge_cost_usd

RUBRIC = {
    "criteria": [
        {"id": "c1", "dimension": "method", "points": 4,
         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}},
        {"id": "c2", "dimension": "method", "points": 2,
         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}},
    ]
}
TRACE = "Some trace text"
ANSWER = "Some answer text"


def _make_req(key: str, judge_id: str) -> dict:
    return {
        "key": key,
        "judge_id": judge_id,
        "rubric": RUBRIC,
        "trace": TRACE,
        "answer": ANSWER,
    }


# ---------------------------------------------------------------------------
# Helper: pre-seed a cache file so we can test hit-path cost=0.
# ---------------------------------------------------------------------------

def _seed_cache(tmp_path, monkeypatch, judge_id: str, verdict_text: str) -> str:
    """Write a cache file and redirect ECAA_EVAL_CACHE_DIR to tmp_path.

    Returns the key that would match this judge+rubric+trace+answer combo.
    """
    import hashlib
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    h = hashlib.sha256(
        (judge_id + json.dumps(RUBRIC, sort_keys=True) + TRACE + ANSWER).encode()
    ).hexdigest()
    cache_dir = tmp_path / "judge"
    cache_dir.mkdir(parents=True, exist_ok=True)
    (cache_dir / f"{judge_id}-{h}.txt").write_text(verdict_text)


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_cache_hit_returns_cost_zero(tmp_path, monkeypatch):
    """A pre-seeded cache entry is returned with cost_usd=0."""
    _seed_cache(tmp_path, monkeypatch, "gemini-3.1-pro", "c1: A\nc2: A")

    req = _make_req("k-hit", "gemini-3.1-pro")
    results = judge_batch([req])

    assert "k-hit" in results
    assert results["k-hit"]["cost_usd"] == 0.0
    assert results["k-hit"]["overall"] == 100.0


def test_gemini_miss_calls_gemini_batch(tmp_path, monkeypatch):
    """A Gemini cache miss delegates to _gemini_batch; result is cached + returned."""
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))

    canned_text = "c1: A\nc2: B"
    canned_in = 500
    canned_out = 20

    def fake_gemini_batch(items):
        return {item["key"]: (canned_text, canned_in, canned_out) for item in items}

    monkeypatch.setattr(judge_mod, "_gemini_batch", fake_gemini_batch)

    req = _make_req("k-gemini-miss", "gemini-3.1-pro")
    results = judge_batch([req])

    assert "k-gemini-miss" in results
    v = results["k-gemini-miss"]
    expected_cost = _judge_cost_usd("gemini-3.1-pro", canned_in, canned_out)
    assert abs(v["cost_usd"] - expected_cost) < 1e-9
    # c1: A (4 pts * 1.0) + c2: B (2 pts * 0.5) = 5 of 6 -> 83.333...
    assert abs(v["overall"] - round(100.0 * 5 / 6, 4)) < 1e-3


def test_anthropic_miss_calls_anthropic_batch(tmp_path, monkeypatch):
    """An Anthropic cache miss delegates to _anthropic_batch; result is cached + returned."""
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))

    canned_text = "c1: B\nc2: C"
    canned_in = 800
    canned_out = 30

    def fake_anthropic_batch(items):
        return {item["key"]: (canned_text, canned_in, canned_out) for item in items}

    monkeypatch.setattr(judge_mod, "_anthropic_batch", fake_anthropic_batch)

    req = _make_req("k-anthropic-miss", "anthropic-opus")
    results = judge_batch([req])

    assert "k-anthropic-miss" in results
    v = results["k-anthropic-miss"]
    expected_cost = _judge_cost_usd("anthropic-opus", canned_in, canned_out)
    assert abs(v["cost_usd"] - expected_cost) < 1e-9


def test_mixed_hit_and_miss_both_returned(tmp_path, monkeypatch):
    """Mix of one pre-cached and one cache-miss returns both, with right costs."""
    # Pre-seed cache for the hit.
    _seed_cache(tmp_path, monkeypatch, "gemini-3.1-pro", "c1: A\nc2: A")

    canned_text = "c1: C\nc2: C"

    def fake_gemini_batch(items):
        # Only the miss key is passed here.
        return {item["key"]: (canned_text, 100, 10) for item in items}

    monkeypatch.setattr(judge_mod, "_gemini_batch", fake_gemini_batch)

    # Build two requests with different keys but same rubric+trace+answer for
    # the hit, and a different trace to force a cache miss for the second.
    import hashlib
    # hit: same TRACE/ANSWER -> will hit cache
    req_hit = _make_req("k-hit2", "gemini-3.1-pro")

    # miss: different trace string -> no cache entry
    req_miss = {
        "key": "k-miss2",
        "judge_id": "gemini-3.1-pro",
        "rubric": RUBRIC,
        "trace": "completely different trace",
        "answer": ANSWER,
    }

    results = judge_batch([req_hit, req_miss])

    assert results["k-hit2"]["cost_usd"] == 0.0
    assert results["k-miss2"]["cost_usd"] == _judge_cost_usd("gemini-3.1-pro", 100, 10)


def test_cache_file_written_after_miss(tmp_path, monkeypatch):
    """After fetching a miss, the response text is written to the cache file."""
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    canned_text = "c1: A\nc2: A"

    def fake_gemini_batch(items):
        return {item["key"]: (canned_text, 100, 5) for item in items}

    monkeypatch.setattr(judge_mod, "_gemini_batch", fake_gemini_batch)

    req = _make_req("k-write", "gemini-3.1-pro")
    judge_batch([req])

    # The cache file should now exist and contain the canned text.
    import hashlib
    h = hashlib.sha256(
        ("gemini-3.1-pro" + json.dumps(RUBRIC, sort_keys=True) + TRACE + ANSWER).encode()
    ).hexdigest()
    cache_file = tmp_path / "judge" / f"gemini-3.1-pro-{h}.txt"
    assert cache_file.exists(), "cache file should be written after a miss"
    assert cache_file.read_text() == canned_text


def test_both_providers_batched_in_one_call(tmp_path, monkeypatch):
    """Gemini and Anthropic misses both go to their respective batch helpers."""
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))

    gemini_called = []
    anthropic_called = []

    def fake_gemini_batch(items):
        gemini_called.extend(items)
        return {item["key"]: ("c1: A\nc2: A", 10, 5) for item in items}

    def fake_anthropic_batch(items):
        anthropic_called.extend(items)
        return {item["key"]: ("c1: B\nc2: B", 20, 8) for item in items}

    monkeypatch.setattr(judge_mod, "_gemini_batch", fake_gemini_batch)
    monkeypatch.setattr(judge_mod, "_anthropic_batch", fake_anthropic_batch)

    reqs = [
        {"key": "g1", "judge_id": "gemini-3.1-pro", "rubric": RUBRIC,
         "trace": "trace-g1", "answer": ANSWER},
        {"key": "a1", "judge_id": "anthropic-opus", "rubric": RUBRIC,
         "trace": "trace-a1", "answer": ANSWER},
        {"key": "g2", "judge_id": "gemini-3.1-pro", "rubric": RUBRIC,
         "trace": "trace-g2", "answer": ANSWER},
    ]
    results = judge_batch(reqs)

    assert len(gemini_called) == 2, "both gemini misses submitted together"
    assert len(anthropic_called) == 1, "single anthropic miss submitted"
    assert set(results.keys()) == {"g1", "a1", "g2"}
