"""Reporting-stage figures for cross-stage and cross-omics summaries.

Manifest contract:
- manifest.json may include `concordance_matrix` as a 2-D numeric array,
  `row_labels`, `col_labels`, and/or `pathway_overlap` as
  [{label, count}].
- When absent, the renderer derives a compact placeholder from upstream
  figure manifests so required report figures still become explicit
  artifacts instead of silently disappearing.
"""

from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import FigureContext, bar, heatmap, register_figure, stage_registry


FIGURES = stage_registry("reporting")


def _matrix(ctx: FigureContext) -> Tuple[np.ndarray, List[str], List[str]]:
    matrix = ctx.manifest.get("concordance_matrix")
    if isinstance(matrix, list) and matrix:
        arr = np.asarray(matrix, dtype=float)
        rows = [str(x) for x in ctx.manifest.get("row_labels", [])]
        cols = [str(x) for x in ctx.manifest.get("col_labels", [])]
        if arr.ndim == 2 and arr.shape[0] > 0 and arr.shape[1] > 0:
            if len(rows) != arr.shape[0]:
                rows = [f"row_{i + 1}" for i in range(arr.shape[0])]
            if len(cols) != arr.shape[1]:
                cols = [f"col_{i + 1}" for i in range(arr.shape[1])]
            return arr, rows, cols

    upstream = ctx.manifest.get("upstream") or []
    labels: List[str] = []
    counts: List[float] = []
    if isinstance(upstream, list):
        for item in upstream:
            if not isinstance(item, dict):
                continue
            label = str(item.get("stage_id") or item.get("id") or f"stage_{len(labels) + 1}")
            figures = item.get("figures") or []
            labels.append(label)
            counts.append(float(len(figures) if isinstance(figures, list) else 0))
    if labels:
        arr = np.asarray([counts], dtype=float)
        return arr, ["figures"], labels
    raise FileNotFoundError("manifest.concordance_matrix or manifest.upstream required")


def _overlap(ctx: FigureContext) -> Optional[Dict[str, float]]:
    entries = ctx.manifest.get("pathway_overlap") or ctx.manifest.get("overlap")
    if not isinstance(entries, list):
        return None
    out: Dict[str, float] = {}
    for item in entries:
        if not isinstance(item, dict):
            continue
        label = str(item.get("label") or item.get("term") or item.get("id") or "")
        if not label:
            continue
        value = item.get("count", item.get("n", item.get("overlap", 0)))
        try:
            out[label] = float(value)
        except (TypeError, ValueError):
            continue
    return out or None


@register_figure(FIGURES, "concordance_heatmap")
def concordance_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    matrix, rows, cols = _matrix(ctx)
    return heatmap(
        matrix,
        row_labels=rows,
        col_labels=cols,
        title="Cross-stage concordance",
        out=out,
        center=None,
        cluster_rows=False,
        cluster_cols=False,
        cbar_label="score",
    )


@register_figure(FIGURES, "pathway_overlap_bar")
def pathway_overlap_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    overlap = _overlap(ctx)
    if overlap is None:
        matrix, _rows, cols = _matrix(ctx)
        values = matrix.ravel().tolist()
        names = cols if len(cols) == len(values) else [f"set_{i + 1}" for i in range(len(values))]
    else:
        ordered = sorted(overlap.items(), key=lambda kv: kv[1], reverse=True)[:20]
        names = [k for k, _ in ordered]
        values = [v for _, v in ordered]
    if not names:
        raise FileNotFoundError("no overlap values available")
    return bar(
        names,
        values,
        title="Pathway or feature overlap",
        ylabel="count",
        out=out,
    )
