"""Offline tests for cost-capture rendering in scorecard markdown (T16)."""
from __future__ import annotations
import tempfile
from pathlib import Path

from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import write_scorecard


def _minimal_score(overall: float = 80.0, judge_cost_usd: float = 0.0) -> Score:
    return Score(
        task_id="t1",
        arm="ecaa",
        trial=0,
        overall=overall,
        dimensions={},
        jaccard=None,
        error_cells=None,
        judge_id="gemini-3.1-pro",
        extra={"judge_cost_usd": judge_cost_usd},
    )


def test_scorecard_cost_rendered_in_markdown():
    """write_scorecard emits 'Judge cost (USD): <value>' when meta has cost key."""
    card = Scorecard(
        benchmark="biomnibench",
        rows=[_minimal_score(judge_cost_usd=1.2345)],
        meta={"cost": {"judge_usd": 1.2345}},
    )
    with tempfile.TemporaryDirectory() as td:
        out_dir = write_scorecard(card, Path(td))
        md = (out_dir / "scorecard.md").read_text()

    assert "Judge cost (USD): 1.2345" in md


def test_scorecard_cost_not_double_printed():
    """The 'cost' meta key must not appear as a raw scalar bullet."""
    card = Scorecard(
        benchmark="biomnibench",
        rows=[_minimal_score()],
        meta={"cost": {"judge_usd": 0.5}},
    )
    with tempfile.TemporaryDirectory() as td:
        out_dir = write_scorecard(card, Path(td))
        md = (out_dir / "scorecard.md").read_text()

    # The rich-key skip prevents a bullet like "- **cost:** ..."
    assert "**cost:**" not in md
    # But the formatted line must appear
    assert "Judge cost (USD): 0.5" in md
