"""Subgroup analyses stage — clinical-trial taxonomy. Renders a forest
plot across the SAP-listed subgroups with interaction tests visible
through the marker color and CI excludes-null annotation.

Plan reference: §S12 (clinical-trial-analysis stage `subgroup_analyses`).

Manifest contract:
- ``forest_table``: TSV with `label`, `effect`, `ci_lo`, `ci_hi`,
  optional `weight`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    forest,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("subgroup_analyses")


@register_figure(FIGURES, "forest")
def forest_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "forest_table")
    if p is None:
        raise FileNotFoundError("manifest.forest_table required")
    cols = load_tsv_columns(
        p,
        {
            "label": ("label", "subgroup", "stratum"),
            "effect": ("effect", "estimate", "logHR", "logOR", "beta"),
            "ci_lo": ("ci_lo", "lower", "lcl"),
            "ci_hi": ("ci_hi", "upper", "ucl"),
            "weight?": ("weight", "n", "size"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable forest table: {p}")
    return forest(
        frame=cols, title="Subgroup analyses", out=out
    )
