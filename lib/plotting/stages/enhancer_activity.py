"""Enhancer activity (STARR-seq) renderer — RNA vs DNA scatter + activity-score
histogram."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("enhancer_activity")


@register_figure(FIGURES, "rna_vs_dna_scatter")
def rna_vs_dna_scatter(ctx, out):
    p = resolve_artifact_path(ctx, "activity_path", "activity.tsv")
    cols = load_tsv_columns(p) or {}
    rna = np.array([float(x) for x in cols.get("rna_count", [])])
    dna = np.array([float(x) for x in cols.get("dna_count", [])])
    if rna.size == 0:
        raise ValueError("no activity rows")
    fig, ax = plt.subplots(figsize=(6.5, 6.5))
    ax.loglog(dna, rna, "o", color="#882E72", alpha=0.7, markeredgecolor="white")
    lo = max(min(float(dna.min()), float(rna.min())) / 2.0, 1.0)
    hi = max(float(dna.max()), float(rna.max())) * 2.0
    ax.plot([lo, hi], [lo, hi], linestyle="--", color="#888888", linewidth=0.8)
    ax.set_xlim(lo, hi)
    ax.set_ylim(lo, hi)
    ax.set_xlabel("DNA count (log)")
    ax.set_ylabel("RNA count (log)")
    ax.set_title("Enhancer RNA vs DNA")
    ax.set_aspect("equal", adjustable="box")
    return savefig(fig, out)


@register_figure(FIGURES, "activity_score_histogram")
def activity_score_histogram(ctx, out):
    p = resolve_artifact_path(ctx, "activity_path", "activity.tsv")
    cols = load_tsv_columns(p) or {}
    scores = [float(x) for x in cols.get("activity_score", [])]
    if not scores:
        raise ValueError("no scores")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(scores, bins=20, color="#CC79A7", edgecolor="white")
    ax.axvline(1.0, linestyle="--", color="#000000", label="neutral (=1)")
    ax.set_xlabel("RNA/DNA activity score")
    ax.set_ylabel("enhancers")
    ax.set_title("Activity score histogram")
    ax.legend()
    return savefig(fig, out)
