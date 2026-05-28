"""Batch-correction stage figures. Applies to any modality that runs an
integration step with a pre/post "batch mixing" metric — scRNA-seq is
the obvious case, but bulk-RNA-seq ComBat + proteomics pycombat + spatial
integration all fit the same pattern.

Expected inputs (any subset):
- manifest.json with `compartments: [{id, mixing_pre, mixing_post, ...}]`
  (or `runs: [...]` with the same keys)
- <compartment>/integration_stats.json with {mixing_pre, mixing_post}
- <compartment>/integrated_embeddings.tsv[.gz] (optional)
"""

from __future__ import annotations

import gzip
import json
from pathlib import Path
from typing import List, Optional

import numpy as np

from ..core import (
    FigureContext,
    categorical_palette,
    iter_runs,
    register_figure,
    savefig,
    stage_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("batch_correction")


def _load_stats(ctx: FigureContext, run: dict) -> Optional[dict]:
    if "mixing_pre" in run and "mixing_post" in run:
        return {"mixing_pre": run["mixing_pre"], "mixing_post": run["mixing_post"]}
    run_id = run.get("id", "run")
    for candidate in (
        ctx.outputs_dir / run_id / "integration_stats.json",
        ctx.outputs_dir / "integration_stats.json",
    ):
        if candidate.exists():
            try:
                data = json.loads(candidate.read_text())
                if "mixing_pre" in data and "mixing_post" in data:
                    return {"mixing_pre": data["mixing_pre"], "mixing_post": data["mixing_post"]}
            except (OSError, json.JSONDecodeError):
                continue
    return None


def _load_embedding(ctx: FigureContext, run: dict) -> Optional[np.ndarray]:
    run_id = run.get("id", "run")
    for p in (
        ctx.outputs_dir / run_id / "integrated_embeddings.tsv.gz",
        ctx.outputs_dir / run_id / "integrated_embeddings.tsv",
        ctx.outputs_dir / "integrated_embeddings.tsv.gz",
        ctx.outputs_dir / "integrated_embeddings.tsv",
    ):
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            xs: List[float] = []
            ys: List[float] = []
            batches: List[str] = []
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                try:
                    i_x = header.index("umap_1") if "umap_1" in header else 0
                    i_y = header.index("umap_2") if "umap_2" in header else 1
                    i_b = header.index("batch") if "batch" in header else None
                except ValueError:
                    i_x, i_y, i_b = 0, 1, None
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= max(i_x, i_y):
                        continue
                    try:
                        xs.append(float(parts[i_x]))
                        ys.append(float(parts[i_y]))
                    except ValueError:
                        continue
                    if i_b is not None and len(parts) > i_b:
                        batches.append(parts[i_b])
                    else:
                        batches.append("")
            if not xs:
                return None
            return np.asarray(list(zip(xs, ys, batches)), dtype=object)
        except OSError:
            continue
    return None


@register_figure(FIGURES, "mixing_score_bar")
def mixing_score_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Grouped bars of pre vs post mixing score per compartment/run."""
    runs = iter_runs(ctx)
    rows: List[tuple] = []
    for run in runs:
        stats = _load_stats(ctx, run)
        if stats is None:
            continue
        try:
            pre = float(stats["mixing_pre"])
            post = float(stats["mixing_post"])
        except (TypeError, ValueError):
            continue
        rows.append((str(run.get("id", "run")), pre, post))
    if not rows:
        raise FileNotFoundError("no integration_stats.json with mixing_pre/post")
    labels = [r[0] for r in rows]
    pre_vals = [r[1] for r in rows]
    post_vals = [r[2] for r in rows]
    x = np.arange(len(labels))
    width = 0.38
    fig, ax = plt.subplots(figsize=(max(6.0, 1.5 * len(labels)), 5.0))
    ax.bar(x - width / 2, pre_vals, width, label="pre", color="lightsteelblue")
    ax.bar(x + width / 2, post_vals, width, label="post", color="steelblue")
    ax.set_xticks(x)
    ax.set_xticklabels(labels, rotation=30, ha="right")
    ax.set_ylabel("mixing score")
    ax.set_title("Batch correction: pre vs post mixing score")
    ax.legend()
    fig.tight_layout()
    return savefig(fig, out)


@register_figure(FIGURES, "umap_pre_post")
def umap_pre_post(ctx: FigureContext, out: Path) -> Optional[Path]:
    """UMAP colored by batch for each compartment. Only renders the
    post-integration embedding — pre-integration is rarely persisted
    and the diagnostic value of side-by-side declines quickly with cell
    count.
    """
    runs = iter_runs(ctx)
    chosen = None
    for run in runs:
        emb = _load_embedding(ctx, run)
        if emb is not None and len(emb) > 0:
            chosen = (run.get("id", "run"), emb)
            break
    if chosen is None:
        raise FileNotFoundError("no integrated_embeddings.tsv[.gz]")
    run_id, emb = chosen
    xs = np.asarray([row[0] for row in emb], dtype=float)
    ys = np.asarray([row[1] for row in emb], dtype=float)
    batches = np.asarray([row[2] for row in emb])
    fig, ax = plt.subplots(figsize=(7.5, 6.0))
    unique = sorted(set(str(b) for b in batches))
    palette = categorical_palette(len(unique), name=f"batch_correction.{run_id}")
    rasterize = len(xs) > 50_000
    for i, b in enumerate(unique):
        mask = batches == b
        ax.scatter(
            xs[mask],
            ys[mask],
            s=2,
            alpha=0.6,
            color=palette[i],
            label=b if b else "<unassigned>",
            linewidths=0,
            rasterized=rasterize,
        )
    ax.set_xlabel("UMAP 1")
    ax.set_ylabel("UMAP 2")
    ax.set_aspect("equal", adjustable="datalim")
    ax.set_title(f"Integrated embedding by batch — {run_id} (n = {len(xs):,} cells)")
    if len(unique) <= 12:
        ax.legend(loc="best", markerscale=3)
    return savefig(fig, out)
