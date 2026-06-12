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

import matplotlib.pyplot as plt
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


# ── PCA on the normalized expression matrix ──────────────────────────
#
# Standard sample-level QC plot for bulk RNA-seq: project samples into
# PC1/PC2 space on the VST-transformed counts and color by condition.
# Distinct treatment vs control clusters indicate the biological signal
# survives library-size correction; overlapping clusters flag batch
# effects or insufficient replicates.

_VST_FILENAMES = (
    "vst_matrix.tsv",
    "vst_matrix.tsv.gz",
    "normalized_counts.tsv",
    "normalized_counts.tsv.gz",
    "logcpm_matrix.tsv",
    "logcpm_matrix.tsv.gz",
)


def _load_normalized_matrix(
    ctx: FigureContext,
) -> Optional[Tuple[np.ndarray, List[str], List[str]]]:
    """Locate and parse the normalized expression matrix.

    Returns ``(matrix_genes_by_samples, gene_ids, sample_ids)`` when a
    parseable file is found anywhere under ``outputs_dir`` (top-level or
    one level under a per-run subdir). Returns None when nothing matches.
    """
    candidates: List[Path] = []
    for name in _VST_FILENAMES:
        candidates.append(ctx.outputs_dir / name)
    # one-level-deep search for per-run normalized matrices
    if ctx.outputs_dir.exists():
        for sub in ctx.outputs_dir.iterdir():
            if sub.is_dir():
                for name in _VST_FILENAMES:
                    candidates.append(sub / name)
    for p in candidates:
        if not p.exists():
            continue
        opener = gzip.open if str(p).endswith(".gz") else open
        try:
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                # Expect: first column is feature/gene id, remaining are samples
                if len(header) < 3:
                    continue
                sample_ids = header[1:]
                gene_ids: List[str] = []
                rows: List[List[float]] = []
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) != len(header):
                        continue
                    try:
                        vals = [float(x) for x in parts[1:]]
                    except ValueError:
                        continue
                    gene_ids.append(parts[0])
                    rows.append(vals)
            if not rows:
                continue
            return np.asarray(rows, dtype=float), gene_ids, sample_ids
        except OSError:
            continue
    return None


def _load_sample_labels(ctx: FigureContext, sample_ids: List[str]) -> List[str]:
    """Best-effort: read a samples table (samples.tsv / sample_metadata.tsv)
    from the data_acquisition outputs and look up `condition` per sample.
    Falls back to the sample id string when no metadata is available.
    """
    # Walk upward from outputs_dir to find <pkg>/runtime/outputs/data_acquisition/
    sample_to_label: dict = {}
    pkg_runtime = None
    for parent in [ctx.outputs_dir] + list(ctx.outputs_dir.parents):
        if parent.name == "outputs" and (parent / "data_acquisition").exists():
            pkg_runtime = parent
            break
    if pkg_runtime is not None:
        for rel in (
            "data_acquisition/data/*/samples.tsv",
            "data_acquisition/data/*/sample_metadata.tsv",
            "data_acquisition/samples.tsv",
        ):
            for candidate in pkg_runtime.glob(rel):
                try:
                    with open(candidate, "rt") as f:
                        header = f.readline().rstrip("\n").split("\t")
                        try:
                            i_sample = header.index("sample")
                        except ValueError:
                            i_sample = 0
                        label_col = None
                        for col_name in ("condition", "group", "treatment", "label"):
                            if col_name in header:
                                label_col = header.index(col_name)
                                break
                        if label_col is None:
                            continue
                        for line in f:
                            parts = line.rstrip("\n").split("\t")
                            if len(parts) > max(i_sample, label_col):
                                sample_to_label[parts[i_sample]] = parts[label_col]
                except OSError:
                    continue
                if sample_to_label:
                    break
            if sample_to_label:
                break
    return [sample_to_label.get(s, s) for s in sample_ids]


@register_figure(FIGURES, "sample_pca")
def sample_pca(ctx: FigureContext, out: Path) -> Optional[Path]:
    """PC1 vs PC2 scatter of samples on the normalized expression matrix.

    Reads a VST / log-CPM / normalized-counts matrix from outputs_dir
    (top-level or per-run subdir), centers + scales it, computes PCA via
    numpy SVD, and renders a scatter with sample labels above each point.
    Color encodes the `condition` factor when a sample metadata table is
    discoverable in the data_acquisition outputs; falls back to a single
    palette color otherwise.

    Variance-explained values are shown in the axis labels.
    """
    matrix_result = _load_normalized_matrix(ctx)
    if matrix_result is None:
        raise FileNotFoundError(
            "no normalized expression matrix (vst_matrix.tsv / normalized_counts.tsv) found"
        )
    matrix, _gene_ids, sample_ids = matrix_result
    if len(sample_ids) < 2:
        raise FileNotFoundError(
            f"sample_pca requires >=2 samples, got {len(sample_ids)}"
        )
    # samples × genes
    X = matrix.T.astype(float)
    # Center per-gene (column-wise mean removal)
    X_centered = X - X.mean(axis=0, keepdims=True)
    # Compute PCA via SVD. economy SVD: U is (n_samples, k), S is (k,)
    try:
        _u, s, vt = np.linalg.svd(X_centered, full_matrices=False)
    except np.linalg.LinAlgError as e:
        raise RuntimeError(f"SVD failed on normalized matrix: {e}") from e
    if s.shape[0] < 2:
        raise FileNotFoundError(
            f"sample_pca requires rank >= 2, got rank {s.shape[0]}"
        )
    # PC scores: U * S = X_centered @ V (project samples onto PCs)
    pc = X_centered @ vt.T
    pc1 = pc[:, 0]
    pc2 = pc[:, 1]
    var_explained = (s**2) / max(float((s**2).sum()), 1e-12)
    pct1 = 100.0 * float(var_explained[0])
    pct2 = 100.0 * float(var_explained[1])
    # Color by condition when discoverable.
    labels = _load_sample_labels(ctx, sample_ids)
    unique_labels = []
    for lab in labels:
        if lab not in unique_labels:
            unique_labels.append(lab)
    palette = ["#0072B2", "#D55E00", "#009E73", "#CC79A7", "#F0E442", "#56B4E9"]
    label_to_color = {
        lab: palette[i % len(palette)] for i, lab in enumerate(unique_labels)
    }
    colors = [label_to_color[lab] for lab in labels]
    fig, ax = plt.subplots(figsize=(6.0, 5.0))
    ax.scatter(pc1, pc2, c=colors, s=60, edgecolor="black", linewidth=0.5)
    for x, y, lab in zip(pc1, pc2, sample_ids):
        ax.annotate(
            lab,
            (x, y),
            xytext=(4, 4),
            textcoords="offset points",
            fontsize=8,
        )
    ax.set_xlabel(f"PC1 ({pct1:.1f}% var)")
    ax.set_ylabel(f"PC2 ({pct2:.1f}% var)")
    ax.set_title("Sample PCA")
    ax.axhline(0, color="#cccccc", linewidth=0.5, zorder=0)
    ax.axvline(0, color="#cccccc", linewidth=0.5, zorder=0)
    # Legend (only when >1 distinct condition labels exist).
    if len(unique_labels) > 1:
        from matplotlib.lines import Line2D

        handles = [
            Line2D(
                [0],
                [0],
                marker="o",
                color="w",
                markerfacecolor=label_to_color[lab],
                markeredgecolor="black",
                markersize=8,
                label=lab,
            )
            for lab in unique_labels
        ]
        ax.legend(handles=handles, loc="best", fontsize=8, frameon=False)
    return savefig(fig, out)


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
