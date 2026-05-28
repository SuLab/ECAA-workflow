"""Forecasting inference stage — time-series-forecast taxonomy.
Renders the forecast ribbon (point + interval + actual overlay) and
the anomaly timeline so the SAP review committee can see model fit
and anomalous events in the same lens.

Plan reference: §S14 (time-series-forecast taxonomy stage
`forecasting_inference`).

Manifest contract:
- ``forecast_table``: TSV with `time`, `forecast`, `lower`, `upper`,
  optional `actual`. The forecast columns drive the ribbon.
- ``anomaly_table``: TSV with `time`, `value`, `is_anomaly`. The
  anomaly column drives the shaded windows.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    anomaly_timeline,
    forecast_ribbon,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("forecasting_inference")


@register_figure(FIGURES, "forecast_ribbon")
def forecast_ribbon_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "forecast_table")
    if p is None:
        raise FileNotFoundError("manifest.forecast_table required")
    cols = load_tsv_columns(
        p,
        {
            "time": ("time", "date", "timestamp", "t"),
            "forecast": ("forecast", "yhat", "predicted", "value"),
            "lower": ("lower", "lcl", "yhat_lower", "low"),
            "upper": ("upper", "ucl", "yhat_upper", "high"),
            "actual?": ("actual", "y", "observed"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable forecast table: {p}")
    actual_col = "actual" if "actual" in cols else None
    return forecast_ribbon(
        frame=cols, title="Forecast", out=out, actual_col=actual_col
    )


@register_figure(FIGURES, "anomaly_timeline")
def anomaly_timeline_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "anomaly_table")
    if p is None:
        raise FileNotFoundError("manifest.anomaly_table required")
    cols = load_tsv_columns(
        p,
        {
            "time": ("time", "date", "timestamp", "t"),
            "value": ("value", "y", "observation"),
            "is_anomaly": ("is_anomaly", "anomaly", "flag", "alert"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable anomaly table: {p}")
    return anomaly_timeline(
        frame=cols, title="Anomaly timeline", out=out
    )
