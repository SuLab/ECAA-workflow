"""Chromatin loops renderer — loop counts, size distribution, differential
loops (volcano + arc diagram)."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    arc,
    bar,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
    volcano,
)

FIGURES = stage_registry("chromatin_loops")


@register_figure(FIGURES, "loop_count_bar")
def loop_count_bar(ctx, out):
    p = resolve_artifact_path(ctx, "loops_path", "loops.tsv")
    cols = load_tsv_columns(p) or {}
    n = len(cols.get("loop_id", []))
    return bar(names=["total loops"], values=[float(n)],
               title=f"Loop count: {n:,}",
               xlabel="", ylabel="count", out=out)


@register_figure(FIGURES, "loop_size_distribution")
def loop_size_distribution(ctx, out):
    p = resolve_artifact_path(ctx, "loops_path", "loops.tsv")
    cols = load_tsv_columns(p) or {}
    sizes = [float(x) for x in cols.get("size", [])]
    if not sizes:
        raise ValueError("no loop sizes")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(sizes, bins=20, color="#882E72", edgecolor="white")
    ax.set_xscale("log")
    ax.set_xlabel("loop size (bp)")
    ax.set_ylabel("count")
    ax.set_title("Loop size distribution")
    return savefig(fig, out)


@register_figure(FIGURES, "loop_volcano")
def loop_volcano(ctx, out):
    p = resolve_artifact_path(ctx, "differential_loops_path", "differential_loops.tsv")
    cols = load_tsv_columns(p) or {}
    log2fc = np.array([float(x) for x in cols.get("log2fc", [])])
    pv = np.array([float(x) for x in cols.get("pvalue", [])])
    if log2fc.size == 0:
        raise ValueError("no differential loops")
    nlp = -np.log10(np.clip(pv, 1e-300, 1.0))
    return volcano(log_fc=log2fc, neg_log10_p=nlp,
                   title="Differential loop volcano",
                   out=out,
                   labels=cols.get("loop_id", []))


@register_figure(FIGURES, "differential_loop_arc")
def differential_loop_arc(ctx, out):
    p = resolve_artifact_path(ctx, "differential_loops_path", "differential_loops.tsv")
    cols = load_tsv_columns(p) or {}
    starts = np.array([float(x) for x in cols.get("start", [])])
    ends = np.array([float(x) for x in cols.get("end", [])])
    weights = np.array([float(x) for x in cols.get("log2fc", [])])
    if starts.size == 0:
        raise ValueError("no loops")
    return arc(starts=starts, ends=ends, weights=weights,
               title="Differential loops (arc width ∝ |log2FC|)",
               xlabel="genomic position (bp)", out=out)
