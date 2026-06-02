"""V(D)J immune-repertoire renderer — covers vdj_reconstruction (per-cell
chain assignment + CDR3 length distribution), cdr3_clonotype_clustering
(public-clonotype size bar + clonotype network), and repertoire_diversity
(rank-frequency curve + V-segment usage bar).

Inputs (any subset, resolved via manifest paths or fixture-dir fallback):
- vdj_per_cell.tsv: {cell, chain, cdr3_length, v_segment}
- clonotype_table.tsv: {clonotype_id, frequency, is_public, n_members}
- diversity_table.tsv: {sample, shannon, gini, chao1}
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    FigureContext,
    bar,
    categorical_palette,
    register_figure,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("vdj_repertoire")


def _load_tsv(path: Path) -> Optional[dict]:
    if not path.exists():
        return None
    try:
        with open(path, "rt") as f:
            header = f.readline().rstrip("\n").split("\t")
            cols: dict = {h: [] for h in header}
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) != len(header):
                    continue
                for h, v in zip(header, parts):
                    cols[h].append(v)
        return cols
    except OSError:
        return None


def _resolve(ctx: FigureContext, manifest_key: str, default_name: str) -> Optional[Path]:
    p = ctx.manifest.get(manifest_key)
    if p:
        candidate = Path(p) if Path(p).is_absolute() else ctx.outputs_dir / p
        if candidate.exists():
            return candidate
    fallback = ctx.outputs_dir / default_name
    return fallback if fallback.exists() else None


@register_figure(FIGURES, "chain_assignment_per_cell_bar")
def chain_assignment_per_cell_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = _resolve(ctx, "vdj_per_cell_path", "vdj_per_cell.tsv")
    if p is None:
        raise FileNotFoundError("vdj_per_cell.tsv")
    cols = _load_tsv(p) or {}
    chains = cols.get("chain") or []
    counts: dict = {}
    for c in chains:
        counts[c] = counts.get(c, 0) + 1
    names = sorted(counts.keys())
    values = [float(counts[n]) for n in names]
    return bar(names=names, values=values, title="Chain assignment per cell",
               xlabel="chain", ylabel="cell count", out=out)


@register_figure(FIGURES, "cdr3_length_distribution")
def cdr3_length_distribution(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = _resolve(ctx, "vdj_per_cell_path", "vdj_per_cell.tsv")
    if p is None:
        raise FileNotFoundError("vdj_per_cell.tsv")
    cols = _load_tsv(p) or {}
    lengths = [int(x) for x in (cols.get("cdr3_length") or []) if x.isdigit()]
    if not lengths:
        raise ValueError("no cdr3_length values")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(lengths, bins=range(min(lengths), max(lengths) + 2),
            color="#0072B2", edgecolor="white")
    ax.set_xlabel("CDR3 length (aa)")
    ax.set_ylabel("cells")
    ax.set_title("CDR3 length distribution")
    return savefig(fig, out)


@register_figure(FIGURES, "public_clonotype_size_bar")
def public_clonotype_size_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = _resolve(ctx, "clonotype_table_path", "clonotype_table.tsv")
    if p is None:
        raise FileNotFoundError("clonotype_table.tsv")
    cols = _load_tsv(p) or {}
    cids = cols.get("clonotype_id") or []
    sizes = cols.get("n_members") or []
    is_public = cols.get("is_public") or []
    pairs = [(c, int(n)) for c, n, pub in zip(cids, sizes, is_public) if pub == "true"]
    pairs.sort(key=lambda x: -x[1])
    if not pairs:
        raise ValueError("no public clonotypes")
    names = [c for c, _ in pairs[:30]]
    values = [float(n) for _, n in pairs[:30]]
    return bar(names=names, values=values, title="Public clonotype sizes",
               xlabel="clonotype", ylabel="n cells", out=out)


@register_figure(FIGURES, "clonotype_network")
def clonotype_network(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Simple radial network: top-N clonotypes as nodes, size proportional to frequency."""
    p = _resolve(ctx, "clonotype_table_path", "clonotype_table.tsv")
    if p is None:
        raise FileNotFoundError("clonotype_table.tsv")
    cols = _load_tsv(p) or {}
    cids = cols.get("clonotype_id") or []
    freqs = [float(x) for x in (cols.get("frequency") or [])]
    pairs = sorted(zip(cids, freqs), key=lambda x: -x[1])[:20]
    if not pairs:
        raise ValueError("no clonotypes")
    n = len(pairs)
    theta = np.linspace(0.0, 2 * np.pi, n, endpoint=False)
    x = np.cos(theta)
    y = np.sin(theta)
    sizes = np.array([f for _, f in pairs]) * 4000 + 60
    fig, ax = plt.subplots(figsize=(6.5, 6.5))
    palette = categorical_palette(n, name="vdj.clonotypes")
    ax.scatter(x, y, s=sizes, c=palette, alpha=0.7, edgecolors="white", linewidths=1.0)
    for xi, yi, (cid, _) in zip(x, y, pairs):
        ax.annotate(cid, (xi, yi), fontsize=7, ha="center", va="center")
    ax.set_xlim(-1.4, 1.4)
    ax.set_ylim(-1.4, 1.4)
    ax.set_aspect("equal")
    ax.set_axis_off()
    ax.set_title("Clonotype network (size proportional to frequency)")
    return savefig(fig, out)


@register_figure(FIGURES, "clonal_frequency_rank")
def clonal_frequency_rank(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = _resolve(ctx, "clonotype_table_path", "clonotype_table.tsv")
    if p is None:
        raise FileNotFoundError("clonotype_table.tsv")
    cols = _load_tsv(p) or {}
    freqs = sorted((float(x) for x in (cols.get("frequency") or [])), reverse=True)
    if not freqs:
        raise ValueError("no frequencies")
    ranks = np.arange(1, len(freqs) + 1)
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.loglog(ranks, freqs, marker="o", color="#D55E00", linewidth=1.5)
    ax.set_xlabel("rank")
    ax.set_ylabel("frequency")
    ax.set_title("Clonal frequency rank-abundance")
    ax.grid(True, which="both", linestyle=":", alpha=0.5)
    return savefig(fig, out)


@register_figure(FIGURES, "v_segment_usage_bar")
def v_segment_usage_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = _resolve(ctx, "vdj_per_cell_path", "vdj_per_cell.tsv")
    if p is None:
        raise FileNotFoundError("vdj_per_cell.tsv")
    cols = _load_tsv(p) or {}
    segs = cols.get("v_segment") or []
    counts: dict = {}
    for s in segs:
        counts[s] = counts.get(s, 0) + 1
    pairs = sorted(counts.items(), key=lambda x: -x[1])[:25]
    if not pairs:
        raise ValueError("no v_segments")
    return bar(names=[k for k, _ in pairs],
               values=[float(v) for _, v in pairs],
               title="V-segment usage", xlabel="V segment",
               ylabel="cell count", out=out)
