"""SNF (similarity-network fusion) renderer — fused-similarity heatmap,
cluster-size bar, and shared multi-omics summaries."""

from __future__ import annotations
import numpy as np

from ..core import (
    bar,
    heatmap,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    stage_registry,
)
from . import multi_omics_integration as _multi_omics

FIGURES = stage_registry("snf")


def _load_matrix(path):
    # Accepts None (when resolve_artifact_path doesn't find a candidate)
    # so the caller can short-circuit on labels is None.
    if path is None or not path.exists():
        return None, None
    with open(path) as f:
        header = f.readline().rstrip("\n").split("\t")[1:]
        rows = []
        labels = []
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) == len(header) + 1:
                labels.append(parts[0])
                rows.append([float(v) for v in parts[1:]])
    return labels, np.array(rows)


@register_figure(FIGURES, "fused_similarity_heatmap")
def fused_similarity_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "fused_matrix_path", "fused_matrix.tsv")
    labels, matrix = _load_matrix(p)
    if labels is None:
        raise FileNotFoundError("fused_matrix.tsv")
    return heatmap(matrix=matrix, row_labels=labels, col_labels=labels,
                   title="Fused similarity matrix", out=out,
                   figsize=(6.0, 5.5))


@register_figure(FIGURES, "snf_cluster_size_bar")
def snf_cluster_size_bar(ctx, out):
    p = resolve_artifact_path(ctx, "clusters_path", "clusters.tsv")
    cols = load_tsv_columns(p) or {}
    names = cols.get("cluster", [])
    values = [float(x) for x in cols.get("n_samples", [])]
    if not names:
        raise ValueError("no clusters")
    return bar(names=names, values=values, title="SNF cluster sizes",
               xlabel="cluster", ylabel="n samples", out=out)


@register_figure(FIGURES, "modality_concordance_heatmap")
def modality_concordance_heatmap(ctx, out):
    return _multi_omics.modality_concordance_heatmap(ctx, out)


@register_figure(FIGURES, "factor_variance_bar")
def factor_variance_bar(ctx, out):
    return _multi_omics.factor_variance_bar(ctx, out)
