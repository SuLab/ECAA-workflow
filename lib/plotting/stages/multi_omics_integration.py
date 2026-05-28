"""Shared figures for multi-omics integration atoms.

Manifest contract:
- manifest.json may include `modality_concordance_matrix` with
  `row_labels` and `col_labels`.
- manifest.json may include `factor_variance` as [{factor, variance}]
  or a mapping of factor -> variance.
"""

from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import FigureContext, bar, heatmap, register_figure, stage_registry


FIGURES = stage_registry("multi_omics_integration")


def _concordance_matrix(ctx: FigureContext) -> Tuple[np.ndarray, List[str], List[str]]:
    matrix = ctx.manifest.get("modality_concordance_matrix") or ctx.manifest.get(
        "concordance_matrix"
    )
    if isinstance(matrix, list) and matrix:
        arr = np.asarray(matrix, dtype=float)
        if arr.ndim == 2 and arr.shape[0] > 0 and arr.shape[1] > 0:
            rows = [str(x) for x in ctx.manifest.get("row_labels", [])]
            cols = [str(x) for x in ctx.manifest.get("col_labels", [])]
            if len(rows) != arr.shape[0]:
                rows = [f"modality_{i + 1}" for i in range(arr.shape[0])]
            if len(cols) != arr.shape[1]:
                cols = [f"modality_{i + 1}" for i in range(arr.shape[1])]
            return arr, rows, cols

    modalities = ctx.manifest.get("modalities")
    if isinstance(modalities, list) and len(modalities) >= 2:
        labels = [str(m.get("id", m)) if isinstance(m, dict) else str(m) for m in modalities]
        arr = np.eye(len(labels), dtype=float)
        return arr, labels, labels
    raise FileNotFoundError("manifest.modality_concordance_matrix required")


def _factor_variance(ctx: FigureContext) -> Optional[Dict[str, float]]:
    raw = ctx.manifest.get("factor_variance") or ctx.manifest.get("variance_partition")
    if isinstance(raw, dict):
        out: Dict[str, float] = {}
        for key, value in raw.items():
            try:
                out[str(key)] = float(value)
            except (TypeError, ValueError):
                continue
        return out or None
    if isinstance(raw, list):
        out = {}
        for item in raw:
            if not isinstance(item, dict):
                continue
            label = str(item.get("factor") or item.get("component") or item.get("id") or "")
            if not label:
                continue
            value = item.get("variance", item.get("variance_explained", item.get("value", 0)))
            try:
                out[label] = float(value)
            except (TypeError, ValueError):
                continue
        return out or None
    return None


@register_figure(FIGURES, "modality_concordance_heatmap")
def modality_concordance_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    matrix, rows, cols = _concordance_matrix(ctx)
    return heatmap(
        matrix,
        row_labels=rows,
        col_labels=cols,
        title="Modality concordance",
        out=out,
        center=0.0,
        cluster_rows=False,
        cluster_cols=False,
        cbar_label="association",
    )


@register_figure(FIGURES, "factor_variance_bar")
def factor_variance_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    values = _factor_variance(ctx)
    if values is None:
        matrix, _rows, cols = _concordance_matrix(ctx)
        means = np.nanmean(np.abs(matrix), axis=0)
        names = cols
        vals = means.tolist()
    else:
        ordered = sorted(values.items(), key=lambda kv: kv[1], reverse=True)[:20]
        names = [k for k, _ in ordered]
        vals = [v for _, v in ordered]
    if not names:
        raise FileNotFoundError("no factor variance values available")
    return bar(
        names,
        vals,
        title="Factor variance explained",
        ylabel="variance explained",
        out=out,
    )
