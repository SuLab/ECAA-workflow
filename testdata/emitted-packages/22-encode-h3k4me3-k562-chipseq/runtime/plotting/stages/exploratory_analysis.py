"""Exploratory analysis stage — time-series-forecast taxonomy.
Renders the decomposition panel (observed / trend / seasonal /
residual) and the paired ACF / PACF lag-correlation plot for SAP-style
exploratory analysis.

Plan reference: §S14 (time-series-forecast taxonomy stage
`exploratory_analysis`).

Manifest contract:
- ``series_table``: TSV with `time`, `value`. Evenly-spaced points
  (the decomposition + ACF primitives require an evenly-spaced grid).
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    acf_pacf_panel,
    decomposition_panel,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("exploratory_analysis")


@register_figure(FIGURES, "decomposition_panel")
def decomposition_panel_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "series_table")
    if p is None:
        raise FileNotFoundError("manifest.series_table required")
    cols = load_tsv_columns(
        p,
        {
            "time": ("time", "date", "timestamp", "t"),
            "value": ("value", "y", "observation"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable series table: {p}")
    return decomposition_panel(
        frame=cols, title="Series decomposition", out=out
    )


@register_figure(FIGURES, "acf_pacf_panel")
def acf_pacf_panel_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "series_table")
    if p is None:
        raise FileNotFoundError("manifest.series_table required")
    cols = load_tsv_columns(
        p,
        {
            "value": ("value", "y", "observation"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable series table: {p}")
    return acf_pacf_panel(
        frame=cols, title="ACF + PACF", out=out
    )
