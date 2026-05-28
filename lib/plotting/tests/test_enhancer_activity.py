"""Snapshot tests for the enhancer_activity renderer."""

from __future__ import annotations

from pathlib import Path

import pytest

from lib.plotting.stages import enhancer_activity as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "enhancer_activity"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "enhancer_activity"


@pytest.mark.parametrize("figure_id", ["rna_vs_dna_scatter", "activity_score_histogram"])
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in enhancer_activity"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
