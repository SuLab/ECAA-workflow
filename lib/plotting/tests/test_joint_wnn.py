from pathlib import Path
import pytest
from lib.plotting.stages import joint_wnn as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "joint_wnn"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "joint_wnn"

@pytest.mark.parametrize("figure_id", ["wnn_umap", "cluster_composition_bar"])
def test_renders(figure_id, tmp_path):
    ctx = make_context(FIXTURE)
    out = tmp_path / f"{figure_id}.png"
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None
    fn(ctx, out)
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
