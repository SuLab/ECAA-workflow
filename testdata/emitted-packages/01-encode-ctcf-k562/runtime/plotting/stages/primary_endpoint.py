"""Primary endpoint stage — clinical-trial taxonomy. Renders the SAP's
pre-specified primary-endpoint figures: Kaplan-Meier curve (when
time-to-event) and a forest plot of the headline subgroup contrasts.

Plan reference: §S12 (clinical-trial-analysis stage `primary_endpoint`).

Manifest contract:
- ``survival_table``: TSV with `time`, `event`, optional `group` columns.
- ``forest_table``: TSV with `label`, `effect`, `ci_lo`, `ci_hi`, optional `weight`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    forest,
    kaplan_meier,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("primary_endpoint")


@register_figure(FIGURES, "kaplan_meier")
def kaplan_meier_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "survival_table")
    if p is None:
        raise FileNotFoundError("manifest.survival_table required")
    cols = load_tsv_columns(
        p,
        {
            "time": ("time", "follow_up", "fu_time", "T"),
            "event": ("event", "status", "censored", "E"),
            "group?": ("group", "arm", "treatment", "trt"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable survival table: {p}")
    group_col = "group" if "group" in cols else None
    return kaplan_meier(
        frame=cols,
        title="Primary endpoint — survival",
        out=out,
        group_col=group_col,
    )


@register_figure(FIGURES, "forest")
def forest_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "forest_table")
    if p is None:
        raise FileNotFoundError("manifest.forest_table required")
    cols = load_tsv_columns(
        p,
        {
            "label": ("label", "study", "contrast", "subgroup"),
            "effect": ("effect", "estimate", "logHR", "logOR", "beta"),
            "ci_lo": ("ci_lo", "lower", "lcl"),
            "ci_hi": ("ci_hi", "upper", "ucl"),
            "weight?": ("weight", "n", "size"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable forest table: {p}")
    return forest(frame=cols, title="Primary endpoint — forest", out=out)
