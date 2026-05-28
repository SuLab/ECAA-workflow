"""Dimensionality-reduction stage figures — applies to scRNA and bulk
RNA-seq pipelines that produce PCA + optional UMAP embeddings.

Expected inputs (any subset):
- manifest.json with a `compartments` or `runs` list, each entry having
  `n_components`, `variance_explained: [floats]`, `embedding_path`
- <compartment>/variance_ratio.json — bare list of floats
- <compartment>/embedding.tsv[.gz] — two-column x, y (first header row)

Generic across modalities: bulk RNA-seq stores a single run; scRNA stores
one per compartment. The functions iterate over whatever the manifest
advertises, falling back to a flat layout.
"""

from __future__ import annotations

import gzip
import json
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    register_figure,
    register_view,
    savefig,
    scatter,
    stage_registry,
    stage_view_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("dimensionality_reduction")
VIEWS = stage_view_registry("dimensionality_reduction")


def _iter_runs(ctx: FigureContext) -> List[dict]:
    runs = ctx.manifest.get("compartments") or ctx.manifest.get("runs")
    if isinstance(runs, list) and runs:
        return runs
    # Flat-layout fallback: assume `outputs_dir` itself is one run
    return [{"id": "run", "variance_explained": None, "embedding_path": None}]


def _load_variance_explained(ctx: FigureContext, run: dict) -> Optional[np.ndarray]:
    v = run.get("variance_explained")
    if isinstance(v, list) and v:
        return np.asarray(v, dtype=float)
    run_id = run.get("id", "run")
    for candidate in (
        ctx.outputs_dir / run_id / "variance_ratio.json",
        ctx.outputs_dir / "variance_ratio.json",
    ):
        if candidate.exists():
            try:
                loaded = json.loads(candidate.read_text())
                if isinstance(loaded, list):
                    return np.asarray(loaded, dtype=float)
            except (OSError, json.JSONDecodeError):
                continue
    return None


def _load_embedding(ctx: FigureContext, run: dict) -> Optional[Tuple[np.ndarray, np.ndarray]]:
    path = run.get("embedding_path")
    candidates: List[Path] = []
    if path:
        candidates.append(Path(path))
        candidates.append(ctx.outputs_dir / path)
    run_id = run.get("id", "run")
    candidates.extend(
        [
            ctx.outputs_dir / run_id / "embedding.tsv.gz",
            ctx.outputs_dir / run_id / "embedding.tsv",
            ctx.outputs_dir / "embedding.tsv.gz",
            ctx.outputs_dir / "embedding.tsv",
        ]
    )
    for p in candidates:
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            xs: List[float] = []
            ys: List[float] = []
            with opener(p, "rt") as f:
                f.readline()  # header
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) < 2:
                        continue
                    try:
                        xs.append(float(parts[0]))
                        ys.append(float(parts[1]))
                    except ValueError:
                        continue
            if xs:
                return np.asarray(xs), np.asarray(ys)
        except OSError:
            continue
    return None


@register_figure(FIGURES, "variance_explained_elbow")
def variance_explained_elbow(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Elbow plot — cumulative variance explained per PC. Aggregates
    across runs when multiple are present; renders a single line per
    run on a shared axis.
    """
    runs = _iter_runs(ctx)
    fig, ax = plt.subplots(figsize=(7.5, 5.5))
    plotted_any = False
    for run in runs:
        ve = _load_variance_explained(ctx, run)
        if ve is None or ve.size == 0:
            continue
        # Clip to ratios in [0, 1] defensively.
        ve = np.clip(np.asarray(ve, dtype=float), 0.0, 1.0)
        cum = np.cumsum(ve)
        ax.plot(range(1, len(cum) + 1), cum, marker="o", markersize=3, label=str(run.get("id", "run")))
        plotted_any = True
    if not plotted_any:
        raise FileNotFoundError("no variance_explained data in any run")
    ax.set_xlabel("principal component")
    ax.set_ylabel("cumulative variance explained")
    ax.set_title("PCA: cumulative variance explained")
    ax.set_ylim(0.0, 1.05)
    ax.axhline(0.9, color="gray", linestyle=":", linewidth=0.7)
    if len(runs) > 1:
        ax.legend(fontsize=8, loc="lower right")
    fig.tight_layout()
    return savefig(fig, out)


_SUBSAMPLE_CAP = 50_000


@register_view(VIEWS, "embedding_scatter")
def view_embedding_scatter(ctx: FigureContext) -> dict:
    """JSON payload for the interactive UMAP/PCA scatter. Subsamples
    large runs to _SUBSAMPLE_CAP points using the context's seeded RNG
    so repeated loads of the same session return identical point sets.
    """
    runs = _iter_runs(ctx)
    out_runs = []
    for run in runs:
        emb = _load_embedding(ctx, run)
        if emb is None:
            continue
        x, y = emb
        n = len(x)
        if n == 0:
            continue
        if n > _SUBSAMPLE_CAP:
            idx = ctx.rng.choice(n, size=_SUBSAMPLE_CAP, replace=False)
            idx.sort()
            x = x[idx]
            y = y[idx]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "n_points": int(len(x)),
                "n_total": int(n),
                "x": x.tolist(),
                "y": y.tolist(),
            }
        )
    if not out_runs:
        raise FileNotFoundError("no readable embeddings for any run")
    return {
        "runs": out_runs,
        "axis_labels": {"x": "component 1", "y": "component 2"},
    }


@register_view(VIEWS, "variance_explained")
def view_variance_explained(ctx: FigureContext) -> dict:
    runs = _iter_runs(ctx)
    out_runs = []
    for run in runs:
        ve = _load_variance_explained(ctx, run)
        if ve is None or ve.size == 0:
            continue
        cum = np.cumsum(np.clip(ve.astype(float), 0.0, 1.0))
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "per_component": ve.astype(float).tolist(),
                "cumulative": cum.tolist(),
            }
        )
    if not out_runs:
        raise FileNotFoundError("no variance_explained in any run")
    return {"runs": out_runs}


@register_figure(FIGURES, "embedding_scatter")
def embedding_scatter(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Scatter of the first embedding we find. When multiple compartments
    exist, render the one with the most cells (falls back to the first).
    """
    runs = _iter_runs(ctx)
    # Pick the run most likely to have data
    sorted_runs = sorted(
        runs,
        key=lambda r: -int(r.get("n_cells") or r.get("n_samples") or 0),
    )
    for run in sorted_runs:
        emb = _load_embedding(ctx, run)
        if emb is None:
            continue
        x, y = emb
        run_id = run.get("id", "run")
        return scatter(
            x=x,
            y=y,
            title=f"Embedding: {run_id}",
            xlabel="component 1",
            ylabel="component 2",
            out=out,
            point_size=2.5 if len(x) > 5000 else 6.0,
        )
    raise FileNotFoundError("no readable embedding artifact in any run")
