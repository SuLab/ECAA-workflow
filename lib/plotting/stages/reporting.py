"""Reporting-stage figures for cross-stage and cross-omics summaries.

Manifest contract:
- manifest.json may include `concordance_matrix` as a 2-D numeric array,
  `row_labels`, `col_labels`, and/or `pathway_overlap` as
  [{label, count}].
- Each `pathway_overlap` entry may additionally carry an OPTIONAL
  direction/NES field (`nes`/`NES`/`normalized_enrichment_score`/
  `score`, sign-only, or a string `direction`/`regulation`/`sign` such
  as "up"/"down"/"enriched"/"depleted"). When present, enriched
  (direction > 0) and depleted (direction < 0) entries render in
  distinct diverging colors instead of a single flat color. This is
  purely additive — `[{label, count}]` with no direction field still
  renders exactly as before.
- When absent, the renderer derives a compact placeholder from upstream
  figure manifests so required report figures still become explicit
  artifacts instead of silently disappearing.

Overlap magnitude contract
--------------------------
`pathway_overlap_bar` plots an OVERLAP MAGNITUDE, so the bar height has
to be a measure — not a row marker. A deposited run supplied twenty
entries each with `count: 1`; the resulting chart was twenty
identical-height bars, i.e. a categorical listing of term names wearing
the costume of a quantitative figure.

The magnitude is therefore resolved on ONE uniform basis for the whole
figure (mixed bases would put incomparable units on a shared axis), in
this order:

1. `leading-edge ∩ significant` — every entry carries a member set
   (`leading_edge`/`leadingEdge`/`members`/`genes`/`features`/
   `entities`) AND the manifest carries the analysis' significant-entity
   set (`significant_entities`/`significant_genes`/
   `significant_features`/`significant_set`). This is the real overlap:
   how much of the term's driving membership is in the significant call
   set.
2. `member set size` — every entry carries a member set but no
   significant set is available to intersect against.
3. `reported count` — the legacy `[{label, count}]` path, using the
   first of `n_overlap`/`overlap`/`n`/`count`/`size` that parses.

The resolved basis is named on the value axis, so the reader is never
left guessing which of the three a bar means.

Degenerate input is DROPPED, not drawn: when two or more entries resolve
to an identical magnitude the axis carries no information, and the
renderer raises so `core.generate()` records the figure as skipped with
the reason. Shipping the equal-height chart would assert a measurement
that was never made. (The `concordance_matrix`/`upstream` placeholder
fallback is exempt — its stated job is to keep a required figure from
vanishing, so applying the guard there would defeat its purpose.)
"""

from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence, Tuple

import numpy as np

from ..core import FigureContext, bar, heatmap, register_figure, stage_registry


FIGURES = stage_registry("reporting")


def _matrix(ctx: FigureContext) -> Tuple[np.ndarray, List[str], List[str]]:
    matrix = ctx.manifest.get("concordance_matrix")
    if isinstance(matrix, list) and matrix:
        arr = np.asarray(matrix, dtype=float)
        rows = [str(x) for x in ctx.manifest.get("row_labels", [])]
        cols = [str(x) for x in ctx.manifest.get("col_labels", [])]
        if arr.ndim == 2 and arr.shape[0] > 0 and arr.shape[1] > 0:
            if len(rows) != arr.shape[0]:
                rows = [f"row_{i + 1}" for i in range(arr.shape[0])]
            if len(cols) != arr.shape[1]:
                cols = [f"col_{i + 1}" for i in range(arr.shape[1])]
            return arr, rows, cols

    upstream = ctx.manifest.get("upstream") or []
    labels: List[str] = []
    counts: List[float] = []
    if isinstance(upstream, list):
        for item in upstream:
            if not isinstance(item, dict):
                continue
            label = str(item.get("stage_id") or item.get("id") or f"stage_{len(labels) + 1}")
            figures = item.get("figures") or []
            labels.append(label)
            counts.append(float(len(figures) if isinstance(figures, list) else 0))
    if labels:
        arr = np.asarray([counts], dtype=float)
        return arr, ["figures"], labels
    raise FileNotFoundError("manifest.concordance_matrix or manifest.upstream required")


def _direction_sign(item: Dict[str, Any]) -> Optional[float]:
    """Extract a signed enrichment-direction value from an overlap entry.

    A numeric NES-like field's sign carries the direction (`nes` > 0 =
    enriched, `nes` < 0 = depleted); a string `direction`/`regulation`/
    `sign` field maps "up"/"enriched"/... to +1 and "down"/"depleted"/...
    to -1. Returns `None` when the entry carries no directional signal
    at all, so legacy `[{label, count}]` inputs (the pre-existing
    contract) are unaffected.
    """
    for key in ("nes", "NES", "normalized_enrichment_score", "score"):
        if key in item:
            try:
                return float(item[key])
            except (TypeError, ValueError):
                continue
    for key in ("direction", "regulation", "sign"):
        if key in item:
            raw = str(item[key]).strip().lower()
            if raw in ("up", "enriched", "increased", "positive", "+", "+1"):
                return 1.0
            if raw in ("down", "depleted", "decreased", "negative", "-", "-1"):
                return -1.0
    return None


#: Keys under which an overlap entry may carry its MEMBER SET (the
#: entities driving the term). A list, or a `|`/`,`/`;`/whitespace-
#: separated string, both parse.
MEMBER_KEYS: Tuple[str, ...] = (
    "leading_edge",
    "leadingEdge",
    "members",
    "genes",
    "features",
    "entities",
    "overlap_members",
)

#: Manifest-level keys under which the analysis' significant-entity set may
#: be supplied, to intersect member sets against.
SIGNIFICANT_SET_KEYS: Tuple[str, ...] = (
    "significant_entities",
    "significant_genes",
    "significant_features",
    "significant_set",
)

#: Per-entry keys for an already-computed overlap magnitude, in resolution
#: order. `count` sits last-but-one because it is the loosest of the
#: names — a producer that means "row exists" reaches for it first.
COUNT_KEYS: Tuple[str, ...] = ("n_overlap", "overlap", "n", "count", "size")

#: Value-axis label per resolved magnitude basis. The label is part of the
#: figure's honesty contract: three different measures must never share an
#: unqualified "count" axis.
BASIS_AXIS_LABEL: Dict[str, str] = {
    "leading_edge_intersect_significant": "|members ∩ significant set|",
    "member_set_size": "member set size",
    "reported_count": "reported overlap count",
}


def _as_member_set(value: Any) -> Optional[frozenset]:
    """Parse an entry's member set. Returns `None` (not the empty set) when
    the entry carries no member field at all — "absent" and "empty" select
    different magnitude bases and must stay distinguishable."""
    if isinstance(value, (list, tuple, set, frozenset)):
        return frozenset(str(x).strip() for x in value if str(x).strip())
    if isinstance(value, str):
        raw = value.replace(",", "|").replace(";", "|").replace("\t", "|")
        parts = [p.strip() for chunk in raw.split("|") for p in chunk.split()]
        return frozenset(p for p in parts if p)
    return None


def _members(item: Dict[str, Any]) -> Optional[frozenset]:
    for key in MEMBER_KEYS:
        if key in item:
            parsed = _as_member_set(item[key])
            if parsed is not None:
                return parsed
    return None


def _significant_set(ctx: FigureContext) -> Optional[frozenset]:
    for key in SIGNIFICANT_SET_KEYS:
        if key in ctx.manifest:
            parsed = _as_member_set(ctx.manifest[key])
            if parsed:
                return parsed
    return None


def _reported_count(item: Dict[str, Any]) -> Optional[float]:
    for key in COUNT_KEYS:
        if key in item:
            try:
                return float(item[key])
            except (TypeError, ValueError):
                continue
    return None


def _overlap(ctx: FigureContext) -> Optional[Tuple[List[Dict[str, Any]], str]]:
    """Parse `manifest.pathway_overlap`/`.overlap` into
    `({label, count, direction}, basis)`.

    `direction` (Optional[float], sign only significant) is the NES
    channel — `None` when an entry carries no directional signal,
    preserving the legacy `[{label, count}]` rendering path.

    `count` is the overlap MAGNITUDE resolved on a single basis for the
    whole figure (see the module docstring); `basis` is the key into
    `BASIS_AXIS_LABEL` naming which one was used, so the caller can label
    the axis with the measure it actually plotted.
    """
    entries = ctx.manifest.get("pathway_overlap") or ctx.manifest.get("overlap")
    if not isinstance(entries, list):
        return None

    parsed: List[Tuple[str, Optional[frozenset], Optional[float], Optional[float]]] = []
    for item in entries:
        if not isinstance(item, dict):
            continue
        label = str(item.get("label") or item.get("term") or item.get("id") or "")
        if not label:
            continue
        parsed.append((label, _members(item), _reported_count(item), _direction_sign(item)))
    if not parsed:
        return None

    # One basis for the whole figure — mixing bases would put incomparable
    # units on a shared axis.
    all_have_members = all(members is not None for _, members, _, _ in parsed)
    significant = _significant_set(ctx) if all_have_members else None
    if all_have_members and significant is not None:
        basis = "leading_edge_intersect_significant"
    elif all_have_members:
        basis = "member_set_size"
    else:
        basis = "reported_count"

    out: Dict[str, Dict[str, Any]] = {}
    for label, members, reported, direction in parsed:
        if basis == "leading_edge_intersect_significant":
            count: Optional[float] = float(len(members & significant))  # type: ignore[operator]
        elif basis == "member_set_size":
            count = float(len(members))  # type: ignore[arg-type]
        else:
            count = reported
        if count is None:
            continue
        out[label] = {"label": label, "count": count, "direction": direction}
    if not out:
        return None
    return list(out.values()), basis


def _degenerate(values: Sequence[float]) -> bool:
    """`True` when two or more bars would resolve to the same magnitude —
    the axis then encodes nothing and the chart is a categorical listing
    dressed as a measurement."""
    return len(values) >= 2 and len(set(values)) == 1


@register_figure(FIGURES, "concordance_heatmap")
def concordance_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    matrix, rows, cols = _matrix(ctx)
    return heatmap(
        matrix,
        row_labels=rows,
        col_labels=cols,
        title="Cross-stage concordance",
        out=out,
        center=None,
        cluster_rows=False,
        cluster_cols=False,
        cbar_label="score",
    )


@register_figure(FIGURES, "pathway_overlap_bar")
def pathway_overlap_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Overlap magnitude per term, colored by enrichment direction.

    The magnitude basis is resolved by `_overlap` and named on the value
    axis. Degenerate input (≥2 entries at one magnitude) raises rather
    than rendering an equal-height chart — `core.generate()` turns that
    into a recorded skip, which is the honest outcome for an overlap
    figure whose producer never measured an overlap.
    """
    overlap = _overlap(ctx)
    directions: List[Optional[float]]
    if overlap is None:
        # Placeholder path — exempt from the degeneracy guard by design
        # (see module docstring).
        matrix, _rows, cols = _matrix(ctx)
        values = matrix.ravel().tolist()
        names = cols if len(cols) == len(values) else [f"set_{i + 1}" for i in range(len(values))]
        directions = [None] * len(values)
        ylabel = "score"
    else:
        entries, basis = overlap
        # Sort by magnitude desc, label asc — the label tiebreak keeps the
        # selected top-20 independent of manifest row order.
        ordered = sorted(entries, key=lambda e: (-e["count"], e["label"]))[:20]
        names = [e["label"] for e in ordered]
        values = [e["count"] for e in ordered]
        directions = [e["direction"] for e in ordered]
        if _degenerate(values):
            raise FileNotFoundError(
                f"overlap magnitudes are degenerate ({len(values)} entries all equal to "
                f"{values[0]:g} on basis '{basis}') — the bar height would encode nothing; "
                "supply per-entry member sets (leading_edge/members/genes) plus a "
                "manifest-level significant-entity set, or a varying n_overlap"
            )
        ylabel = BASIS_AXIS_LABEL[basis]
    if not names:
        raise FileNotFoundError("no overlap values available")
    return bar(
        names,
        values,
        title="Pathway or feature overlap",
        ylabel=ylabel,
        out=out,
        directions=directions if any(d is not None for d in directions) else None,
        direction_labels=(
            "Enriched (direction > 0)",
            "Depleted (direction < 0)",
            "No direction reported",
        ),
    )
