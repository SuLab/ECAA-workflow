"""Trajectory-analysis stage — primarily scRNA-seq (scanpy, monocle,
slingshot). Reads per-compartment pseudotime assignments + UMAP
coordinates. The pattern (continuous scalar colored over a 2-D
embedding) generalizes to spatial pseudogradients too.

Expected inputs:
- manifest.json with `runs: [{id, pseudotime_path}]`
- <run>/pseudotime.tsv[.gz] with columns {cell, pseudotime[, x, y, branch]}
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    register_figure,
    register_view,
    scatter,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("trajectory_analysis")
VIEWS = stage_view_registry("trajectory_analysis")

_SUBSAMPLE_CAP = 50_000


def _iter_runs(ctx: FigureContext) -> List[dict]:
    runs = ctx.manifest.get("runs") or ctx.manifest.get("compartments")
    if isinstance(runs, list) and runs:
        return runs
    return [{"id": "run"}]


def _load_pseudotime(
    ctx: FigureContext, run: dict
) -> Optional[Tuple[np.ndarray, np.ndarray, np.ndarray, np.ndarray]]:
    run_id = run.get("id", "run")
    for p in (
        ctx.outputs_dir / run_id / "pseudotime.tsv.gz",
        ctx.outputs_dir / run_id / "pseudotime.tsv",
        ctx.outputs_dir / "pseudotime.tsv.gz",
        ctx.outputs_dir / "pseudotime.tsv",
    ):
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                try:
                    i_pt = header.index("pseudotime")
                except ValueError:
                    continue
                i_x = header.index("x") if "x" in header else None
                i_y = header.index("y") if "y" in header else None
                i_b = header.index("branch") if "branch" in header else None
                pts: List[float] = []
                xs: List[float] = []
                ys: List[float] = []
                branches: List[str] = []
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= i_pt:
                        continue
                    try:
                        pts.append(float(parts[i_pt]))
                    except ValueError:
                        continue
                    if i_x is not None and i_y is not None and len(parts) > max(i_x, i_y):
                        try:
                            xs.append(float(parts[i_x]))
                            ys.append(float(parts[i_y]))
                        except ValueError:
                            xs.append(0.0)
                            ys.append(0.0)
                    else:
                        xs.append(0.0)
                        ys.append(0.0)
                    branches.append(parts[i_b] if i_b is not None and len(parts) > i_b else "")
            if not pts:
                return None
            return (
                np.asarray(pts, dtype=float),
                np.asarray(xs, dtype=float),
                np.asarray(ys, dtype=float),
                np.asarray(branches, dtype=object),
            )
        except OSError:
            continue
    return None


@register_figure(FIGURES, "pseudotime_umap")
def pseudotime_umap(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = _iter_runs(ctx)
    for run in runs:
        loaded = _load_pseudotime(ctx, run)
        if loaded is None:
            continue
        pts, xs, ys, _branches = loaded
        if len(pts) == 0 or not (np.any(xs != 0.0) or np.any(ys != 0.0)):
            continue
        return scatter(
            x=xs,
            y=ys,
            color=pts,
            cmap="viridis",
            title=f"Pseudotime — {run.get('id','run')}",
            xlabel="UMAP 1",
            ylabel="UMAP 2",
            out=out,
            point_size=2.5,
        )
    raise FileNotFoundError("no pseudotime.tsv with x/y")


@register_view(VIEWS, "pseudotime_scatter")
def view_pseudotime_scatter(ctx: FigureContext) -> dict:
    runs = _iter_runs(ctx)
    out_runs = []
    for run in runs:
        loaded = _load_pseudotime(ctx, run)
        if loaded is None:
            continue
        pts, xs, ys, branches = loaded
        n = len(pts)
        if n == 0:
            continue
        if n > _SUBSAMPLE_CAP:
            idx = ctx.rng.choice(n, size=_SUBSAMPLE_CAP, replace=False)
            idx.sort()
            pts = pts[idx]
            xs = xs[idx]
            ys = ys[idx]
            branches = branches[idx]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "n_points": int(len(pts)),
                "n_total": int(n),
                "x": xs.tolist(),
                "y": ys.tolist(),
                "pseudotime": pts.tolist(),
                "branch": [str(b) for b in branches.tolist()],
            }
        )
    if not out_runs:
        raise FileNotFoundError("no pseudotime data")
    return {"runs": out_runs}
