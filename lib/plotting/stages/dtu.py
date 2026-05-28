"""DTU renderer — differential-transcript-usage volcano + per-gene stacked bar."""

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
    volcano,
)

FIGURES = stage_registry("dtu")


@register_figure(FIGURES, "dtu_volcano")
def dtu_volcano(ctx, out):
    p = resolve_artifact_path(ctx, "dtu_path", "dtu.tsv")
    cols = load_tsv_columns(p) or {}
    dp = np.array([float(x) for x in cols.get("dProportion", [])])
    pv = np.array([float(x) for x in cols.get("pvalue", [])])
    if dp.size == 0:
        raise ValueError("no DTU rows")
    nlp = -np.log10(np.clip(pv, 1e-300, 1.0))
    return volcano(log_fc=dp, neg_log10_p=nlp,
                   title="DTU volcano (ΔProportion)",
                   out=out, labels=cols.get("transcript", []))


@register_figure(FIGURES, "transcript_usage_stacked_bar")
def transcript_usage_stacked_bar(ctx, out):
    p = resolve_artifact_path(ctx, "dtu_path", "dtu.tsv")
    cols = load_tsv_columns(p) or {}
    genes = cols.get("gene", [])
    transcripts = cols.get("transcript", [])
    dp = [float(x) for x in cols.get("dProportion", [])]
    if not genes:
        raise ValueError("no rows")
    by_gene: dict = {}
    for g, t, d in zip(genes, transcripts, dp):
        by_gene.setdefault(g, []).append((t, abs(d)))
    gene_order = sorted(by_gene.keys())
    transcript_set: list = []
    for g in gene_order:
        for t, _ in by_gene[g]:
            if t not in transcript_set:
                transcript_set.append(t)
    palette = categorical_palette(len(transcript_set), name="dtu.transcripts")
    fig, ax = plt.subplots(figsize=(7.5, 4.5))
    xpos = np.arange(len(gene_order))
    bottom = np.zeros(len(gene_order))
    for i, t in enumerate(transcript_set):
        heights = []
        for g in gene_order:
            v = next((d for tt, d in by_gene[g] if tt == t), 0.0)
            heights.append(v)
        heights = np.array(heights)
        ax.bar(xpos, heights, bottom=bottom, color=palette[i], label=t)
        bottom = bottom + heights
    ax.set_xticks(xpos)
    ax.set_xticklabels(gene_order)
    ax.set_xlabel("gene")
    ax.set_ylabel("|ΔProportion| per transcript")
    ax.set_title("Transcript usage shifts per gene")
    ax.legend(bbox_to_anchor=(1.02, 1.0), loc="upper left", fontsize=8)
    return savefig(fig, out)
