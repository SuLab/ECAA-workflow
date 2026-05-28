"""Snapshot tests for chromatin_loops renderer."""
from pathlib import Path
import pytest
from lib.plotting.stages import chromatin_loops as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "chromatin_loops"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "chromatin_loops"

@pytest.mark.parametrize(
    "figure_id",
    ["loop_count_bar", "loop_size_distribution", "loop_volcano", "differential_loop_arc"],
)
def test_renders(figure_id, tmp_path):
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in chromatin_loops"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
