"""Secondary endpoints stage — clinical-trial taxonomy. Renders the
SAP's pre-specified secondary-endpoint figures: per-subject
longitudinal trajectories (spaghetti) and a forest plot of secondary
contrasts.

Plan reference: §S12 (clinical-trial-analysis stage `secondary_endpoints`).

Manifest contract:
- ``longitudinal_table``: TSV with `id`, `time`, `value`, optional `group`.
- ``forest_table``: TSV with `label`, `effect`, `ci_lo`, `ci_hi`, optional `weight`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    forest,
    register_figure,
    spaghetti,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("secondary_endpoints")


@register_figure(FIGURES, "spaghetti")
def spaghetti_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "longitudinal_table")
    if p is None:
        raise FileNotFoundError("manifest.longitudinal_table required")
    cols = load_tsv_columns(
        p,
        {
            "id": ("id", "subject", "subject_id", "USUBJID"),
            "time": ("time", "visit", "day", "VISIT", "AVISITN"),
            "value": ("value", "AVAL", "result", "score"),
            "group?": ("group", "arm", "treatment", "trt"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable longitudinal table: {p}")
    group_col = "group" if "group" in cols else None
    return spaghetti(
        frame=cols,
        title="Secondary endpoint — longitudinal",
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
            "label": ("label", "endpoint", "contrast"),
            "effect": ("effect", "estimate", "logHR", "logOR", "beta"),
            "ci_lo": ("ci_lo", "lower", "lcl"),
            "ci_hi": ("ci_hi", "upper", "ucl"),
            "weight?": ("weight", "n", "size"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable forest table: {p}")
    return forest(
        frame=cols, title="Secondary endpoints — forest", out=out
    )
