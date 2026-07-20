"""Reporting-stage figures for cross-stage and cross-omics summaries.

Manifest contract:
- manifest.json may include `concordance_matrix` as a 2-D numeric array,
  `row_labels`, `col_labels`, and/or `pathway_overlap` as
  [{label, count}].
- Each `pathway_overlap` entry may additionally carry an OPTIONAL
  direction/NES field (`nes`/`NES`/`normalized_enrichment_score`/
  `score`, sign-only, or a string `direction`/`regulation`/`sign` such
  as "up"/"down"/"enriched"/"depleted"). When present, enriched
  (direction > 0) and depleted (direction < 0) entries render in
  distinct diverging colors instead of a single flat color. This is
  purely additive — `[{label, count}]` with no direction field still
  renders exactly as before.
- When absent, the renderer derives a compact placeholder from upstream
  figure manifests so required report figures still become explicit
  artifacts instead of silently disappearing.
"""

from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import matplotlib.pyplot as plt
import numpy as np
from matplotlib.patches import Patch

from ..core import THEME, FigureContext, bar, heatmap, register_figure, savefig, stage_registry


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


def _direction_sign(item: Dict[str, Any]) -> Optional[float]:
    """Extract a signed enrichment-direction value from an overlap entry.

    A numeric NES-like field's sign carries the direction (`nes` > 0 =
    enriched, `nes` < 0 = depleted); a string `direction`/`regulation`/
    `sign` field maps "up"/"enriched"/... to +1 and "down"/"depleted"/...
    to -1. Returns `None` when the entry carries no directional signal
    at all, so legacy `[{label, count}]` inputs (the pre-existing
    contract) are unaffected.
    """
    for key in ("nes", "NES", "normalized_enrichment_score", "score"):
        if key in item:
            try:
                return float(item[key])
            except (TypeError, ValueError):
                continue
    for key in ("direction", "regulation", "sign"):
        if key in item:
            raw = str(item[key]).strip().lower()
            if raw in ("up", "enriched", "increased", "positive", "+", "+1"):
                return 1.0
            if raw in ("down", "depleted", "decreased", "negative", "-", "-1"):
                return -1.0
    return None


def _overlap(ctx: FigureContext) -> Optional[List[Dict[str, Any]]]:
    """Parse `manifest.pathway_overlap`/`.overlap` into
    `{label, count, direction}` rows. `direction` (Optional[float], sign
    only significant) is the RP-6 NES channel — `None` when an entry
    carries no directional signal, preserving the legacy
    `[{label, count}]` rendering path."""
    entries = ctx.manifest.get("pathway_overlap") or ctx.manifest.get("overlap")
    if not isinstance(entries, list):
        return None
    out: Dict[str, Dict[str, Any]] = {}
    for item in entries:
        if not isinstance(item, dict):
            continue
        label = str(item.get("label") or item.get("term") or item.get("id") or "")
        if not label:
            continue
        value = item.get("count", item.get("n", item.get("overlap", 0)))
        try:
            count = float(value)
        except (TypeError, ValueError):
            continue
        out[label] = {"label": label, "count": count, "direction": _direction_sign(item)}
    return list(out.values()) or None


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


def _diverging_overlap_bar(
    names: List[str],
    values: List[float],
    directions: List[Optional[float]],
    out: Path,
) -> Path:
    """Diverging-color variant of the overlap bar: entries with a
    positive direction (enriched, e.g. NES > 0) draw in the theme's
    up-regulation color, negative-direction entries (depleted, NES < 0)
    in the down-regulation color, and entries reporting no direction at
    all fall back to a neutral grey — the same up/down convention
    `volcano`/`ma_plot` use, so an enriched and a depleted set are never
    visually identical (RP-6)."""
    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    sig_down = palette.get("sig_down", "#0072B2")
    non_sig = palette.get("non_sig", "#999999")
    colors = [
        sig_up if (d is not None and d > 0)
        else sig_down if (d is not None and d < 0)
        else non_sig
        for d in directions
    ]
    n = len(names)
    horizontal = n > 12
    positions = np.arange(n)
    fig, ax = plt.subplots(figsize=(8.0, 5.0))
    if horizontal:
        ax.barh(positions, values, color=colors)
        ax.set_yticks(positions)
        ax.set_yticklabels(names)
        ax.invert_yaxis()
        ax.set_xlabel("count")
    else:
        ax.bar(positions, values, color=colors)
        ax.set_xticks(positions)
        ax.set_xticklabels(names, rotation=45, ha="right")
        ax.set_ylabel("count")
    ax.set_title("Pathway or feature overlap")

    handles = []
    if any(d is not None and d > 0 for d in directions):
        handles.append(Patch(facecolor=sig_up, label="Enriched (direction > 0)"))
    if any(d is not None and d < 0 for d in directions):
        handles.append(Patch(facecolor=sig_down, label="Depleted (direction < 0)"))
    if any(d is None for d in directions):
        handles.append(Patch(facecolor=non_sig, label="No direction reported"))
    if handles:
        ax.legend(
            handles=handles,
            loc="best",
            fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
        )
    return savefig(fig, out)


@register_figure(FIGURES, "pathway_overlap_bar")
def pathway_overlap_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    overlap = _overlap(ctx)
    directions: List[Optional[float]]
    if overlap is None:
        matrix, _rows, cols = _matrix(ctx)
        values = matrix.ravel().tolist()
        names = cols if len(cols) == len(values) else [f"set_{i + 1}" for i in range(len(values))]
        directions = [None] * len(values)
    else:
        ordered = sorted(overlap, key=lambda e: e["count"], reverse=True)[:20]
        names = [e["label"] for e in ordered]
        values = [e["count"] for e in ordered]
        directions = [e["direction"] for e in ordered]
    if not names:
        raise FileNotFoundError("no overlap values available")
    if any(d is not None for d in directions):
        return _diverging_overlap_bar(names, values, directions, out)
    return bar(
        names,
        values,
        title="Pathway or feature overlap",
        ylabel="count",
        out=out,
    )
