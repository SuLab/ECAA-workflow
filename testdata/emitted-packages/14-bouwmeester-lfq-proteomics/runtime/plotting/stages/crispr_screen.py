"""CRISPR screen renderer — sgRNA UMI distribution + per-perturbation cell count."""

from __future__ import annotations
import matplotlib.pyplot as plt

from ..core import (
    bar,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("crispr_screen")


@register_figure(FIGURES, "sgrna_umi_histogram")
def sgrna_umi_histogram(ctx, out):
    p = resolve_artifact_path(ctx, "assignments_path", "sgrna_assignments.tsv")
    cols = load_tsv_columns(p) or {}
    umis = [int(x) for x in cols.get("umi_count", []) if x.isdigit()]
    if not umis:
        raise ValueError("no UMI counts")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(umis, bins=20, color="#4EB265", edgecolor="white")
    ax.set_xlabel("sgRNA UMI count per cell")
    ax.set_ylabel("cells")
    ax.set_title("sgRNA UMI distribution")
    return savefig(fig, out)


@register_figure(FIGURES, "cells_per_perturbation_bar")
def cells_per_perturbation_bar(ctx, out):
    p = resolve_artifact_path(ctx, "assignments_path", "sgrna_assignments.tsv")
    cols = load_tsv_columns(p) or {}
    perts = cols.get("perturbation", [])
    counts: dict = {}
    for p in perts:
        counts[p] = counts.get(p, 0) + 1
    names = sorted(counts.keys())
    values = [float(counts[n]) for n in names]
    return bar(names=names, values=values, title="Cells per perturbation",
               xlabel="perturbation", ylabel="n cells", out=out)
