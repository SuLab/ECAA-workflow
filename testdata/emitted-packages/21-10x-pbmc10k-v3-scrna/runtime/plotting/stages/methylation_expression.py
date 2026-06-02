"""Methylation × expression renderer — per-gene scatter + top-driver heatmap."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    heatmap,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("methylation_expression")


@register_figure(FIGURES, "methylation_vs_expression_scatter")
def methylation_vs_expression_scatter(ctx, out):
    p = resolve_artifact_path(ctx, "correlations_path", "correlations.tsv")
    cols = load_tsv_columns(p) or {}
    meth = np.array([float(x) for x in cols.get("methylation", [])])
    expr = np.array([float(x) for x in cols.get("expression", [])])
    corr = np.array([float(x) for x in cols.get("correlation", [])])
    if meth.size == 0:
        raise ValueError("no rows")
    fig, ax = plt.subplots(figsize=(7.0, 5.5))
    sc = ax.scatter(meth, expr, c=corr, cmap="RdBu_r", vmin=-1.0, vmax=1.0,
                    s=60, edgecolors="white")
    plt.colorbar(sc, ax=ax, label="correlation (ρ)")
    ax.set_xlabel("methylation β")
    ax.set_ylabel("expression (log2)")
    ax.set_title("Methylation vs expression (per gene)")
    return savefig(fig, out)


@register_figure(FIGURES, "methylation_driven_gene_heatmap")
def methylation_driven_gene_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "correlations_path", "correlations.tsv")
    cols = load_tsv_columns(p) or {}
    genes = cols.get("gene", [])
    corr = np.array([float(x) for x in cols.get("correlation", [])])
    if not genes:
        raise ValueError("no genes")
    order = np.argsort(corr)  # most negative first → strongest methylation-driven
    top = order[:min(20, len(order))]
    matrix = corr[top].reshape(-1, 1)
    return heatmap(matrix=matrix, row_labels=[genes[i] for i in top],
                   col_labels=["correlation"],
                   title="Top methylation-driven genes",
                   out=out, figsize=(4.5, max(4.0, 0.3 * len(top))))
