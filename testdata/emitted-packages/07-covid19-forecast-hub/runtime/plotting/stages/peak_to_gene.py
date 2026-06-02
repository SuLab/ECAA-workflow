"""Peak-to-gene linking renderer — arc diagram for regulatory edges +
per-cluster link-count bar."""

from __future__ import annotations
import numpy as np

from ..core import (
    arc,
    bar,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    stage_registry,
)

FIGURES = stage_registry("peak_to_gene")


@register_figure(FIGURES, "peak_to_gene_arc")
def peak_to_gene_arc(ctx, out):
    p = resolve_artifact_path(ctx, "links_path", "links.tsv")
    cols = load_tsv_columns(p) or {}
    starts = np.array([float(x) for x in cols.get("peak_start", [])])
    ends = np.array([float(x) for x in cols.get("tss", [])])
    weights = np.array([float(x) for x in cols.get("score", [])])
    if starts.size == 0:
        raise ValueError("no peak-gene links")
    return arc(starts=starts, ends=ends, weights=weights,
               title="Peak-to-gene links (arc width ∝ score)",
               xlabel="genomic position (bp)", out=out)


@register_figure(FIGURES, "links_per_cluster_bar")
def links_per_cluster_bar(ctx, out):
    p = resolve_artifact_path(ctx, "links_path", "links.tsv")
    cols = load_tsv_columns(p) or {}
    clusters = cols.get("cluster", [])
    if not clusters:
        raise ValueError("no clusters")
    counts: dict = {}
    for c in clusters:
        counts[c] = counts.get(c, 0) + 1
    names = sorted(counts.keys())
    values = [float(counts[n]) for n in names]
    return bar(names=names, values=values, title="Links per cluster",
               xlabel="cluster", ylabel="n links", out=out)
