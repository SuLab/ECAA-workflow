"""Normalization stage figures — applies to bulk RNA-seq (DESeq2/vst),
scRNA-seq (SCTransform / Pearson residuals / log-normalize), proteomics
(VSN / median shift). Reads a manifest describing per-sample or
per-compartment normalization outputs with mean/variance pairs and
HVG counts.

Expected inputs (any subset):
- manifest.json with `runs: [{id, mean_variance_path, n_hvg}, ...]`
- <run>/mean_variance.tsv[.gz] with columns {feature, mean, variance}
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    bar,
    register_figure,
    register_view,
    savefig,
    scatter,
    stage_registry,
    stage_view_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("normalization")
VIEWS = stage_view_registry("normalization")

_SUBSAMPLE_CAP = 30_000


def _iter_runs(ctx: FigureContext) -> List[dict]:
    runs = ctx.manifest.get("runs") or ctx.manifest.get("compartments")
    if isinstance(runs, list) and runs:
        return runs
    return [{"id": "run"}]


def _load_mean_variance(
    ctx: FigureContext, run: dict
) -> Optional[Tuple[np.ndarray, np.ndarray]]:
    run_id = run.get("id", "run")
    for name in ("mean_variance.tsv.gz", "mean_variance.tsv"):
        for p in (ctx.outputs_dir / run_id / name, ctx.outputs_dir / name):
            if not p.exists():
                continue
            opener = gzip.open if str(p).endswith(".gz") else open
            try:
                means: List[float] = []
                vars_: List[float] = []
                with opener(p, "rt") as f:
                    header = f.readline().rstrip("\n").split("\t")
                    try:
                        i_m = header.index("mean")
                        i_v = header.index("variance")
                    except ValueError:
                        continue
                    for line in f:
                        parts = line.rstrip("\n").split("\t")
                        if len(parts) <= max(i_m, i_v):
                            continue
                        try:
                            means.append(float(parts[i_m]))
                            vars_.append(float(parts[i_v]))
                        except ValueError:
                            continue
                if means:
                    return np.asarray(means), np.asarray(vars_)
            except OSError:
                continue
    return None


@register_figure(FIGURES, "mean_variance")
def mean_variance(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = _iter_runs(ctx)
    for run in runs:
        mv = _load_mean_variance(ctx, run)
        if mv is None:
            continue
        means, vars_ = mv
        # log-log scatter of mean vs variance — canonical diagnostic
        return scatter(
            x=np.log1p(means),
            y=np.log1p(vars_),
            title=f"Mean-variance — {run.get('id','run')}",
            xlabel="log1p(mean)",
            ylabel="log1p(variance)",
            out=out,
            point_size=2.0,
        )
    raise FileNotFoundError("no mean_variance.tsv")


@register_figure(FIGURES, "hvg_count_bar")
def hvg_count_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    runs = _iter_runs(ctx)
    names: List[str] = []
    values: List[float] = []
    for run in runs:
        n_hvg = run.get("n_hvg") or run.get("n_highly_variable_features")
        if isinstance(n_hvg, (int, float)):
            names.append(str(run.get("id", "run")))
            values.append(float(n_hvg))
    if not names:
        raise FileNotFoundError("manifest.runs[].n_hvg required")
    return bar(
        names=names,
        values=values,
        title="Highly-variable features per run",
        ylabel="n HVG",
        xlabel="run",
        out=out,
    )


@register_view(VIEWS, "mean_variance")
def view_mean_variance(ctx: FigureContext) -> dict:
    runs = _iter_runs(ctx)
    out_runs = []
    for run in runs:
        mv = _load_mean_variance(ctx, run)
        if mv is None:
            continue
        means, vars_ = mv
        n = len(means)
        if n == 0:
            continue
        if n > _SUBSAMPLE_CAP:
            idx = ctx.rng.choice(n, size=_SUBSAMPLE_CAP, replace=False)
            idx.sort()
            means = means[idx]
            vars_ = vars_[idx]
        out_runs.append(
            {
                "id": run.get("id", "run"),
                "n_points": int(len(means)),
                "n_total": int(n),
                "x": np.log1p(means).tolist(),
                "y": np.log1p(vars_).tolist(),
            }
        )
    if not out_runs:
        raise FileNotFoundError("no mean_variance data")
    return {"runs": out_runs, "axis_labels": {"x": "log1p(mean)", "y": "log1p(variance)"}}
