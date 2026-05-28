"""HLA peptidomics renderer — peptide search QC, MHC binding predictions,
neoantigen catalog summaries."""

from __future__ import annotations
import numpy as np
import matplotlib.pyplot as plt

from ..core import (
    bar,
    heatmap,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    savefig,
    stage_registry,
)

FIGURES = stage_registry("hla_peptidomics")


@register_figure(FIGURES, "peptide_length_distribution")
def peptide_length_distribution(ctx, out):
    p = resolve_artifact_path(ctx, "peptides_path", "peptides.tsv")
    cols = load_tsv_columns(p) or {}
    lengths = [int(x) for x in cols.get("length", []) if x.isdigit()]
    if not lengths:
        raise ValueError("no peptide lengths")
    counts: dict = {}
    for L in lengths:
        counts[L] = counts.get(L, 0) + 1
    names = [str(k) for k in sorted(counts)]
    values = [float(counts[int(n)]) for n in names]
    return bar(names=names, values=values, title="Peptide length distribution",
               xlabel="length (aa)", ylabel="count", out=out)


@register_figure(FIGURES, "psm_score_distribution")
def psm_score_distribution(ctx, out):
    p = resolve_artifact_path(ctx, "peptides_path", "peptides.tsv")
    cols = load_tsv_columns(p) or {}
    scores = [float(x) for x in cols.get("score", [])]
    if not scores:
        raise ValueError("no scores")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(scores, bins=20, color="#56B4E9", edgecolor="white")
    ax.set_xlabel("PSM score")
    ax.set_ylabel("count")
    ax.set_title("PSM score distribution")
    return savefig(fig, out)


@register_figure(FIGURES, "binder_rank_histogram")
def binder_rank_histogram(ctx, out):
    p = resolve_artifact_path(ctx, "binders_path", "binders.tsv")
    cols = load_tsv_columns(p) or {}
    ranks = [float(x) for x in cols.get("rank", [])]
    if not ranks:
        raise ValueError("no ranks")
    fig, ax = plt.subplots(figsize=(7.0, 4.5))
    ax.hist(ranks, bins=20, color="#009E73", edgecolor="white")
    ax.axvline(2.0, linestyle="--", color="#D55E00",
               label="weak-binder threshold (%rank=2.0)")
    ax.axvline(0.5, linestyle=":", color="#000000",
               label="strong-binder threshold (%rank=0.5)")
    ax.set_xlabel("%rank")
    ax.set_ylabel("count")
    ax.set_title("Binder rank histogram")
    ax.legend()
    return savefig(fig, out)


@register_figure(FIGURES, "allele_presentation_heatmap")
def allele_presentation_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "binders_path", "binders.tsv")
    cols = load_tsv_columns(p) or {}
    alleles = cols.get("allele", [])
    peps = cols.get("peptide", [])
    if not alleles or not peps:
        raise ValueError("no allele/peptide pairs")
    a_order = sorted(set(alleles))
    p_order = sorted(set(peps))
    m = np.zeros((len(p_order), len(a_order)))
    for pep, al in zip(peps, alleles):
        m[p_order.index(pep), a_order.index(al)] = 1.0
    return heatmap(matrix=m, row_labels=p_order, col_labels=a_order,
                   title="Allele × peptide presentation",
                   out=out, figsize=(6.0, max(4.0, 0.25 * len(p_order))))


@register_figure(FIGURES, "neoantigen_per_patient_bar")
def neoantigen_per_patient_bar(ctx, out):
    p = resolve_artifact_path(ctx, "neoantigens_path", "neoantigens.tsv")
    cols = load_tsv_columns(p) or {}
    patients = cols.get("patient", [])
    if not patients:
        raise ValueError("no patients")
    counts: dict = {}
    for pt in patients:
        counts[pt] = counts.get(pt, 0) + 1
    names = sorted(counts.keys())
    values = [float(counts[n]) for n in names]
    return bar(names=names, values=values, title="Neoantigens per patient",
               xlabel="patient", ylabel="n peptides", out=out)


@register_figure(FIGURES, "shared_neoantigen_heatmap")
def shared_neoantigen_heatmap(ctx, out):
    p = resolve_artifact_path(ctx, "neoantigens_path", "neoantigens.tsv")
    cols = load_tsv_columns(p) or {}
    patients = cols.get("patient", [])
    peptides = cols.get("peptide", [])
    if not patients or not peptides:
        raise ValueError("no patient/peptide pairs")
    pt_order = sorted(set(patients))
    pep_order = sorted(set(peptides))
    m = np.zeros((len(pep_order), len(pt_order)))
    for pt, pep in zip(patients, peptides):
        m[pep_order.index(pep), pt_order.index(pt)] = 1.0
    return heatmap(matrix=m, row_labels=pep_order, col_labels=pt_order,
                   title="Shared neoantigens (patient × peptide)",
                   out=out, figsize=(max(4.0, 0.6 * len(pt_order)),
                                     max(4.0, 0.25 * len(pep_order))))
