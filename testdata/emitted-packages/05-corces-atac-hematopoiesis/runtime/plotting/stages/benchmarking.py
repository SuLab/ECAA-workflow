"""Benchmarking stage — gwas-coloc + clinical-trial taxonomies.
Renders a forest plot of concordance against the declared reference
(per-locus agreement, Jaccard / rank-correlation, ...) so an SME can
see at a glance which loci diverge from the published reference.

Plan reference: §S12 (gwas-coloc stage `benchmarking`).

Manifest contract:
- ``forest_table``: TSV with `label`, `effect`, `ci_lo`, `ci_hi`,
  optional `weight`. Effect = concordance score; CI from bootstrap
  resampling.
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

FIGURES = stage_registry("benchmarking")


@register_figure(FIGURES, "forest")
def forest_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "forest_table")
    if p is None:
        raise FileNotFoundError("manifest.forest_table required")
    cols = load_tsv_columns(
        p,
        {
            "label": ("label", "locus", "metric"),
            "effect": ("effect", "concordance", "jaccard", "rho"),
            "ci_lo": ("ci_lo", "lower", "lcl"),
            "ci_hi": ("ci_hi", "upper", "ucl"),
            "weight?": ("weight", "n", "size"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable benchmark forest table: {p}")
    return forest(
        frame=cols,
        title="Benchmarking concordance",
        out=out,
        xlabel="concordance (95% CI)",
    )
