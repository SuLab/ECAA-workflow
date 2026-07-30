"""Tests for the mediation-analysis renderer contract."""

from __future__ import annotations

from pathlib import Path

from lib.plotting.stages import mediation_analysis as stage
from lib.plotting.tests.helpers import make_context


def test_forest_reads_mediation_specific_estimands(tmp_path: Path) -> None:
    table = tmp_path / "mediation_results.tsv"
    table.write_text(
        "mediator\tindirect_effect\tindirect_ci_lower\tindirect_ci_upper\n"
        "IL6\t0.21\t0.08\t0.34\n"
        "VCAM1\t-0.12\t-0.25\t0.01\n"
    )
    ctx = make_context(
        tmp_path,
        manifest={"mediation_results": table.name},
        stage_id="mediation_analysis",
        figure_id="forest",
    )
    renderer = stage.FIGURES.get("forest")
    assert renderer is not None
    output = tmp_path / "forest.png"
    assert renderer(ctx, output) == output
    assert output.is_file()
