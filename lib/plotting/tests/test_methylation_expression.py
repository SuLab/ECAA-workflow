"""Snapshot tests for the methylation_expression renderer."""

from __future__ import annotations

from pathlib import Path

import pytest

from lib.plotting.stages import methylation_expression as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "methylation_expression"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "methylation_expression"


@pytest.mark.parametrize(
    "figure_id",
    ["methylation_vs_expression_scatter", "methylation_driven_gene_heatmap"],
)
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in methylation_expression"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
