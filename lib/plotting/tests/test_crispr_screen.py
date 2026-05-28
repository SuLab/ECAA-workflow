"""Snapshot tests for the crispr_screen renderer."""

from __future__ import annotations

from pathlib import Path

import pytest

from lib.plotting.stages import crispr_screen as stage
from lib.plotting.tests.helpers import make_context, structural_snapshot, assert_snapshot

FIXTURE = Path(__file__).parent / "fixtures" / "crispr_screen"
SNAPSHOTS = Path(__file__).parent / "snapshots" / "structural" / "crispr_screen"


@pytest.mark.parametrize("figure_id", ["sgrna_umi_histogram", "cells_per_perturbation_bar"])
def test_renders(figure_id: str, tmp_path: Path) -> None:
    ctx = make_context(FIXTURE)
    fn = stage.FIGURES.get(figure_id)
    assert fn is not None, f"figure_id '{figure_id}' not registered in crispr_screen"
    out = tmp_path / f"{figure_id}.png"
    result = fn(ctx, out)
    assert result == out
    assert out.exists()
    assert_snapshot(structural_snapshot(out), SNAPSHOTS / f"{figure_id}.json")
