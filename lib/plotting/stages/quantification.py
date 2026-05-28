"""Quantification stage — proteomics taxonomy. Renders peptide
coverage of the target proteins and a ridgeline plot of ion-intensity
distributions across samples.

Plan reference: §S13 (proteomics taxonomy stage `quantification`).

Manifest contract:
- ``coverage_table``: TSV with `position`, `coverage`. Optional
  `protein` column when multiple targets — when present, the first
  protein is rendered (extension to multi-protein follow-on).
- ``intensity_table``: TSV with `group`, `value`. Group = sample id;
  value = log-ion-intensity per peptide.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    peptide_coverage,
    register_figure,
    ridgeline,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("quantification")


@register_figure(FIGURES, "peptide_coverage")
def peptide_coverage_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "coverage_table")
    if p is None:
        raise FileNotFoundError("manifest.coverage_table required")
    cols = load_tsv_columns(
        p,
        {
            "position": ("position", "residue", "aa_pos"),
            "coverage": ("coverage", "depth", "n_peptides"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable coverage table: {p}")
    return peptide_coverage(
        frame=cols, title="Peptide coverage", out=out
    )


@register_figure(FIGURES, "ridgeline")
def ridgeline_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "intensity_table")
    if p is None:
        raise FileNotFoundError("manifest.intensity_table required")
    cols = load_tsv_columns(
        p,
        {
            "group": ("group", "sample", "run", "channel"),
            "value": ("value", "intensity", "log_intensity", "abundance"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable intensity table: {p}")
    return ridgeline(
        frame=cols, title="Ion-intensity ridgeline", out=out
    )
