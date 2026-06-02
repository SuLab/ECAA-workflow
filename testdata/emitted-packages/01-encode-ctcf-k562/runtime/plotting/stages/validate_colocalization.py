"""Validate colocalization stage — re-runs the coloc PP panel as a
QC gate so the SAP review committee can confirm the upstream
``colocalization`` result reproduces under the validation harness.

Plan reference: §S12 (gwas-coloc taxonomy stage `validate_colocalization`).

Manifest contract (same shape as `colocalization`):
- ``coloc_table``: path to a TSV with locus/pp.h0..pp.h4 columns.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    coloc_pp_panel,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("validate_colocalization")


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
    return coloc_pp_panel(
        frame=cols, title="Colocalization validation", out=out
    )
