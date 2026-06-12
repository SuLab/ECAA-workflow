"""DIABLO renderer — components scatter, top-loading heatmap, modality
concordance heatmap, and shared factor-variance summary."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    categorical_palette,
    heatmap,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)
from . import multi_omics_integration as _multi_omics

FIGURES = stage_registry("diablo")


@register_figure(FIGURES, "diablo_components_scatter")
def diablo_components_scatter(ctx, out):
    p = resolve_artifact_path(ctx, "components_path", "components.tsv")
    cols = load_tsv_columns(p) or {}
    c1 = np.array([float(x) for x in cols.get("comp1", [])])
    c2 = np.array([float(x) for x in cols.get("comp2", [])])
    groups = cols.get("group", [])
    if c1.size == 0:
        raise ValueError("no components")
    unique = sorted(set(groups))
    palette = categorical_palette(len(unique), name="diablo.groups")
    fig, ax = plt.subplots(figsize=(7.0, 6.0))
    for i, g in enumerate(unique):
        mask = np.array([gg == g for gg in groups])
        ax.scatter(c1[mask], c2[mask], color=palette[i], label=g, s=60, edgecolors="white")
    ax.set_xlabel("Component 1")
    ax.set_ylabel("Component 2")
    ax.set_title("DIABLO components")
    ax.legend()
    return savefig(fig, out)


@register_figure(FIGURES, "diablo_loadings_heatmap")
def diablo_loadings_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "loadings_path", "loadings.tsv")
    cols = load_tsv_columns(p) or {}
    feats = cols.get("feature", [])
    mods = cols.get("modality", [])
    load = np.array([float(x) for x in cols.get("loading_comp1", [])])
    if not feats:
        raise ValueError("no loadings")
    order = np.argsort(-np.abs(load))[:min(20, load.size)]
    labels = [f"{feats[i]} ({mods[i]})" for i in order]
    matrix = load[order].reshape(-1, 1)
    return heatmap(matrix=matrix, row_labels=labels, col_labels=["loading"],
                   title="Top DIABLO loadings (comp 1)",
                   out=out, figsize=(5.0, max(4.0, 0.3 * len(order))))


@register_figure(FIGURES, "modality_concordance_heatmap")
def modality_concordance_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "concordance_path", "concordance.tsv")
    cols = load_tsv_columns(p) or {}
    a = cols.get("modality_a", [])
    b = cols.get("modality_b", [])
    corr = [float(x) for x in cols.get("correlation", [])]
    if not a:
        raise ValueError("no concordance")
    order = sorted(set(a) | set(b))
    m = np.zeros((len(order), len(order)))
    for ai, bi, c in zip(a, b, corr):
        m[order.index(ai), order.index(bi)] = c
    return heatmap(matrix=m, row_labels=order, col_labels=order,
                   title="Modality concordance", out=out, figsize=(5.5, 5.0))


@register_figure(FIGURES, "factor_variance_bar")
def factor_variance_bar(ctx, out):
    return _multi_omics.factor_variance_bar(ctx, out)
