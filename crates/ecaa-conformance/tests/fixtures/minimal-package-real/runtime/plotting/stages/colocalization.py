"""Colocalization stage — GWAS coloc taxonomy. Renders the locus-zoom
+ Manhattan + QQ + Miami + credible-set + per-locus PP panel surface
the SAP review committee needs to confirm a colocalization signal.

Plan reference: §S12 (variant + GWAS phase F primitives wired to
the gwas-coloc taxonomy stage `colocalization`).

Manifest contract:
- ``summary_stats``: path to a TSV with chrom/pos/pvalue (+optional
  gene). Used by manhattan + qq + locus_zoom + credible_set_track.
- ``coloc_table``: path to a TSV with locus/pp.h0..pp.h4 columns
  (LD-aware coloc.abf style). Used by coloc_pp_panel.
- ``miami_top`` / ``miami_bottom``: paired summary-stats paths for
  miami plot (often case-vs-control or paired phenotype). Falls
  through to skipping the miami fig when one side absent.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Dict, Optional

import numpy as np

from ..core import (
    FigureContext,
    coloc_pp_panel,
    credible_set_track,
    locus_zoom,
    manhattan,
    miami,
    qq,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("colocalization")


def _sumstats_columns():
    return {
        "chrom": ("chrom", "chromosome", "#CHROM", "CHR"),
        "pos": ("pos", "position", "POS", "BP"),
        "pvalue": ("pvalue", "P", "p_value", "p"),
        "gene?": ("gene", "Gene", "symbol", "SNP"),
    }


def _load_sumstats(ctx: FigureContext, key: str = "summary_stats") -> Dict[str, np.ndarray]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, key)
    if p is None:
        raise FileNotFoundError(f"manifest.{key} required")
    cols = load_tsv_columns(p, _sumstats_columns())
    if cols is None:
        raise FileNotFoundError(f"unparseable summary stats: {p}")
    return cols


@register_figure(FIGURES, "manhattan")
def manhattan_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    cols = _load_sumstats(ctx)
    return manhattan(frame=cols, title="Manhattan", out=out)


@register_figure(FIGURES, "qq")
def qq_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    cols = _load_sumstats(ctx)
    return qq(frame=cols, title="QQ", out=out)


@register_figure(FIGURES, "miami")
def miami_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    top = manifest_path(ctx.manifest, ctx.outputs_dir, "miami_top")
    bottom = manifest_path(ctx.manifest, ctx.outputs_dir, "miami_bottom")
    if top is None or bottom is None:
        raise FileNotFoundError("manifest.miami_top + miami_bottom required")
    top_cols = load_tsv_columns(top, _sumstats_columns())
    bottom_cols = load_tsv_columns(bottom, _sumstats_columns())
    if top_cols is None or bottom_cols is None:
        raise FileNotFoundError("unparseable miami summary stats")
    return miami(top=top_cols, bottom=bottom_cols, title="Miami", out=out)


@register_figure(FIGURES, "locus_zoom")
def locus_zoom_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    cols = _load_sumstats(ctx)
    if "neg_log10_p" not in cols and "pvalue" in cols:
        with np.errstate(divide="ignore", invalid="ignore"):
            cols["neg_log10_p"] = -np.log10(np.clip(cols["pvalue"], 1e-300, 1.0))
    return locus_zoom(frame=cols, title="Locus zoom", out=out)


@register_figure(FIGURES, "credible_set_track")
def credible_set_track_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "credible_set_table")
    if p is None:
        # Fall back to summary stats with posterior heuristic when no
        # dedicated credible-set table — use 1/(1+e^z²) as a uniform
        # rank substitute so the figure still surfaces signal shape.
        sum_cols = _load_sumstats(ctx)
        nlogp = sum_cols.get("neg_log10_p")
        if nlogp is None:
            with np.errstate(divide="ignore", invalid="ignore"):
                nlogp = -np.log10(np.clip(sum_cols["pvalue"], 1e-300, 1.0))
        z = np.sqrt(2.0 * np.log(10.0)) * nlogp.astype(float)
        post = 1.0 / (1.0 + np.exp(-z * z / 2.0))
        post = post / max(post.sum(), 1.0)
        return credible_set_track(
            frame={"pos": sum_cols["pos"], "posterior": post},
            title="Credible set",
            out=out,
        )
    cols = load_tsv_columns(
        p,
        {
            "pos": ("pos", "position", "POS"),
            "posterior": ("posterior", "PIP", "pip", "post_prob"),
            "credible_set?": ("credible_set", "in_cs", "cs"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable credible-set table: {p}")
    return credible_set_track(frame=cols, title="Credible set", out=out)


@register_figure(FIGURES, "coloc_pp_panel")
def coloc_pp_panel_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "coloc_table")
    if p is None:
        raise FileNotFoundError("manifest.coloc_table required")
    cols = load_tsv_columns(
        p,
        {
            "region": ("region", "locus", "id"),
            "pp_h0": ("pp_h0", "PP.H0", "h0"),
            "pp_h1": ("pp_h1", "PP.H1", "h1"),
            "pp_h2": ("pp_h2", "PP.H2", "h2"),
            "pp_h3": ("pp_h3", "PP.H3", "h3"),
            "pp_h4": ("pp_h4", "PP.H4", "h4"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable coloc table: {p}")
    return coloc_pp_panel(frame=cols, title="Colocalization PP", out=out)
