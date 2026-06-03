"""verify_campaign.py asserts a produced scorecard satisfies campaign.toml."""
import json
from pathlib import Path
import pytest
from scripts.eval.verify_campaign import verify_run, CampaignViolation


def _write_scorecard(run_dir: Path, *, benchmark, arms, n_pairs, seed=1729):
    run_dir.mkdir(parents=True, exist_ok=True)
    rows = []
    for arm in arms:
        for i in range(n_pairs):
            rows.append({"task_id": f"t{i}", "arm": arm, "trial": 0,
                         "overall": 80.0, "dimensions": {}, "jaccard": None,
                         "error_cells": None, "judge_id": "gemini-3.1-pro",
                         "extra": {}})
    meta = {"paired_delta": {"n_pairs": n_pairs, "min_power_pairs": 10},
            "seed": seed}
    (run_dir / "scorecard.json").write_text(
        json.dumps({"benchmark": benchmark, "meta": meta, "rows": rows}))


def test_compliant_scorecard_passes(tmp_path):
    rd = tmp_path / "biomnibench-run"
    _write_scorecard(rd, benchmark="biomnibench",
                     arms=["ecaa", "claude-direct"], n_pairs=10)
    # Compliant -> returns a report dict, no raise.
    report = verify_run(rd)
    assert report["compliant"] is True
    assert report["n_pairs"] == 10


def test_missing_arm_fails(tmp_path):
    rd = tmp_path / "run"
    _write_scorecard(rd, benchmark="biomnibench", arms=["ecaa"], n_pairs=10)
    with pytest.raises(CampaignViolation, match="arm"):
        verify_run(rd)


def test_underpowered_fails(tmp_path):
    rd = tmp_path / "run"
    _write_scorecard(rd, benchmark="biomnibench",
                     arms=["ecaa", "claude-direct"], n_pairs=4)
    with pytest.raises(CampaignViolation, match="paired"):
        verify_run(rd)


def test_seed_mismatch_fails(tmp_path):
    rd = tmp_path / "run"
    _write_scorecard(rd, benchmark="biomnibench",
                     arms=["ecaa", "claude-direct"], n_pairs=10, seed=42)
    with pytest.raises(CampaignViolation, match="seed"):
        verify_run(rd)
