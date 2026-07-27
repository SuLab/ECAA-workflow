"""Biological interpretation / pathway enrichment stage. Reads a
manifest of enrichment results: set names + overlap sizes + corrected
p-values + (optionally) a signed effect. Applies across bulk + scRNA +
proteomics since the underlying data shape (set vs feature list) is
modality-agnostic.

Expected inputs:
- manifest.json with `enrichments: [{id, term, n_overlap, n_set,
  n_universe, p_value, adj_p_value, NES?}, ...]`
- <run>/enrichment.tsv[.gz] with columns {term, p_value, adj_p_value,
  n_overlap} and optionally a signed-effect column (NES / ES /
  normalized_enrichment_score / signed_effect / effect / log2FoldChange)

Ranking + direction contract
----------------------------
The primary figure is `top_significant_terms`: terms ranked by
SIGNIFICANCE (most significant first), with the signed effect carried on
the value axis and in the fill color whenever the inputs supply one.

It is deliberately NOT named `top_enriched_terms`. Ranking by
`-log10(adj_p)` is direction-blind — that score is positive for a
depleted set exactly as it is for an enriched one — so a figure captioned
"top enriched" while ranked on significance silently mislabels every
depleted term it draws. The honest statement of what the ranking computes
is "the most significant terms, whichever direction they run in", and the
direction is then encoded explicitly (signed bar + diverging fill +
legend) rather than asserted in the title.

`top_enriched_terms` stays registered as an ALIAS of the corrected
renderer so the ids already declared in
`config/plot-affordances/registered.yaml` and
`config/stage-atoms/pathway_enrichment.yaml` keep resolving; the alias
renders the corrected figure, not the old one.

The tie-break order (significance ascending, then |effect| descending,
then term ascending) is the same total order
`crates/core/src/report_contract/pathway_ranking.rs` applies, so the
figure and the deterministic top-term selection can never disagree about
which terms are "top".
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np

from ..core import (
    FigureContext,
    bar,
    register_alias,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("biological_interpretation")
VIEWS = stage_view_registry("biological_interpretation")

#: Candidate keys for the SIGNED effect of an enrichment row, in
#: resolution order. Only the sign is load-bearing: > 0 = enriched /
#: up-regulated, < 0 = depleted / down-regulated. A data-driven list — the
#: renderer never assumes one enrichment tool's column name.
EFFECT_KEYS: Tuple[str, ...] = (
    "NES",
    "nes",
    "normalized_enrichment_score",
    "signed_effect",
    "effect",
    "log2FoldChange",
    "log2fc",
    "ES",
    "es",
)

#: Candidate keys for a categorical direction label, consulted only when
#: no numeric effect resolves. Maps to ±1 (magnitude then unknown).
DIRECTION_KEYS: Tuple[str, ...] = ("direction", "regulation", "sign")

_DIRECTION_UP = ("up", "enriched", "increased", "positive", "+", "+1")
_DIRECTION_DOWN = ("down", "depleted", "decreased", "negative", "-", "-1")

#: Candidate manifest keys carrying the significance threshold the stage
#: actually applied. Surfaced in the figure title so the reader knows what
#: "significant" meant for this run instead of having to assume 0.05.
THRESHOLD_KEYS: Tuple[str, ...] = (
    "pathway_fdr_threshold",
    "adj_p_threshold",
    "significance_threshold",
    "fdr_threshold",
)

TOP_N = 20


def _as_float(value: Any) -> Optional[float]:
    """Parse a manifest/TSV cell as a finite float, else `None`."""
    try:
        v = float(value)
    except (TypeError, ValueError):
        return None
    return v if np.isfinite(v) else None


def _effect(entry: Dict[str, Any]) -> Optional[float]:
    """Signed effect of one enrichment row, or `None` when the row carries
    no directional signal at all. A numeric effect wins; a bare categorical
    direction label degrades to ±1 (sign known, magnitude not)."""
    for key in EFFECT_KEYS:
        if key in entry:
            v = _as_float(entry[key])
            if v is not None:
                return v
    for key in DIRECTION_KEYS:
        if key in entry:
            raw = str(entry[key]).strip().lower()
            if raw in _DIRECTION_UP:
                return 1.0
            if raw in _DIRECTION_DOWN:
                return -1.0
    return None


def _significance(entry: Dict[str, Any]) -> Optional[float]:
    """Adjusted p-value when present, else the raw p-value. `None` when
    neither parses — such a row cannot be ranked, and is dropped rather
    than silently sorted as if it were maximally significant (the old
    `e.get("adj_p_value") or e.get("p_value") or 1.0` chain also turned a
    legitimate `adj_p_value == 0.0` into `1.0`, ranking the single most
    significant term LAST)."""
    for key in ("adj_p_value", "adj_p", "padj", "qvalue", "q_value"):
        if key in entry:
            v = _as_float(entry[key])
            if v is not None:
                return v
    for key in ("p_value", "pvalue", "pval"):
        if key in entry:
            v = _as_float(entry[key])
            if v is not None:
                return v
    return None


def _neg_log10(p: Optional[float]) -> float:
    if p is None:
        return 0.0
    return float(-np.log10(max(p, 1e-300)))


def _threshold(ctx: FigureContext) -> Optional[float]:
    for key in THRESHOLD_KEYS:
        if key in ctx.manifest:
            v = _as_float(ctx.manifest[key])
            if v is not None:
                return v
    return None


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
                        header.index("adj_p") if "adj_p" in header else (
                            header.index("padj") if "padj" in header else None
                        )
                    )
                    i_n = header.index("n_overlap") if "n_overlap" in header else None
                except ValueError:
                    continue
                # Signed-effect column: the first EFFECT_KEYS candidate present
                # in the header wins. Absent → the rows carry no direction and
                # the renderer falls back to the unsigned significance axis.
                i_eff = next((header.index(k) for k in EFFECT_KEYS if k in header), None)
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
                        if i_eff is not None and len(parts) > i_eff:
                            eff = _as_float(parts[i_eff])
                            if eff is not None:
                                row["signed_effect"] = eff
                        out.append(row)
                    except ValueError:
                        continue
            if out:
                return out
        except OSError:
            continue
    return None


def _rank_key(entry: Dict[str, Any]) -> Tuple[float, float, str]:
    """Total order matching `report_contract::pathway_ranking`: most
    significant first, ties broken by larger |effect|, then by term name.
    The final tiebreak matters in practice — fgsea emits many rows sharing
    one BH-adjusted p, and without it the "top" term would depend on input
    row order."""
    sig = _significance(entry)
    eff = _effect(entry)
    return (
        sig if sig is not None else float("inf"),
        -abs(eff) if eff is not None else 0.0,
        str(entry.get("term", "")),
    )


@register_figure(FIGURES, "top_significant_terms")
def top_significant_terms(ctx: FigureContext, out: Path):
    """Top-N terms by significance, with direction carried explicitly.

    Rank: adjusted p ascending (raw p when no adjusted column exists),
    |effect| descending as tiebreak, term name as final tiebreak.

    Value axis: the SIGNED effect when the inputs supply one — a depleted
    term then draws on the opposite side of zero from an enriched one and
    the two can never be confused. When no row carries a direction
    (unsigned over-representation output, or a modality with no effect
    column at all), the axis falls back to `-log10(adj_p)` and the figure
    makes no directional claim in either the bars or the title.
    """
    enrichments = _load_enrichments(ctx)
    if not enrichments:
        raise FileNotFoundError("no enrichments in manifest or enrichment.tsv")

    rankable = [e for e in enrichments if isinstance(e, dict) and _significance(e) is not None]
    if not rankable:
        raise FileNotFoundError("no enrichment row carries a parseable p-value")

    threshold = _threshold(ctx)
    if threshold is not None:
        passing = [e for e in rankable if (_significance(e) or 1.0) < threshold]
        # A threshold admitting nothing is reported honestly by falling back
        # to the unfiltered ranking with no significance claim in the title,
        # rather than emitting an empty figure.
        if passing:
            rankable = passing
        else:
            threshold = None

    top = sorted(rankable, key=_rank_key)[:TOP_N]
    names = [str(e.get("term", "?"))[:40] for e in top]
    effects = [_effect(e) for e in top]
    signed = any(e is not None for e in effects)

    suffix = f" (adj p < {threshold:g})" if threshold is not None else ""
    directions: Optional[List[Optional[float]]]
    if signed:
        # Rows in a signed set that carry no direction of their own plot at
        # zero in the neutral color — visibly "no direction reported",
        # never silently folded into one of the two sign classes.
        values = [e if e is not None else 0.0 for e in effects]
        title = f"Top terms by adjusted p — bar = signed effect{suffix}"
        ylabel = "signed effect (NES / log2FC)"
        directions = effects
    else:
        values = [_neg_log10(_significance(e)) for e in top]
        title = f"Top terms by adjusted p{suffix}"
        ylabel = "-log10(adj_p)"
        directions = None

    return bar(
        names=names,
        values=values,
        title=title,
        ylabel=ylabel,
        out=out,
        figsize=(9.0, max(5.0, 0.3 * len(names))),
        directions=directions,
        direction_labels=(
            "Enriched (effect > 0)",
            "Depleted (effect < 0)",
            "No direction reported",
        ),
    )


# `top_enriched_terms` is the id already declared in the plot-affordance
# registry and in `config/stage-atoms/pathway_enrichment.yaml`. Aliasing it
# onto the corrected renderer fixes every existing caller in place; the id
# itself still wants renaming in those configs (see the module docstring).
register_alias(FIGURES, "top_enriched_terms", "top_significant_terms")


@register_view(VIEWS, "enrichment_table")
def view_enrichment_table(ctx: FigureContext) -> dict:
    enrichments = _load_enrichments(ctx)
    if not enrichments:
        raise FileNotFoundError("no enrichments")
    rows = []
    for e in enrichments:
        if not isinstance(e, dict):
            continue
        sig = _significance(e)
        eff = _effect(e)
        rows.append(
            {
                "term": str(e.get("term", "?")),
                "p_value": float(_as_float(e.get("p_value")) or 0.0),
                "adj_p_value": float(_as_float(e.get("adj_p_value")) or 0.0),
                "n_overlap": int(_as_float(e.get("n_overlap")) or 0),
                "neg_log10_p": _neg_log10(sig),
                # Direction travels with the row so a table consumer can tell
                # an enriched term from a depleted one without re-deriving it
                # from a tool-specific column name.
                "signed_effect": eff,
                "direction": (
                    "enriched" if (eff is not None and eff > 0)
                    else "depleted" if (eff is not None and eff < 0)
                    else "undetermined"
                ),
            }
        )
    rows.sort(key=lambda r: (-r["neg_log10_p"], r["term"]))
    return {"rows": rows[:200]}
