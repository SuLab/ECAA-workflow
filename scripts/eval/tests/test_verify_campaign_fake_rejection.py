"""verify_campaign.verify_run rejects fake/single-arm/empty/degenerate cards.

These are the B2 acceptance-gate cases: a committed scorecard must be a real
two-arm run before any value claim is published. A placeholder benchmark, an
empty card, a single-arm card, or a card whose every row carries one constant
``overall`` (the signature of a synthesized fixture, e.g. all 42.0) is rejected.
"""
import json
from pathlib import Path

import pytest

from scripts.eval.verify_campaign import verify_run, CampaignViolation

_MANIFEST = {
    "campaign": {"seed": 1729, "min_paired_pairs": 10,
                 "arms": ["ecaa", "claude-direct"]},
    "benchmarks": [{"name": "nekrutenko", "judge": "deterministic"},
                   {"name": "biomnibench", "judge": "gemini-3.1-pro"}],
}


def _write(run_dir: Path, card: dict) -> Path:
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "scorecard.json").write_text(json.dumps(card))
    return run_dir


def test_rejects_unknown_benchmark(tmp_path):
    card = {"benchmark": "fake", "meta": {"seed": 1729,
            "paired_delta": {"n_pairs": 12}},
            "rows": [{"arm": "ecaa", "task_id": "t", "overall": 1.0,
                      "judge_id": "deterministic"},
                     {"arm": "claude-direct", "task_id": "t", "overall": 2.0,
                      "judge_id": "deterministic"}]}
    rd = _write(tmp_path / "r", card)
    with pytest.raises(CampaignViolation, match="benchmark"):
        verify_run(rd, _MANIFEST)


def test_rejects_empty_rows(tmp_path):
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 0}}, "rows": []}
    rd = _write(tmp_path / "r", card)
    with pytest.raises(CampaignViolation, match="no rows|empty"):
        verify_run(rd, _MANIFEST)


def test_rejects_single_arm(tmp_path):
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 12}},
            "rows": [{"arm": "claude-direct", "task_id": "t", "overall": 42.0,
                      "judge_id": "deterministic"}]}
    rd = _write(tmp_path / "r", card)
    with pytest.raises(CampaignViolation, match="missing required arm"):
        verify_run(rd, _MANIFEST)


def test_rejects_degenerate_constant_overall(tmp_path):
    rows = [{"arm": a, "task_id": f"t{i}", "overall": 42.0,
             "judge_id": "deterministic"}
            for a in ("ecaa", "claude-direct") for i in range(12)]
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 12}},
            "rows": rows}
    rd = _write(tmp_path / "r", card)
    with pytest.raises(CampaignViolation, match="degenerate|constant"):
        verify_run(rd, _MANIFEST)


def test_accepts_real_two_arm_card(tmp_path):
    rows = [{"arm": a, "task_id": f"t{i}", "overall": 40.0 + i,
             "judge_id": "deterministic"}
            for a in ("ecaa", "claude-direct") for i in range(12)]
    card = {"benchmark": "nekrutenko",
            "meta": {"seed": 1729, "paired_delta": {"n_pairs": 12}},
            "rows": rows}
    rd = _write(tmp_path / "r", card)
    report = verify_run(rd, _MANIFEST)
    assert report["compliant"] is True
    assert sorted(report["arms"]) == ["claude-direct", "ecaa"]
