"""Snapshot tests for chromatin_contacts renderer."""
from pathlib import Path
import pytest
from lib.plotting.stages import chromatin_contacts as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "chromatin_contacts"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "chromatin_contacts"

@pytest.mark.parametrize("figure_id", ["cis_trans_ratio", "distance_decay_curve"])
def test_renders(figure_id, tmp_path):
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in chromatin_contacts"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
