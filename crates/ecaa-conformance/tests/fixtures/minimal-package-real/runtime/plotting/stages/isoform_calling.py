"""Isoform calling stage — long-read RNA-seq taxonomy. Renders the
transcript structure plot for the top isoform models and a sashimi
plot of splice junctions in a representative locus.

Plan reference: §S13 (long-read-rnaseq taxonomy stage `isoform_calling`).

Manifest contract:
- ``isoform_table``: TSV with `transcript`, `exon_starts`, `exon_ends`,
  optional `strand`. exon_starts/ends are comma-separated lists of ints.
- ``junction_table``: TSV with `junction` ("start-end" string), `count`.
"""

from __future__ import annotations

from pathlib import Path
from typing import List, Optional

from ..core import (
    FigureContext,
    isoform_structure,
    register_figure,
    sashimi,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path, open_text

FIGURES = stage_registry("isoform_calling")


def _parse_int_list(s: str) -> List[int]:
    if not s:
        return []
    return [int(x) for x in s.split(",") if x.strip()]


def _load_isoforms(p: Path) -> Optional[dict]:
    """Custom loader for isoform tables — load_tsv_columns can't handle
    the comma-list cells, so we parse them inline.
    """
    transcripts: List[str] = []
    exon_starts: List[List[int]] = []
    exon_ends: List[List[int]] = []
    strand: List[str] = []
    try:
        with open_text(p) as f:
            header = f.readline().rstrip("\n").split("\t")
            t_idx = next(
                (i for i, h in enumerate(header)
                 if h.lower() in ("transcript", "transcript_id", "tx")),
                None,
            )
            s_idx = next(
                (i for i, h in enumerate(header)
                 if h.lower() in ("exon_starts", "starts", "blockstarts")),
                None,
            )
            e_idx = next(
                (i for i, h in enumerate(header)
                 if h.lower() in ("exon_ends", "ends", "blockends")),
                None,
            )
            sd_idx = next(
                (i for i, h in enumerate(header) if h.lower() in ("strand",)),
                None,
            )
            if t_idx is None or s_idx is None or e_idx is None:
                return None
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) <= max(t_idx, s_idx, e_idx):
                    continue
                transcripts.append(parts[t_idx])
                exon_starts.append(_parse_int_list(parts[s_idx]))
                exon_ends.append(_parse_int_list(parts[e_idx]))
                strand.append(parts[sd_idx] if sd_idx is not None and sd_idx < len(parts) else "+")
    except OSError:
        return None
    if not transcripts:
        return None
    return {
        "transcript": transcripts,
        "exon_starts": exon_starts,
        "exon_ends": exon_ends,
        "strand": strand,
    }


@register_figure(FIGURES, "isoform_structure")
def isoform_structure_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "isoform_table")
    if p is None:
        raise FileNotFoundError("manifest.isoform_table required")
    cols = _load_isoforms(p)
    if cols is None:
        raise FileNotFoundError(f"unparseable isoform table: {p}")
    return isoform_structure(
        frame=cols,
        title="Isoform structure",
        out=out,
        strand_col="strand",
    )


@register_figure(FIGURES, "sashimi")
def sashimi_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "junction_table")
    if p is None:
        raise FileNotFoundError("manifest.junction_table required")
    cols = load_tsv_columns(
        p,
        {
            "junction": ("junction", "intron", "id"),
            "count": ("count", "n_reads", "support"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable junction table: {p}")
    return sashimi(frame=cols, title="Splice-junction sashimi", out=out)
