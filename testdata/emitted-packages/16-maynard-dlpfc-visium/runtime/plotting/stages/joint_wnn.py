"""Joint WNN renderer — UMAP coloured by cluster + modality-weight stacked
bar per cluster."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    categorical_palette,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("joint_wnn")


@register_figure(FIGURES, "wnn_umap")
def wnn_umap(ctx, out):
    p = resolve_artifact_path(ctx, "embedding_path", "wnn_embedding.tsv")
    cols = load_tsv_columns(p) or {}
    x = np.array([float(v) for v in cols.get("x", [])])
    y = np.array([float(v) for v in cols.get("y", [])])
    clusters = cols.get("cluster", [])
    if x.size == 0:
        raise ValueError("no embedding")
    unique = sorted(set(clusters))
    palette = categorical_palette(len(unique), name="wnn.clusters")
    fig, ax = plt.subplots(figsize=(7.0, 6.0))
    for i, lab in enumerate(unique):
        mask = np.array([c == lab for c in clusters])
        ax.scatter(x[mask], y[mask], s=12, color=palette[i], label=lab, alpha=0.8)
    ax.set_xlabel("WNN UMAP 1")
    ax.set_ylabel("WNN UMAP 2")
    ax.set_title("Joint WNN UMAP")
    ax.legend(loc="best", markerscale=2)
    return savefig(fig, out)


@register_figure(FIGURES, "cluster_composition_bar")
def cluster_composition_bar(ctx, out):
    p = resolve_artifact_path(ctx, "embedding_path", "wnn_embedding.tsv")
    cols = load_tsv_columns(p) or {}
    clusters = cols.get("cluster", [])
    rna_w = [float(v) for v in cols.get("rna_weight", [])]
    atac_w = [float(v) for v in cols.get("atac_weight", [])]
    if not clusters:
        raise ValueError("no clusters")
    unique = sorted(set(clusters))
    rna_means = []
    atac_means = []
    for u in unique:
        idx = [i for i, c in enumerate(clusters) if c == u]
        rna_means.append(float(np.mean([rna_w[i] for i in idx])))
        atac_means.append(float(np.mean([atac_w[i] for i in idx])))
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    xpos = np.arange(len(unique))
    ax.bar(xpos, rna_means, label="RNA weight", color="#0072B2")
    ax.bar(xpos, atac_means, bottom=rna_means, label="ATAC weight", color="#D55E00")
    ax.set_xticks(xpos)
    ax.set_xticklabels(unique)
    ax.set_xlabel("cluster")
    ax.set_ylabel("mean modality weight")
    ax.set_title("Per-cluster modality weight composition")
    ax.legend()
    return savefig(fig, out)
