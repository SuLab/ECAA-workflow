"""Ribo-seq renderer — P-site offset calibration QC + translation-efficiency
differential analysis."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    bar,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
    volcano,
)

FIGURES = stage_registry("ribo_seq")


@register_figure(FIGURES, "psite_offset_per_length")
def psite_offset_per_length(ctx, out):
    p = resolve_artifact_path(ctx, "psite_offset_path", "psite_offset.tsv")
    cols = load_tsv_columns(p) or {}
    lengths = [int(x) for x in cols.get("read_length", []) if x.isdigit()]
    offsets = [int(x) for x in cols.get("offset", []) if x.isdigit()]
    if not lengths:
        raise ValueError("no offsets")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.bar(lengths, offsets, color="#5289C7")
    ax.set_xlabel("read length (nt)")
    ax.set_ylabel("P-site offset (nt)")
    ax.set_title("P-site offset by read length")
    return savefig(fig, out)


@register_figure(FIGURES, "frame_periodicity")
def frame_periodicity(ctx, out):
    p = resolve_artifact_path(ctx, "frame_counts_path", "frame_counts.tsv")
    cols = load_tsv_columns(p) or {}
    frames = cols.get("frame", [])
    counts = [float(x) for x in cols.get("count", [])]
    if not frames:
        raise ValueError("no frames")
    return bar(names=frames, values=counts, title="Frame periodicity",
               xlabel="reading frame", ylabel="reads", out=out)


@register_figure(FIGURES, "te_volcano")
def te_volcano(ctx, out):
    p = resolve_artifact_path(ctx, "te_results_path", "te_results.tsv")
    cols = load_tsv_columns(p) or {}
    lfc = np.array([float(x) for x in cols.get("log2fc_te", [])])
    pv = np.array([float(x) for x in cols.get("pvalue", [])])
    if lfc.size == 0:
        raise ValueError("no TE results")
    nlp = -np.log10(np.clip(pv, 1e-300, 1.0))
    return volcano(log_fc=lfc, neg_log10_p=nlp, title="TE volcano",
                   out=out, labels=cols.get("gene", []))


@register_figure(FIGURES, "te_quadrant_plot")
def te_quadrant_plot(ctx, out):
    p = resolve_artifact_path(ctx, "te_results_path", "te_results.tsv")
    cols = load_tsv_columns(p) or {}
    rna = np.array([float(x) for x in cols.get("log2fc_rna", [])])
    ribo = np.array([float(x) for x in cols.get("log2fc_ribo", [])])
    if rna.size == 0:
        raise ValueError("no TE results")
    fig, ax = plt.subplots(figsize=(6.5, 6.5))
    ax.scatter(rna, ribo, s=30, color="#D55E00", alpha=0.7, edgecolors="white")
    lim = max(float(np.abs(rna).max()), float(np.abs(ribo).max()), 1.0) * 1.1
    ax.set_xlim(-lim, lim)
    ax.set_ylim(-lim, lim)
    ax.axhline(0, color="#000000", linewidth=0.5)
    ax.axvline(0, color="#000000", linewidth=0.5)
    ax.plot([-lim, lim], [-lim, lim], linestyle="--", color="#888888", linewidth=0.8)
    ax.set_xlabel("log2 fold-change (RNA)")
    ax.set_ylabel("log2 fold-change (Ribo)")
    ax.set_title("TE quadrant (RNA vs Ribo)")
    ax.set_aspect("equal")
    return savefig(fig, out)
