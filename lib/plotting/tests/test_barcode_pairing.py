from pathlib import Path
import pytest
from lib.plotting.stages import barcode_pairing as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "barcode_pairing"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "barcode_pairing"

@pytest.mark.parametrize("figure_id", ["paired_capture_summary", "barcode_match_summary"])
def test_renders(figure_id, tmp_path):
    ctx = make_context(FIXTURE)
    out = tmp_path / f"{figure_id}.png"
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None
    fn(ctx, out)
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
