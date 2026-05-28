"""Taxonomic profiling stage — metagenomics taxonomy. Renders a
stacked relative-abundance bar across samples + a violin of alpha
diversity (Shannon, Simpson, etc.).

Plan reference: §S13 (metagenomics taxonomy stage `taxonomic_profiling`).

Manifest contract:
- ``abundance_table``: TSV with `sample`, `taxon`, `abundance`.
- ``diversity_table``: TSV with `group`, `value`. Group = sample
  cohort; value = per-sample diversity index.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    diversity_violin,
    register_figure,
    stage_registry,
    taxonomic_stacked_bar,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("taxonomic_profiling")


@register_figure(FIGURES, "taxonomic_stacked_bar")
def taxonomic_stacked_bar_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "abundance_table")
    if p is None:
        raise FileNotFoundError("manifest.abundance_table required")
    cols = load_tsv_columns(
        p,
        {
            "sample": ("sample", "sample_id", "subject"),
            "taxon": ("taxon", "species", "genus", "family"),
            "abundance": ("abundance", "count", "rel_abundance", "fraction"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable abundance table: {p}")
    return taxonomic_stacked_bar(
        frame=cols, title="Taxonomic composition", out=out
    )


@register_figure(FIGURES, "diversity_violin")
def diversity_violin_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "diversity_table")
    if p is None:
        raise FileNotFoundError("manifest.diversity_table required")
    cols = load_tsv_columns(
        p,
        {
            "group": ("group", "cohort", "condition"),
            "value": ("value", "shannon", "simpson", "diversity"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable diversity table: {p}")
    return diversity_violin(
        frame=cols, title="Alpha diversity", out=out
    )
