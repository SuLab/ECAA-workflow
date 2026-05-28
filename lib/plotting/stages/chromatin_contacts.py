"""Chromatin contacts renderer (Hi-C / HiChIP cis vs trans + decay curves)."""

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
)

FIGURES = stage_registry("chromatin_contacts")


@register_figure(FIGURES, "cis_trans_ratio")
def cis_trans_ratio(ctx, out):
    p = resolve_artifact_path(ctx, "contacts_path", "contacts.tsv")
    cols = load_tsv_columns(p) or {}
    a = cols.get("chrom_a", [])
    b = cols.get("chrom_b", [])
    cnts = [float(x) for x in cols.get("count", [])]
    if not a:
        raise ValueError("no contacts")
    cis = sum(c for ca, cb, c in zip(a, b, cnts) if ca == cb)
    trans = sum(c for ca, cb, c in zip(a, b, cnts) if ca != cb)
    return bar(names=["cis", "trans"], values=[cis, trans],
               title="Cis/trans contact totals",
               xlabel="contact type", ylabel="count", out=out)


@register_figure(FIGURES, "distance_decay_curve")
def distance_decay_curve(ctx, out):
    p = resolve_artifact_path(ctx, "contacts_path", "contacts.tsv")
    cols = load_tsv_columns(p) or {}
    dist = np.array([float(x) for x in cols.get("distance", [])])
    cnt = np.array([float(x) for x in cols.get("count", [])])
    mask = dist > 0
    dist = dist[mask]
    cnt = cnt[mask]
    if dist.size == 0:
        raise ValueError("no cis contacts")
    order = np.argsort(dist)
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.loglog(dist[order], cnt[order], marker="o", linewidth=1.5, color="#0072B2")
    ax.set_xlabel("genomic distance (bp)")
    ax.set_ylabel("contact count")
    ax.set_title("Contact decay vs distance")
    ax.grid(True, which="both", linestyle=":", alpha=0.5)
    return savefig(fig, out)
