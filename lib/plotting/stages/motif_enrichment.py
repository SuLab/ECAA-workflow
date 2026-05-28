"""Motif-enrichment stage figures for peak-set analyses.

Manifest contract:
- ``motif_table`` or ``enriched_motifs_table`` pointing to a TSV with
  motif id/name, adjusted p-value, fold enrichment, and optional
  consensus sequence.
- Fallback files: ``enriched_motifs.tsv`` or ``motifs.tsv``.
"""

from __future__ import annotations

from pathlib import Path
from typing import List, Optional

import numpy as np

from ..core import FigureContext, bar, heatmap, register_figure, stage_registry
from ._shared import find_col, manifest_path, open_text

FIGURES = stage_registry("motif_enrichment")

_MOTIF_COLS = ("motif_id", "motif", "id", "name")
_FAMILY_COLS = ("family", "tf_family", "class")
_P_COLS = ("adj_p_value", "adj_pvalue", "padj", "fdr", "p_value", "pvalue")
_FOLD_COLS = ("fold_enrichment", "enrichment", "fold_change", "odds_ratio")
_CONSENSUS_COLS = ("consensus", "sequence", "motif_consensus")


def _motif_table(ctx: FigureContext) -> Path:
    for key in ("motif_table", "enriched_motifs_table"):
        path = manifest_path(ctx.manifest, ctx.outputs_dir, key)
        if path is not None:
            return path
    for name in ("enriched_motifs.tsv", "motifs.tsv"):
        path = ctx.outputs_dir / name
        if path.exists():
            return path
    raise FileNotFoundError("manifest.motif_table or enriched_motifs.tsv required")


def _load_rows(ctx: FigureContext) -> List[dict]:
    path = _motif_table(ctx)
    rows: List[dict] = []
    try:
        with open_text(path) as f:
            header = f.readline().rstrip("\n").split("\t")
            i_motif = find_col(header, _MOTIF_COLS)
            i_family = find_col(header, _FAMILY_COLS)
            i_p = find_col(header, _P_COLS)
            i_fold = find_col(header, _FOLD_COLS)
            i_consensus = find_col(header, _CONSENSUS_COLS)
            if i_motif is None or i_p is None:
                raise FileNotFoundError(f"unparseable motif table: {path}")
            max_idx = max(i for i in (i_motif, i_family, i_p, i_fold, i_consensus) if i is not None)
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) <= max_idx:
                    continue
                try:
                    p_value = float(parts[i_p])
                    fold = float(parts[i_fold]) if i_fold is not None else 1.0
                except ValueError:
                    continue
                rows.append(
                    {
                        "motif": parts[i_motif],
                        "family": parts[i_family] if i_family is not None else "",
                        "p_value": max(p_value, 1e-300),
                        "fold": fold,
                        "consensus": parts[i_consensus] if i_consensus is not None else "",
                    }
                )
    except OSError as exc:
        raise FileNotFoundError(f"unreadable motif table: {path}") from exc
    if not rows:
        raise FileNotFoundError(f"no parseable motif rows in {path}")
    rows.sort(
        key=lambda row: (-np.log10(float(row["p_value"])) * max(float(row["fold"]), 1.0)),
        reverse=True,
    )
    return rows


@register_figure(FIGURES, "motif_enrichment_table")
def motif_enrichment_table(ctx: FigureContext, out: Path) -> Optional[Path]:
    rows = _load_rows(ctx)[:20]
    names = [
        f"{row['motif']} ({row['family']})" if row["family"] else str(row["motif"])
        for row in rows
    ]
    values = [-np.log10(float(row["p_value"])) for row in rows]
    return bar(
        names,
        values,
        title="Top enriched motifs",
        ylabel="-log10(adjusted p)",
        out=out,
        figsize=(9.0, max(5.0, 0.3 * len(names))),
    )


@register_figure(FIGURES, "motif_logo_grid")
def motif_logo_grid(ctx: FigureContext, out: Path) -> Optional[Path]:
    rows = [row for row in _load_rows(ctx) if row.get("consensus")][:6]
    if not rows:
        raise FileNotFoundError("motif_logo_grid requires a consensus sequence column")
    max_len = max(len(str(row["consensus"])) for row in rows)
    bases = ["A", "C", "G", "T"]
    matrix = np.zeros((len(rows) * len(bases), max_len), dtype=float)
    row_labels: List[str] = []
    for motif_idx, row in enumerate(rows):
        consensus = str(row["consensus"]).upper()
        for base_idx, base in enumerate(bases):
            row_labels.append(f"{row['motif']}:{base}")
            for pos_idx, observed in enumerate(consensus):
                matrix[motif_idx * len(bases) + base_idx, pos_idx] = (
                    2.0 if observed == base else 0.05
                )
    return heatmap(
        matrix,
        row_labels=row_labels,
        col_labels=[str(i + 1) for i in range(max_len)],
        title="Top motif consensus logos",
        out=out,
        center=None,
        cluster_rows=False,
        cluster_cols=False,
        cbar_label="information content",
        figsize=(max(6.0, 0.45 * max_len), max(6.0, 0.18 * len(row_labels))),
    )
