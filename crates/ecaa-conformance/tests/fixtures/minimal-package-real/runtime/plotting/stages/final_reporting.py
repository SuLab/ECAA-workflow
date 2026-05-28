"""Final reporting — composes a summary dashboard figure + a package
inventory view. Works across all modalities because the inputs are
already-produced figure files from upstream stages (read + re-saved)
and a table of upstream manifests.

Expected inputs:
- manifest.json with `upstream: [{stage_id, figures: [{id, path, title?}]}]`
  OR is derived from scanning runtime/outputs/* dirs for figures/.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import List, Optional

import numpy as np

from ..core import (
    FigureContext,
    register_figure,
    register_view,
    savefig,
    stage_registry,
    stage_view_registry,
)
import matplotlib.image as mpimg
import matplotlib.pyplot as plt

FIGURES = stage_registry("final_reporting")
VIEWS = stage_view_registry("final_reporting")


def _discover_upstream_figures(ctx: FigureContext) -> List[tuple]:
    """Return (stage_id, figure_id, abs_path) tuples by walking
    sibling stage output dirs. Works against the package's standard
    `runtime/outputs/<stage>/figures/` layout.
    """
    # The ctx.outputs_dir is runtime/outputs/final_reporting.
    # Walk its parent for sibling stage dirs.
    outputs_root = ctx.outputs_dir.parent
    if not outputs_root.is_dir():
        return []
    found: List[tuple] = []
    for stage_dir in sorted(outputs_root.iterdir()):
        if not stage_dir.is_dir() or stage_dir == ctx.outputs_dir:
            continue
        figs_dir = stage_dir / "figures"
        manifest = figs_dir / "manifest.json"
        if not manifest.exists():
            continue
        try:
            data = json.loads(manifest.read_text())
            written = data.get("written", {})
            if isinstance(written, dict):
                for fig_id, path in written.items():
                    found.append((stage_dir.name, fig_id, Path(path)))
        except (OSError, json.JSONDecodeError):
            continue
    return found


@register_figure(FIGURES, "summary_dashboard")
def summary_dashboard(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Grid of upstream figures composited into a single PNG. Uses
    imshow to stitch existing PNGs without rerunning the underlying
    stages — cheap + deterministic since the inputs are already
    byte-stable.
    """
    figures = _discover_upstream_figures(ctx)
    if not figures:
        raise FileNotFoundError("no upstream figure manifests found")
    # Cap to 12 per dashboard to keep the output legible.
    figures = figures[:12]
    n = len(figures)
    cols = 3 if n > 4 else 2 if n > 1 else 1
    rows = int(np.ceil(n / cols))
    fig, axes = plt.subplots(rows, cols, figsize=(cols * 4.0, rows * 3.0))
    axes_flat = np.atleast_1d(axes).flatten()
    for ax, (stage, fig_id, path) in zip(axes_flat, figures):
        try:
            img = mpimg.imread(str(path))
            ax.imshow(img)
        except (OSError, ValueError):
            pass
        ax.set_title(f"{stage} · {fig_id}", fontsize=8)
        ax.set_xticks([])
        ax.set_yticks([])
    for ax in axes_flat[len(figures):]:
        ax.set_visible(False)
    fig.suptitle("Workflow summary dashboard", fontsize=12)
    fig.tight_layout()
    return savefig(fig, out)


@register_view(VIEWS, "package_summary")
def view_package_summary(ctx: FigureContext) -> dict:
    """JSON inventory of every stage's figures + status. Drives the
    interactive final summary panel that lists upstream work and
    links to each figure/view via its stored path.
    """
    found = _discover_upstream_figures(ctx)
    by_stage: dict = {}
    for stage, fig_id, path in found:
        by_stage.setdefault(stage, []).append(
            {"figure_id": fig_id, "path": str(path)}
        )
    return {
        "stages": [
            {"stage_id": stage, "figures": figs}
            for stage, figs in sorted(by_stage.items())
        ]
    }
