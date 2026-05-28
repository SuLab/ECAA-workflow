"""Snapshot tests for the ribo_seq renderer."""

from __future__ import annotations

from pathlib import Path

import pytest

from lib.plotting.stages import ribo_seq as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "ribo_seq"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "ribo_seq"


@pytest.mark.parametrize(
    "figure_id",
    ["psite_offset_per_length", "frame_periodicity", "te_volcano", "te_quadrant_plot"],
)
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in ribo_seq"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
