"""Differential-accessibility stage figures for ATAC-seq, ChIP-seq,
CUT&RUN, and CUT&Tag peak-count analyses.

Manifest contract:
- ``comparisons: [{id, table_path}]`` where each table has columns like
  ``peak``, ``log2fc``, ``pvalue``/``adj_pvalue``, and optional
  ``baseMean``/``mean_accessibility``.
- Fallback glob: ``**/differential_peaks.tsv*`` or ``**/diffacc*.tsv*``.
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import FigureContext, heatmap, ma_plot, register_figure, stage_registry, volcano
from ._shared import find_col, open_text

FIGURES = stage_registry("differential_accessibility")

_FEATURE_COLS = ("peak", "region", "feature", "id", "locus", "interval")
_LOGFC_COLS = ("log2fc", "log2FoldChange", "logFC", "lfc", "log2_fc")
_P_COLS = ("pvalue", "p_value", "p", "pval")
_ADJ_COLS = ("adj_pvalue", "padj", "fdr", "FDR", "q_value", "adj_p")
_BASEMEAN_COLS = (
    "base_mean",
    "baseMean",
    "mean_accessibility",
    "mean_signal",
    "mean_counts",
)


def _load_table(path: Path) -> Optional[Dict[str, np.ndarray]]:
    try:
        with open_text(path) as f:
            header = f.readline().rstrip("\n").split("\t")
            i_feature = find_col(header, _FEATURE_COLS)
            i_fc = find_col(header, _LOGFC_COLS)
            i_p = find_col(header, _P_COLS)
            i_adj = find_col(header, _ADJ_COLS)
            i_base = find_col(header, _BASEMEAN_COLS)
            if i_fc is None or (i_p is None and i_adj is None):
                return None
            max_idx = max(i for i in (i_feature, i_fc, i_p, i_adj, i_base) if i is not None)
            features: List[str] = []
            fc: List[float] = []
            pvals: List[float] = []
            adj: List[float] = []
            base: List[float] = []
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) <= max_idx:
                    continue
                try:
                    fc_val = float(parts[i_fc])
                    p_source = i_adj if i_adj is not None else i_p
                    p_val = float(parts[p_source]) if p_source is not None else 1.0
                    adj_val = float(parts[i_adj]) if i_adj is not None else p_val
                    base_val = float(parts[i_base]) if i_base is not None else float("nan")
                except ValueError:
                    continue
                features.append(parts[i_feature] if i_feature is not None else "")
                fc.append(fc_val)
                pvals.append(p_val)
                adj.append(adj_val)
                base.append(base_val)
        if not fc:
            return None
        p_arr = np.clip(np.asarray(pvals, dtype=float), 1e-300, 1.0)
        return {
            "features": np.asarray(features),
            "log2fc": np.asarray(fc, dtype=float),
            "padj": np.clip(np.asarray(adj, dtype=float), 1e-300, 1.0),
            "base_mean": np.asarray(base, dtype=float),
            "neg_log10_p": -np.log10(p_arr),
        }
    except OSError:
        return None


def _table_paths(ctx: FigureContext) -> List[Tuple[str, Path]]:
    out: List[Tuple[str, Path]] = []
    comparisons = ctx.manifest.get("comparisons")
    if isinstance(comparisons, list):
        for comp in comparisons:
            if not isinstance(comp, dict):
                continue
            raw = comp.get("table_path") or comp.get("differential_peaks_table")
            if not raw:
                continue
            path = Path(str(raw))
            if not path.is_absolute():
                path = ctx.outputs_dir / path
            if path.exists():
                out.append((str(comp.get("id") or path.parent.name or "comparison"), path))
    if out:
        return out
    for pattern in ("**/differential_peaks.tsv*", "**/diffacc*.tsv*", "**/de_table.tsv*"):
        for path in sorted(ctx.outputs_dir.glob(pattern)):
            out.append((path.parent.name or "comparison", path))
        if out:
            return out
    return out


def _loaded_tables(ctx: FigureContext) -> List[Tuple[str, Dict[str, np.ndarray]]]:
    loaded: List[Tuple[str, Dict[str, np.ndarray]]] = []
    for cid, path in _table_paths(ctx):
        data = _load_table(path)
        if data is not None:
            loaded.append((cid, data))
    if not loaded:
        raise FileNotFoundError("no parseable differential-accessibility tables")
    return loaded


@register_figure(FIGURES, "volcano")
def volcano_plot(ctx: FigureContext, out: Path) -> Optional[Path]:
    cid, data = _loaded_tables(ctx)[0]
    return volcano(
        log_fc=data["log2fc"],
        neg_log10_p=data["neg_log10_p"],
        title=f"Differential accessibility: {cid}",
        out=out,
        labels=[str(x) for x in data["features"]],
    )


@register_figure(FIGURES, "ma_plot")
def ma_plot_figure(ctx: FigureContext, out: Path) -> Optional[Path]:
    cid, data = _loaded_tables(ctx)[0]
    base_mean = data["base_mean"]
    if np.all(np.isnan(base_mean)):
        raise FileNotFoundError("table has no baseMean / mean_accessibility column")
    return ma_plot(
        frame={
            "base_mean": base_mean,
            "log2FoldChange": data["log2fc"],
            "padj": data["padj"],
            "gene": data["features"],
        },
        title=f"MA plot: {cid}",
        out=out,
    )


@register_figure(FIGURES, "diff_heatmap")
def diff_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    loaded = _loaded_tables(ctx)
    n_top = 30
    scores: Dict[str, float] = {}
    for _cid, data in loaded:
        score = np.abs(data["log2fc"]) * data["neg_log10_p"]
        for idx in np.argsort(score)[-n_top:][::-1]:
            peak = str(data["features"][idx])
            if peak:
                scores[peak] = max(scores.get(peak, 0.0), float(score[idx]))
    peaks = [p for p, _ in sorted(scores.items(), key=lambda item: -item[1])][:n_top]
    if not peaks:
        raise FileNotFoundError("no peak labels available for heatmap")
    matrix = np.zeros((len(peaks), len(loaded)), dtype=float)
    for col_idx, (_cid, data) in enumerate(loaded):
        lookup = {str(feature): i for i, feature in enumerate(data["features"])}
        for row_idx, peak in enumerate(peaks):
            src_idx = lookup.get(peak)
            if src_idx is not None:
                matrix[row_idx, col_idx] = float(data["log2fc"][src_idx])
    return heatmap(
        matrix,
        row_labels=peaks,
        col_labels=[cid for cid, _data in loaded],
        title="Differential accessibility log2FC",
        out=out,
        figsize=(max(6.0, 1.0 + 0.8 * len(loaded)), max(6.0, 0.22 * len(peaks))),
    )
