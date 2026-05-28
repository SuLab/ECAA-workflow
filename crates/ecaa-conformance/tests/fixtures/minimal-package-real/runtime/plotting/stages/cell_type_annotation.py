"""Cell-type annotation stage — scRNA-seq + spatial. Reads a manifest
listing per-compartment celltype assignments + top markers per celltype.

Expected inputs:
- manifest.json with `runs: [{id, celltypes: [{id, n_cells, markers: [...]}]}]`
- <run>/celltype_labels.tsv[.gz] with columns {cell, celltype[, x, y]}
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    bar,
    categorical_palette,
    register_figure,
    register_view,
    savefig,
    stage_registry,
    stage_view_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("cell_type_annotation")
VIEWS = stage_view_registry("cell_type_annotation")

_SUBSAMPLE_CAP = 50_000


def _iter_runs(ctx: FigureContext) -> List[dict]:
    runs = ctx.manifest.get("runs") or ctx.manifest.get("compartments")
    if isinstance(runs, list) and runs:
        return runs
    return [{"id": "run"}]


def _load_celltype_labels(
    ctx: FigureContext, run: dict
) -> Optional[Tuple[np.ndarray, np.ndarray, np.ndarray]]:
    run_id = run.get("id", "run")
    for p in (
        ctx.outputs_dir / run_id / "celltype_labels.tsv.gz",
        ctx.outputs_dir / run_id / "celltype_labels.tsv",
        ctx.outputs_dir / "celltype_labels.tsv.gz",
        ctx.outputs_dir / "celltype_labels.tsv",
    ):
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                try:
                    i_c = header.index("celltype")
                except ValueError:
                    continue
                i_x = header.index("x") if "x" in header else None
                i_y = header.index("y") if "y" in header else None
                cts: List[str] = []
                xs: List[float] = []
                ys: List[float] = []
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= i_c:
                        continue
                    cts.append(parts[i_c])
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
            if not cts:
                return None
            return (
                np.asarray(cts, dtype=object),
                np.asarray(xs, dtype=float),
                np.asarray(ys, dtype=float),
            )
        except OSError:
            continue
    return None


def _celltype_sizes_from_manifest(run: dict) -> Optional[Dict[str, int]]:
    cts = run.get("celltypes")
    if not isinstance(cts, list):
        return None
    out: Dict[str, int] = {}
    for ct in cts:
        if not isinstance(ct, dict):
            continue
        name = str(ct.get("id") or ct.get("name") or "")
        n = ct.get("n_cells") or ct.get("n_members")
        if name and isinstance(n, (int, float)):
            out[name] = int(n)
    return out if out else None


@register_figure(FIGURES, "umap_by_celltype")
def umap_by_celltype(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = _iter_runs(ctx)
    chosen = None
    for run in runs:
        loaded = _load_celltype_labels(ctx, run)
        if loaded is None:
            continue
        cts, xs, ys = loaded
        if len(cts) > 0 and (np.any(xs != 0.0) or np.any(ys != 0.0)):
            chosen = (run.get("id", "run"), cts, xs, ys)
            break
    if chosen is None:
        raise FileNotFoundError("no celltype_labels.tsv with x/y")
    run_id, cts, xs, ys = chosen
    unique = sorted(set(cts.tolist()), key=str)
    palette = categorical_palette(len(unique), name=f"cell_type_annotation.{run_id}")
    rasterize = len(xs) > _SUBSAMPLE_CAP
    fig, ax = plt.subplots(figsize=(7.5, 6.0))
    for i, lab in enumerate(unique):
        mask = cts == lab
        ax.scatter(
            xs[mask],
            ys[mask],
            s=2,
            alpha=0.6,
            color=palette[i],
            label=str(lab),
            linewidths=0,
            rasterized=rasterize,
        )
    ax.set_xlabel("UMAP 1")
    ax.set_ylabel("UMAP 2")
    ax.set_aspect("equal", adjustable="datalim")
    ax.set_title(f"Cell types — {run_id} (n = {len(xs):,} cells)")
    if len(unique) <= 20:
        ax.legend(loc="best", markerscale=3)
    return savefig(fig, out)


@register_figure(FIGURES, "celltype_size_bar")
def celltype_size_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = _iter_runs(ctx)
    for run in runs:
        sizes = _celltype_sizes_from_manifest(run)
        if sizes:
            names = sorted(sizes.keys())
            values = [float(sizes[n]) for n in names]
            return bar(
                names=names,
                values=values,
                title=f"Celltype sizes — {run.get('id','run')}",
                ylabel="n cells",
                xlabel="celltype",
                out=out,
            )
    raise FileNotFoundError("no celltype sizes in any run's manifest")


@register_view(VIEWS, "umap_by_celltype")
def view_umap_by_celltype(ctx: FigureContext) -> dict:
    runs = _iter_runs(ctx)
    out_runs = []
    for run in runs:
        loaded = _load_celltype_labels(ctx, run)
        if loaded is None:
            continue
        cts, xs, ys = loaded
        n = len(cts)
        if n == 0:
            continue
        if n > _SUBSAMPLE_CAP:
            idx = ctx.rng.choice(n, size=_SUBSAMPLE_CAP, replace=False)
            idx.sort()
            cts = cts[idx]
            xs = xs[idx]
            ys = ys[idx]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "n_points": int(len(cts)),
                "n_total": int(n),
                "x": xs.tolist(),
                "y": ys.tolist(),
                "cluster": [str(c) for c in cts.tolist()],  # reuse UI scatter key
            }
        )
    if not out_runs:
        raise FileNotFoundError("no celltype labels readable")
    return {"runs": out_runs}
