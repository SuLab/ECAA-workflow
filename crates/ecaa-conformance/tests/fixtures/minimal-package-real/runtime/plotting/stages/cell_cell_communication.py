"""Cell-cell communication stage — scRNA-seq (CellChat, CellPhoneDB,
liana). Renders a signaling-strength heatmap by source→target celltype
and a per-pathway bar chart.

Expected inputs:
- manifest.json with `interactions: [{source, target, n_pairs, strength}]`
  or `pathways: [{name, strength, n_interactions}]`
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, List, Optional

import numpy as np

from ..core import (
    FigureContext,
    bar,
    heatmap,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("cell_cell_communication")
VIEWS = stage_view_registry("cell_cell_communication")


def _interactions(ctx: FigureContext) -> Optional[list]:
    interactions = ctx.manifest.get("interactions")
    if isinstance(interactions, list) and interactions:
        return [i for i in interactions if isinstance(i, dict)]
    return None


def _pathways(ctx: FigureContext) -> Optional[list]:
    p = ctx.manifest.get("pathways")
    if isinstance(p, list) and p:
        return [i for i in p if isinstance(i, dict)]
    return None


@register_figure(FIGURES, "signaling_heatmap")
def signaling_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    interactions = _interactions(ctx)
    if not interactions:
        raise FileNotFoundError("manifest.interactions required")
    sources = sorted({str(i.get("source", "?")) for i in interactions})
    targets = sorted({str(i.get("target", "?")) for i in interactions})
    matrix = np.zeros((len(sources), len(targets)), dtype=float)
    source_idx = {s: i for i, s in enumerate(sources)}
    target_idx = {t: i for i, t in enumerate(targets)}
    for inter in interactions:
        s = str(inter.get("source", "?"))
        t = str(inter.get("target", "?"))
        strength = inter.get("strength") or inter.get("n_pairs") or 0.0
        try:
            matrix[source_idx[s], target_idx[t]] = float(strength)
        except (KeyError, ValueError):
            continue
    return heatmap(
        matrix=matrix,
        row_labels=sources,
        col_labels=targets,
        title="Signaling strength",
        cmap="Reds",
        center=None,
        out=out,
    )


@register_figure(FIGURES, "top_pathways")
def top_pathways(ctx: FigureContext, out: Path) -> Optional[Path]:
    pathways = _pathways(ctx)
    if not pathways:
        raise FileNotFoundError("manifest.pathways required")
    ranked = sorted(
        pathways,
        key=lambda p: -float(p.get("strength") or p.get("n_interactions") or 0),
    )[:20]
    names = [str(p.get("name", "?"))[:30] for p in ranked]
    values = [float(p.get("strength") or p.get("n_interactions") or 0.0) for p in ranked]
    return bar(
        names=names,
        values=values,
        title="Top signaling pathways",
        ylabel="strength",
        out=out,
        figsize=(9.0, max(5.0, 0.3 * len(names))),
    )


@register_view(VIEWS, "signaling_heatmap")
def view_signaling_heatmap(ctx: FigureContext) -> dict:
    interactions = _interactions(ctx)
    if not interactions:
        raise FileNotFoundError("manifest.interactions required")
    sources = sorted({str(i.get("source", "?")) for i in interactions})
    targets = sorted({str(i.get("target", "?")) for i in interactions})
    cells: Dict[str, Dict[str, float]] = {s: {t: 0.0 for t in targets} for s in sources}
    for inter in interactions:
        s = str(inter.get("source", "?"))
        t = str(inter.get("target", "?"))
        strength = float(inter.get("strength") or inter.get("n_pairs") or 0.0)
        if s in cells and t in cells[s]:
            cells[s][t] = strength
    return {
        "sources": sources,
        "targets": targets,
        "matrix": [[cells[s][t] for t in targets] for s in sources],
    }
