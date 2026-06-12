"""Differential-expression stage figures — applies to bulk RNA-seq,
scRNA-seq, and proteomics. Reads a TSV table with log-fold-change +
p-value columns and writes a volcano + a top-N heatmap.

Expected inputs (any subset):
- manifest.json with `comparisons: [{id, table_path, ...}]`
- <comparison>/de_table.tsv[.gz] with columns like
  {feature, log2fc, pvalue, adj_pvalue} — header names tolerated as
  {gene|feature|id, log2FoldChange|logFC|log2fc, pvalue|p_value|p, padj|adj_pvalue|FDR}

Generic feature naming so bulk "gene", scRNA "gene", proteomics "protein"
all work without per-modality branches.
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    heatmap,
    ma_plot,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
    volcano,
)

FIGURES = stage_registry("differential_expression")
VIEWS = stage_view_registry("differential_expression")

_FEATURE_COLS = ("feature", "gene", "protein", "id", "name")
_LOGFC_COLS = ("log2fc", "log2FoldChange", "logFC", "lfc", "log2_fc")
_P_COLS = ("pvalue", "p_value", "p", "pval")
_ADJ_COLS = ("adj_pvalue", "padj", "fdr", "FDR", "adj_p")
_BASEMEAN_COLS = ("base_mean", "baseMean", "mean_expression", "mean_expr", "ave_expr", "AveExpr")


def _find_col(header: List[str], candidates: Tuple[str, ...]) -> Optional[int]:
    lower = [h.lower() for h in header]
    for cand in candidates:
        try:
            return lower.index(cand.lower())
        except ValueError:
            continue
    return None


def _load_de_table(path: Path) -> Optional[Dict[str, np.ndarray]]:
    opener = gzip.open if str(path).endswith(".gz") else open
    try:
        with opener(path, "rt") as f:
            header = f.readline().rstrip("\n").split("\t")
            i_f = _find_col(header, _FEATURE_COLS)
            i_fc = _find_col(header, _LOGFC_COLS)
            i_p = _find_col(header, _P_COLS)
            i_adj = _find_col(header, _ADJ_COLS)
            i_bm = _find_col(header, _BASEMEAN_COLS)
            if i_fc is None or (i_p is None and i_adj is None):
                return None
            features: List[str] = []
            fc: List[float] = []
            pv: List[float] = []
            adj: List[float] = []
            bm: List[float] = []
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) <= max(filter(None, [i_f, i_fc, i_p, i_adj, i_bm])):
                    continue
                try:
                    fc_val = float(parts[i_fc])
                    p_source = i_adj if i_adj is not None else i_p
                    pv_val = float(parts[p_source]) if p_source is not None else 1.0
                    adj_val = float(parts[i_adj]) if i_adj is not None else pv_val
                    bm_val = float(parts[i_bm]) if i_bm is not None else float("nan")
                except ValueError:
                    continue
                features.append(parts[i_f] if i_f is not None else "")
                fc.append(fc_val)
                pv.append(pv_val)
                adj.append(adj_val)
                bm.append(bm_val)
        if not features:
            return None
        pv_arr = np.clip(np.asarray(pv, dtype=float), 1e-300, 1.0)
        return {
            "features": np.asarray(features),
            "log2fc": np.asarray(fc, dtype=float),
            "padj": np.clip(np.asarray(adj, dtype=float), 1e-300, 1.0),
            "base_mean": np.asarray(bm, dtype=float),
            "neg_log10_p": -np.log10(pv_arr),
        }
    except OSError:
        return None


def _find_tables(ctx: FigureContext) -> List[Tuple[str, Path]]:
    """Resolve (comparison_id, path) pairs from manifest.comparisons,
    falling back to a glob of the outputs dir.
    """
    out: List[Tuple[str, Path]] = []
    comparisons = ctx.manifest.get("comparisons")
    if isinstance(comparisons, list):
        for comp in comparisons:
            cid = str(comp.get("id") or "comparison")
            p = comp.get("table_path")
            if p:
                resolved = Path(p)
                if not resolved.is_absolute():
                    resolved = ctx.outputs_dir / resolved
                if resolved.exists():
                    out.append((cid, resolved))
    if out:
        return out
    # Glob fallback
    for candidate in sorted(ctx.outputs_dir.glob("**/de_table.tsv*")):
        cid = candidate.parent.name or "comparison"
        out.append((cid, candidate))
    return out


_VOLCANO_MAX_POINTS = 20_000
_VOLCANO_LABEL_TOP_N = 30


@register_view(VIEWS, "volcano")
def view_volcano(ctx: FigureContext) -> dict:
    """Interactive volcano payload for every comparison in the
    manifest. Subsamples non-significant points above _VOLCANO_MAX_POINTS
    while keeping all significant points + top-scored labels.
    """
    tables = _find_tables(ctx)
    if not tables:
        raise FileNotFoundError("no DE tables")
    out_comparisons = []
    for cid, path in tables:
        data = _load_de_table(path)
        if data is None:
            continue
        fc = data["log2fc"]
        nlp = data["neg_log10_p"]
        feats = [str(f) for f in data["features"]]
        score = np.abs(fc) * nlp
        sig = (np.abs(fc) >= 1.0) & (nlp >= 1.3)
        # Keep every significant point; subsample the non-sig field if
        # needed so the wire payload stays bounded.
        keep_idx = np.where(sig)[0]
        if not sig.all() and len(fc) - keep_idx.size > _VOLCANO_MAX_POINTS:
            non_sig = np.where(~sig)[0]
            draw = ctx.rng.choice(
                non_sig, size=_VOLCANO_MAX_POINTS, replace=False
            )
            draw.sort()
            keep_idx = np.sort(np.concatenate([keep_idx, draw]))
        label_mask = np.zeros(len(fc), dtype=bool)
        top_n_idx = np.argsort(score)[-_VOLCANO_LABEL_TOP_N:]
        label_mask[top_n_idx] = True
        out_comparisons.append(
            {
                "id": cid,
                "n_total": int(len(fc)),
                "n_significant": int(sig.sum()),
                "points": {
                    "log2fc": fc[keep_idx].astype(float).tolist(),
                    "neg_log10_p": nlp[keep_idx].astype(float).tolist(),
                    "significant": sig[keep_idx].tolist(),
                    "labeled": label_mask[keep_idx].tolist(),
                    "feature": [feats[i] for i in keep_idx],
                },
            }
        )
    if not out_comparisons:
        raise FileNotFoundError("no parseable DE tables")
    return {
        "comparisons": out_comparisons,
        "thresholds": {"log2fc": 1.0, "neg_log10_p": 1.3},
    }


@register_figure(FIGURES, "volcano")
def volcano_plot(ctx: FigureContext, out: Path) -> Optional[Path]:
    tables = _find_tables(ctx)
    if not tables:
        raise FileNotFoundError("no DE table resolvable from manifest or outputs_dir")
    # Render the first table only. A future expansion can give each
    # additional comparison its own figure id (volcano_<i>).
    cid, path = tables[0]
    data = _load_de_table(path)
    if data is None:
        raise FileNotFoundError(f"unparseable DE table: {path}")
    return volcano(
        log_fc=data["log2fc"],
        neg_log10_p=data["neg_log10_p"],
        title=f"Differential expression: {cid}",
        out=out,
        labels=list(map(str, data["features"])),
    )


@register_figure(FIGURES, "top_features_heatmap")
def top_features_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Heatmap of the top-N features by |log2FC|·(-log10 p) across every
    comparison in the manifest. When only one comparison exists, renders
    a 1-column heatmap (equivalent to a ranked bar) which is still a
    valid lint artifact.
    """
    tables = _find_tables(ctx)
    if not tables:
        raise FileNotFoundError("no DE tables")
    loaded = []
    for cid, path in tables:
        data = _load_de_table(path)
        if data is None:
            continue
        loaded.append((cid, data))
    if not loaded:
        raise FileNotFoundError("no parseable DE tables")
    # Union top-N features by max score across comparisons
    n_top = 30
    union: Dict[str, float] = {}
    for _cid, data in loaded:
        score = np.abs(data["log2fc"]) * data["neg_log10_p"]
        order = np.argsort(score)[-n_top:][::-1]
        for idx in order:
            feat = str(data["features"][idx])
            if not feat:
                continue
            union[feat] = max(union.get(feat, 0.0), float(score[idx]))
    top_features = [f for f, _ in sorted(union.items(), key=lambda kv: -kv[1])][:n_top]
    matrix = np.zeros((len(top_features), len(loaded)), dtype=float)
    for j, (_cid, data) in enumerate(loaded):
        feat_to_idx = {str(f): i for i, f in enumerate(data["features"])}
        for i, feat in enumerate(top_features):
            idx = feat_to_idx.get(feat)
            if idx is None:
                continue
            matrix[i, j] = float(data["log2fc"][idx])
    return heatmap(
        matrix=matrix,
        row_labels=top_features,
        col_labels=[cid for cid, _ in loaded],
        title="Top features (log2FC)",
        out=out,
        figsize=(max(6.0, 1.0 + 0.5 * len(loaded)), max(6.0, 0.2 * len(top_features))),
    )


@register_figure(FIGURES, "ma_plot")
def ma_plot_figure(ctx: FigureContext, out: Path) -> Optional[Path]:
    """MA plot — log2 fold change vs log10 of mean expression.

    Classic DESeq2 diagnostic that complements the volcano: where the
    volcano emphasizes statistical significance vs effect size, the MA
    plot exposes the relationship between expression level and effect
    size — high-baseMean genes with moderate fold change vs low-baseMean
    genes with dramatic fold change become visually distinct. Requires
    a `baseMean`-equivalent column in the DE table (DESeq2 / edgeR /
    limma-voom all emit one). Renders the first comparison when multiple
    are present; future expansion can emit `ma_plot_<i>` per comparison.
    """
    tables = _find_tables(ctx)
    if not tables:
        raise FileNotFoundError("no DE table resolvable from manifest or outputs_dir")
    cid, path = tables[0]
    data = _load_de_table(path)
    if data is None:
        raise FileNotFoundError(f"unparseable DE table: {path}")
    base_mean = data.get("base_mean")
    if base_mean is None or np.all(np.isnan(base_mean)):
        # MA plot is meaningless without a mean-expression column;
        # rather than render a broken plot or fail the whole stage, skip
        # quietly. The volcano + heatmap still cover this comparison.
        raise FileNotFoundError(
            "DE table has no base_mean / mean_expression column (required for MA plot)"
        )
    # Build a minimal frame-like dict the ma_plot primitive accepts via
    # its `_col` helper (works with dicts and pandas DataFrames alike).
    frame = {
        "base_mean": base_mean,
        "log2FoldChange": data["log2fc"],
        "padj": data["padj"],
        "gene": data["features"],
    }
    return ma_plot(
        frame=frame,
        title=f"MA plot: {cid}",
        out=out,
    )
