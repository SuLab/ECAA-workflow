"""Clustering stage figures — applies to scRNA-seq, spatial
transcriptomics, metagenomics binning, any modality that groups samples
or cells into discrete labels.

Expected inputs (any subset):
- manifest.json with `compartments` / `runs` listing per-run cluster
  assignments (cluster_label_path, n_clusters, embedding_path)
- <run>/cluster_labels.tsv[.gz] with columns {cell, cluster[, x, y]}
- <run>/cluster_sizes.json as a flat {cluster: count} map
"""

from __future__ import annotations

import gzip
import json
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    bar,
    categorical_palette,
    iter_runs,
    register_figure,
    register_view,
    savefig,
    stage_registry,
    stage_view_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("clustering")
VIEWS = stage_view_registry("clustering")

_SUBSAMPLE_CAP = 50_000


def _load_cluster_labels(
    ctx: FigureContext, run: dict
) -> Optional[Tuple[np.ndarray, np.ndarray, np.ndarray]]:
    """Returns (cluster_ids, x, y) arrays, or None when inputs absent.
    Falls back to `cluster_labels.tsv` without x/y columns by returning
    zero-filled embedding arrays so the cluster-size view still works.
    """
    run_id = run.get("id", "run")
    for p in (
        ctx.outputs_dir / run_id / "cluster_labels.tsv.gz",
        ctx.outputs_dir / run_id / "cluster_labels.tsv",
        ctx.outputs_dir / "cluster_labels.tsv.gz",
        ctx.outputs_dir / "cluster_labels.tsv",
    ):
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                try:
                    i_c = header.index("cluster")
                except ValueError:
                    continue
                i_x = header.index("x") if "x" in header else None
                i_y = header.index("y") if "y" in header else None
                clusters: List[str] = []
                xs: List[float] = []
                ys: List[float] = []
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= i_c:
                        continue
                    clusters.append(parts[i_c])
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
            if not clusters:
                return None
            return (
                np.asarray(clusters, dtype=object),
                np.asarray(xs, dtype=float),
                np.asarray(ys, dtype=float),
            )
        except OSError:
            continue
    return None


def _load_cluster_sizes(ctx: FigureContext, run: dict) -> Optional[Dict[str, int]]:
    """Reads a flat {cluster_id: count} map from cluster_sizes.json, or
    derives it from the manifest when `clusters: [{id, n_cells}, ...]`
    is present.
    """
    run_id = run.get("id", "run")
    # Manifest first — cheapest
    clusters = run.get("clusters")
    if isinstance(clusters, list):
        return {
            str(c.get("id")): int(c.get("n_cells") or c.get("n_members") or 0)
            for c in clusters
            if isinstance(c, dict)
        }
    for p in (
        ctx.outputs_dir / run_id / "cluster_sizes.json",
        ctx.outputs_dir / "cluster_sizes.json",
    ):
        if p.exists():
            try:
                data = json.loads(p.read_text())
                if isinstance(data, dict):
                    return {str(k): int(v) for k, v in data.items()}
            except (OSError, json.JSONDecodeError):
                continue
    return None


@register_figure(FIGURES, "umap_clusters")
def umap_clusters(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = iter_runs(ctx)
    chosen = None
    for run in runs:
        loaded = _load_cluster_labels(ctx, run)
        if loaded is None:
            continue
        clusters, xs, ys = loaded
        if len(clusters) > 0 and np.any(xs != 0.0) and np.any(ys != 0.0):
            chosen = (run.get("id", "run"), clusters, xs, ys)
            break
    if chosen is None:
        raise FileNotFoundError("no cluster_labels.tsv with x/y columns")
    run_id, clusters, xs, ys = chosen
    unique = sorted(set(clusters.tolist()), key=str)
    palette = categorical_palette(len(unique), name=f"clustering.{run_id}")
    rasterize = len(xs) > _SUBSAMPLE_CAP
    fig, ax = plt.subplots(figsize=(7.5, 6.0))
    for i, lab in enumerate(unique):
        mask = clusters == lab
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
    ax.set_title(f"Clusters — {run_id} (n = {len(xs):,} cells)")
    if len(unique) <= 20:
        ax.legend(loc="best", markerscale=3)
    return savefig(fig, out)


@register_figure(FIGURES, "cluster_size_bar")
def cluster_size_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = iter_runs(ctx)
    for run in runs:
        sizes = _load_cluster_sizes(ctx, run)
        if sizes:
            names = sorted(sizes.keys(), key=str)
            values = [float(sizes[n]) for n in names]
            return bar(
                names=names,
                values=values,
                title=f"Cluster sizes — {run.get('id','run')}",
                ylabel="n members",
                xlabel="cluster",
                out=out,
            )
    raise FileNotFoundError("no cluster_sizes.json or cluster_labels for any run")


@register_view(VIEWS, "umap_by_cluster")
def view_umap_by_cluster(ctx: FigureContext) -> dict:
    runs = iter_runs(ctx)
    out_runs = []
    for run in runs:
        loaded = _load_cluster_labels(ctx, run)
        if loaded is None:
            continue
        clusters, xs, ys = loaded
        n = len(clusters)
        if n == 0:
            continue
        if n > _SUBSAMPLE_CAP:
            idx = ctx.rng.choice(n, size=_SUBSAMPLE_CAP, replace=False)
            idx.sort()
            clusters = clusters[idx]
            xs = xs[idx]
            ys = ys[idx]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "n_points": int(len(clusters)),
                "n_total": int(n),
                "x": xs.tolist(),
                "y": ys.tolist(),
                "cluster": [str(c) for c in clusters.tolist()],
            }
        )
    if not out_runs:
        raise FileNotFoundError("no cluster labels readable")
    return {"runs": out_runs}


@register_view(VIEWS, "cluster_sizes")
def view_cluster_sizes(ctx: FigureContext) -> dict:
    runs = iter_runs(ctx)
    out_runs = []
    for run in runs:
        sizes = _load_cluster_sizes(ctx, run)
        if not sizes:
            continue
        names = sorted(sizes.keys(), key=str)
        values = [int(sizes[n]) for n in names]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "cluster": names,
                "n_members": values,
            }
        )
    if not out_runs:
        raise FileNotFoundError("no cluster_sizes for any run")
    return {"runs": out_runs}
