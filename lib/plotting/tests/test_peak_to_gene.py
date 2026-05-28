from pathlib import Path
import pytest
from lib.plotting.stages import peak_to_gene as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "peak_to_gene"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "peak_to_gene"

@pytest.mark.parametrize("figure_id", ["peak_to_gene_arc", "links_per_cluster_bar"])
def test_renders(figure_id, tmp_path):
    ctx = make_context(FIXTURE)
    out = tmp_path / f"{figure_id}.png"
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None
    fn(ctx, out)
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
