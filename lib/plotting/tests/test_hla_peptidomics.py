"""Snapshot tests for hla_peptidomics renderer."""

from pathlib import Path
import pytest
from lib.plotting.stages import hla_peptidomics as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "hla_peptidomics"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "hla_peptidomics"


@pytest.mark.parametrize(
    "figure_id",
    [
        "peptide_length_distribution",
        "psm_score_distribution",
        "binder_rank_histogram",
        "allele_presentation_heatmap",
        "neoantigen_per_patient_bar",
        "shared_neoantigen_heatmap",
    ],
)
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in hla_peptidomics"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
