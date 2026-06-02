"""Regulatory variants renderer — cis effect volcano + variant×peak overlap bar."""

from __future__ import annotations
import numpy as np

from ..core import (
    bar,
    load_tsv_columns,
    register_figure,
    resolve_artifact_path,
    stage_registry,
    volcano,
)

FIGURES = stage_registry("regulatory_variants")


@register_figure(FIGURES, "cis_effect_volcano")
def cis_effect_volcano(ctx, out):
    p = resolve_artifact_path(ctx, "scores_path", "cis_scores.tsv")
    cols = load_tsv_columns(p) or {}
    lfc = np.array([float(x) for x in cols.get("log2fc", [])])
    pv = np.array([float(x) for x in cols.get("pvalue", [])])
    if lfc.size == 0:
        raise ValueError("no cis scores")
    nlp = -np.log10(np.clip(pv, 1e-300, 1.0))
    return volcano(log_fc=lfc, neg_log10_p=nlp,
                   title="cis-regulatory effect volcano",
                   out=out, labels=cols.get("variant_id", []))


@register_figure(FIGURES, "variant_peak_overlap_bar")
def variant_peak_overlap_bar(ctx, out):
    p = resolve_artifact_path(ctx, "scores_path", "cis_scores.tsv")
    cols = load_tsv_columns(p) or {}
    overlaps = cols.get("overlaps_peak", [])
    n_overlap = sum(1 for v in overlaps if v == "true")
    n_no = sum(1 for v in overlaps if v == "false")
    return bar(names=["overlaps peak", "no overlap"],
               values=[float(n_overlap), float(n_no)],
               title="Variant × peak overlap",
               xlabel="", ylabel="n variants", out=out)
