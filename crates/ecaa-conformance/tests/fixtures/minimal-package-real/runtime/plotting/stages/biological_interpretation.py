"""Biological interpretation / pathway enrichment stage. Reads a
manifest of enrichment results: set names + overlap sizes + corrected
p-values. Applies across bulk + scRNA + proteomics since the underlying
data shape (set vs feature list) is modality-agnostic.

Expected inputs:
- manifest.json with `enrichments: [{id, term, n_overlap, n_set,
  n_universe, p_value, adj_p_value}, ...]`
- <run>/enrichment.tsv[.gz] with columns {term, p_value, adj_p_value, n_overlap}
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import List, Optional

import numpy as np

from ..core import (
    FigureContext,
    bar,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("biological_interpretation")
VIEWS = stage_view_registry("biological_interpretation")


def _load_enrichments(ctx: FigureContext) -> Optional[list]:
    enrichments = ctx.manifest.get("enrichments")
    if isinstance(enrichments, list) and enrichments:
        return enrichments
    # fallback TSV
    for name in ("enrichment.tsv.gz", "enrichment.tsv"):
        p = ctx.outputs_dir / name
        if not p.exists():
            continue
        opener = gzip.open if name.endswith(".gz") else open
        try:
            out = []
            with opener(p, "rt") as f:
                header = f.readline().rstrip("\n").split("\t")
                try:
                    i_t = header.index("term")
                    i_p = header.index("p_value") if "p_value" in header else header.index("pvalue")
                    i_adj = header.index("adj_p_value") if "adj_p_value" in header else (
                        header.index("adj_p") if "adj_p" in header else (header.index("padj") if "padj" in header else None)
                    )
                    i_n = header.index("n_overlap") if "n_overlap" in header else None
                except ValueError:
                    continue
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= max(i_t, i_p):
                        continue
                    try:
                        row = {
                            "term": parts[i_t],
                            "p_value": float(parts[i_p]),
                        }
                        if i_adj is not None and len(parts) > i_adj:
                            row["adj_p_value"] = float(parts[i_adj])
                        if i_n is not None and len(parts) > i_n:
                            row["n_overlap"] = int(float(parts[i_n]))
                        out.append(row)
                    except ValueError:
                        continue
            if out:
                return out
        except OSError:
            continue
    return None


@register_figure(FIGURES, "top_enriched_terms")
def top_enriched_terms(ctx: FigureContext, out: Path):
    enrichments = _load_enrichments(ctx)
    if not enrichments:
        raise FileNotFoundError("no enrichments in manifest or enrichment.tsv")
    # Rank by -log10(adj_p) or -log10(p)
    def score(e: dict) -> float:
        p = e.get("adj_p_value") or e.get("p_value") or 1.0
        try:
            return -np.log10(max(float(p), 1e-300))
        except (TypeError, ValueError):
            return 0.0

    top = sorted(enrichments, key=score, reverse=True)[:20]
    names = [str(e.get("term", "?"))[:40] for e in top]
    values = [score(e) for e in top]
    return bar(
        names=names,
        values=values,
        title="Top enriched terms",
        ylabel="-log10(adj_p)",
        out=out,
        figsize=(9.0, max(5.0, 0.3 * len(names))),
    )


@register_view(VIEWS, "enrichment_table")
def view_enrichment_table(ctx: FigureContext) -> dict:
    enrichments = _load_enrichments(ctx)
    if not enrichments:
        raise FileNotFoundError("no enrichments")
    rows = []
    for e in enrichments:
        p = e.get("adj_p_value") or e.get("p_value") or 1.0
        try:
            nlp = float(-np.log10(max(float(p), 1e-300)))
        except (TypeError, ValueError):
            nlp = 0.0
        rows.append(
            {
                "term": str(e.get("term", "?")),
                "p_value": float(e.get("p_value") or 0.0),
                "adj_p_value": float(e.get("adj_p_value") or 0.0),
                "n_overlap": int(e.get("n_overlap") or 0),
                "neg_log10_p": nlp,
            }
        )
    rows.sort(key=lambda r: -r["neg_log10_p"])
    return {"rows": rows[:200]}
