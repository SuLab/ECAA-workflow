"""Peak calling stage — chip-seq + atac-seq taxonomies. Renders the
profile pileup around peak summits, an IGV-style coverage track for a
representative locus, and a saturation curve to confirm the read
depth was sufficient.

Plan reference: §S13 (chip-seq + atac-seq taxonomy stage `peak_calling`).

Manifest contract:
- ``profile_table``: TSV with `position`, `signal`, optional `group`.
- ``coverage_table``: TSV with `chrom`, `pos`, `depth`.
- ``saturation_table``: TSV with `depth`, `peaks_called`, optional `group`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    coverage_track,
    peak_saturation,
    profile_pileup,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("peak_calling")


@register_figure(FIGURES, "profile_pileup")
def profile_pileup_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "profile_table")
    if p is None:
        raise FileNotFoundError("manifest.profile_table required")
    cols = load_tsv_columns(
        p,
        {
            "position": ("position", "distance", "offset"),
            "signal": ("signal", "coverage", "rpkm", "fold_enrichment"),
            "group?": ("group", "antibody", "sample", "condition"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable profile table: {p}")
    group_col = "group" if "group" in cols else None
    return profile_pileup(
        frame=cols,
        title="Profile pileup",
        out=out,
        group_col=group_col,
    )


@register_figure(FIGURES, "coverage_track")
def coverage_track_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "coverage_table")
    if p is None:
        raise FileNotFoundError("manifest.coverage_table required")
    cols = load_tsv_columns(
        p,
        {
            "chrom": ("chrom", "chromosome", "#CHROM"),
            "pos": ("pos", "position", "POS"),
            "depth": ("depth", "coverage", "n_reads"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable coverage table: {p}")
    return coverage_track(frame=cols, title="Coverage track", out=out)


@register_figure(FIGURES, "peak_saturation")
def peak_saturation_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "saturation_table")
    if p is None:
        raise FileNotFoundError("manifest.saturation_table required")
    cols = load_tsv_columns(
        p,
        {
            "depth": ("depth", "subsampled_reads", "n_reads"),
            "peaks_called": ("peaks_called", "n_peaks", "peaks"),
            "group?": ("group", "sample", "replicate"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable saturation table: {p}")
    group_col = "group" if "group" in cols else None
    return peak_saturation(
        frame=cols,
        title="Peak saturation",
        out=out,
        group_col=group_col,
    )
