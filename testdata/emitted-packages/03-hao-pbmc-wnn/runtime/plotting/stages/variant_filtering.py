"""Variant filtering stage — VQSR / hard-filter / DeepVariant tranche
review. Renders the post-filter Manhattan + QQ pair so the SME can
confirm filter aggressiveness didn't drop a real signal cluster.

Plan reference: §S12 (variant-calling taxonomy stage `variant_filtering`).

Manifest contract:
- ``summary_stats``: path to a TSV with chrom/pos/pvalue.
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, Optional

import numpy as np

from ..core import (
    FigureContext,
    manhattan,
    qq,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("variant_filtering")


def _load_sumstats(ctx: FigureContext) -> Dict[str, np.ndarray]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "summary_stats")
    if p is None:
        raise FileNotFoundError("manifest.summary_stats required")
    cols = load_tsv_columns(
        p,
        {
            "chrom": ("chrom", "chromosome", "#CHROM", "CHR"),
            "pos": ("pos", "position", "POS", "BP"),
            "pvalue": ("pvalue", "P", "p_value", "p"),
            "gene?": ("gene", "Gene", "symbol", "SNP"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable summary stats: {p}")
    return cols


@register_figure(FIGURES, "manhattan")
def manhattan_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    return manhattan(
        frame=_load_sumstats(ctx), title="Variants — post-filter Manhattan", out=out
    )


@register_figure(FIGURES, "qq")
def qq_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    return qq(
        frame=_load_sumstats(ctx), title="Variants — post-filter QQ", out=out
    )
