"""Spatial clustering stage — spatial-transcriptomics taxonomy.
Renders the per-spot tissue overlay, the Moran's I scatter for
spatially-variable genes, and the neighborhood-enrichment heatmap.

Plan reference: §S13 (spatial-transcriptomics taxonomy stage
`spatial_clustering`).

Manifest contract:
- ``coords_table``: TSV with `x`, `y`, `value`. Value is the
  per-spot statistic (cluster id, gene expression, spatial-domain
  membership) to encode by colormap.
- ``image``: optional path to a tissue image (PNG/JPG) to draw as
  the underlay. When absent, falls back to a clean scatter without
  background.
- ``morans_i_table``: TSV with `gene`, `morans_i`, `p_value`.
- ``neighborhood_table``: TSV with `source`, `target`, `score`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

import numpy as np

from ..core import (
    FigureContext,
    morans_i_scatter,
    neighborhood_enrichment,
    register_figure,
    stage_registry,
    tissue_overlay,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("spatial_clustering")


def _load_image(ctx: FigureContext) -> Optional[np.ndarray]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "image")
    if p is None:
        return None
    try:
        from matplotlib.image import imread

        return imread(p)
    except (ImportError, OSError):
        return None


@register_figure(FIGURES, "tissue_overlay")
def tissue_overlay_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "coords_table")
    if p is None:
        raise FileNotFoundError("manifest.coords_table required")
    cols = load_tsv_columns(
        p,
        {
            "x": ("x", "x_pixel", "imagecol"),
            "y": ("y", "y_pixel", "imagerow"),
            "value": ("value", "cluster", "domain", "expression"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable coords table: {p}")
    image = _load_image(ctx)
    return tissue_overlay(
        coords_df=cols,
        image=image,
        title="Tissue overlay",
        out=out,
    )


@register_figure(FIGURES, "morans_i_scatter")
def morans_i_scatter_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "morans_i_table")
    if p is None:
        raise FileNotFoundError("manifest.morans_i_table required")
    cols = load_tsv_columns(
        p,
        {
            "gene": ("gene", "feature", "symbol"),
            "morans_i": ("morans_i", "I", "morans"),
            "p_value": ("p_value", "pvalue", "p", "morans_p"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable morans_i table: {p}")
    return morans_i_scatter(
        frame=cols, title="Moran's I", out=out
    )


@register_figure(FIGURES, "neighborhood_enrichment")
def neighborhood_enrichment_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "neighborhood_table")
    if p is None:
        raise FileNotFoundError("manifest.neighborhood_table required")
    cols = load_tsv_columns(
        p,
        {
            "source": ("source", "source_type", "from"),
            "target": ("target", "target_type", "to"),
            "score": ("score", "z", "enrichment"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable neighborhood table: {p}")
    return neighborhood_enrichment(
        frame=cols, title="Neighborhood enrichment", out=out
    )
