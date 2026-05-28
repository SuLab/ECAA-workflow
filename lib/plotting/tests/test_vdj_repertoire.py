"""Snapshot tests for the vdj_repertoire renderer."""

from __future__ import annotations

from pathlib import Path

import pytest

from lib.plotting.stages import vdj_repertoire as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "vdj_repertoire"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "vdj_repertoire"


@pytest.mark.parametrize(
    "figure_id",
    [
        "chain_assignment_per_cell_bar",
        "cdr3_length_distribution",
        "public_clonotype_size_bar",
        "clonotype_network",
        "clonal_frequency_rank",
        "v_segment_usage_bar",
    ],
)
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in vdj_repertoire"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
