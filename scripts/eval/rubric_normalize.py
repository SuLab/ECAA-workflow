"""Normalize a BiomniBench task rubric into the judge's internal schema.

Real per-task rubrics are either:
  (a) a dict with a "criteria" (or "rubric") key listing criterion objects, or
  (b) a plain-text string. BiomniBench-DA ships rubric.txt as STRUCTURED text:
      a `CRITERIA (N):` header followed by repeated `Criterion K: <title>`
      blocks, each with a `Levels: A=<wA> B=<wB> C=<wC>` line whose A-weights
      sum to 100 across the scored criteria (the trailing "Source Reliability"
      criterion is a penalty with A=0). We mirror the dataset's own reference
      scorer (`<task>/tests/llm_judge.py::parse_rubric_levels`) — one criterion
      per `Criterion K` block, A-weight as max points — so the headline score is
      the weighted 0–100 comparable to the paper, not a {0,50,100} collapse.

      Only when the text has NO parseable `Criterion`/`Levels` structure do we
      fall back to a single holistic criterion.

Maps input to {"criteria":[{"id","dimension","points","levels":{"A","B","C"}}]}.
"""
from __future__ import annotations

import re

_DIM_CANON = {
    "data handling": "data_handling", "data_handling": "data_handling",
    "method selection": "method_selection", "method_selection": "method_selection",
    "statistical rigor": "statistical_rigor", "statistical_rigor": "statistical_rigor",
    "biological interpretation": "biological_interpretation",
    "biological_interpretation": "biological_interpretation",
    "scientific reasoning": "scientific_reasoning",
    "scientific_reasoning": "scientific_reasoning",
    "source reliability": "source_reliability", "source_reliability": "source_reliability",
}

# The 6 BiomniBench dimensions. The dataset's reference scorer assigns NO
# dimension (it scores flat criteria), so we infer one per criterion from its
# title via keyword match — ordered most-specific first so e.g. "source"
# (penalty criterion) wins before generic interpretation keywords. Anything
# unmatched falls through to scientific_reasoning (the holistic default).
_DIM_KEYWORDS: tuple[tuple[str, tuple[str, ...]], ...] = (
    ("source_reliability", ("source", "reliab", "citation", "provenance", "traceab")),
    ("statistical_rigor", (
        "statistic", "significance", "p-value", "p value", "fdr", "multiple testing",
        "correction", "auc", "roc", "fold-change", "fold change", "test", "permutation",
        "confidence interval", "power",
    )),
    ("data_handling", (
        "data loading", "data handling", "loading", "preprocess", "quality control",
        "schema", "ingest", "parsing", "qc", "missing value", "normalization",
        "dataset construction", "sample definition",
    )),
    ("biological_interpretation", (
        "biolog", "interpret", "summariz", "summaris", "annotation", "cell type",
        "pathway", "marker", "mechanis", "conclusion",
    )),
    ("method_selection", (
        "method", "clustering", "dimensionality", "alignment", "model selection",
        "approach", "pipeline", "analysis design", "scoring method", "ordering",
        "grouping", "computation", "replication",
    )),
)

# A `CRITERIA (N):` header followed by `Criterion K: <title>` blocks each with a
# `Levels: A=<wA> B=<wB> C=<wC>` line.
_CRITERION_SPLIT = re.compile(r"^Criterion\s+(\d+)\s*:", re.MULTILINE)
_LEVELS = re.compile(r"Levels:\s*((?:[A-Za-z]\s*=\s*-?\d+\s*)+)")
_LEVEL_PAIR = re.compile(r"([A-Za-z])\s*=\s*(-?\d+)")
_DESCRIPTION = re.compile(r"Description:\s*(.+?)(?:\n\s*Levels:|\Z)", re.DOTALL)


def _canon_dim(raw: str) -> str:
    key = str(raw or "").strip().lower()
    return _DIM_CANON.get(key, key.replace(" ", "_"))


def _dim_for_title(title: str) -> str:
    """Map a criterion title to one of the 6 BiomniBench dimensions by keyword.

    Heuristic (the reference scorer carries no dimension assignment): scan the
    title lowercased against an ordered keyword table, most-specific first.
    Unmatched titles default to scientific_reasoning."""
    low = title.lower()
    for dim, keywords in _DIM_KEYWORDS:
        if any(kw in low for kw in keywords):
            return dim
    return "scientific_reasoning"


def _levels_from_weights(weights: dict[str, int]) -> tuple[float, dict[str, float]]:
    """Return (points, levels) for one criterion from its A/B/C weights.

    `points` is the A-weight (the criterion's max). Levels are fractions of
    that max so weighted points stay faithful: A=1.0, B=wB/wA (0.5 fallback when
    wA is 0, e.g. the source-reliability penalty criterion), C=0.0."""
    wa = float(weights.get("A", 0))
    wb = float(weights.get("B", 0))
    b_frac = (wb / wa) if wa else 0.5
    return wa, {"A": 1.0, "B": b_frac, "C": 0.0}


def _parse_structured_text(raw: str) -> list[dict] | None:
    """Parse a structured rubric.txt into criterion dicts, or None if it has no
    parseable `Criterion`/`Levels` structure."""
    parts = _CRITERION_SPLIT.split(raw)
    # split() yields [preamble, n1, body1, n2, body2, ...]; <2 captures => no structure.
    if len(parts) < 3:
        return None
    out: list[dict] = []
    for i in range(1, len(parts), 2):
        n = parts[i].strip()
        body = parts[i + 1] if i + 1 < len(parts) else ""
        m = _LEVELS.search(body)
        if not m:
            continue
        weights = {lm.group(1).upper(): int(lm.group(2))
                   for lm in _LEVEL_PAIR.finditer(m.group(1))}
        if not weights:
            continue
        points, levels = _levels_from_weights(weights)
        # Title is the text between "Criterion K:" and the Description/Levels block.
        title = body.split("\n", 1)[0].strip()
        desc_m = _DESCRIPTION.search(body)
        description = desc_m.group(1).strip() if desc_m else ""
        text = f"{title}: {description}".strip(": ").strip() if description else title
        out.append({
            "id": f"criterion_{n}",
            "dimension": _dim_for_title(title),
            "points": points,
            "levels": levels,
            "text": text or title,
        })
    return out or None


def normalize_rubric(raw) -> dict:
    """Normalize a rubric (dict or plain text) into the judge's internal schema."""
    if isinstance(raw, str):
        structured = _parse_structured_text(raw)
        if structured:
            return {"criteria": structured}
        # No parseable structure: wrap the whole text as one holistic criterion
        # so the judge still emits a parseable `overall: A` line.
        return {"criteria": [{
            "id": "overall",
            "dimension": "scientific_reasoning",
            "points": 10.0,
            "levels": {"A": 1.0, "B": 0.5, "C": 0.0},
            "text": raw.strip(),
        }]}
    criteria_in = raw.get("criteria") or raw.get("rubric") or []
    out = []
    for i, c in enumerate(criteria_in):
        out.append({
            "id": str(c.get("id", c.get("criterion_id", f"c{i+1}"))),
            "dimension": _canon_dim(c.get("dimension", c.get("axis", ""))),
            "points": float(c.get("points", c.get("weight", c.get("max_points", 1)))),
            "levels": {"A": 1.0, "B": 0.5, "C": 0.0},
            "text": str(c.get("text", c.get("description", c.get("criterion", "")))),
        })
    return {"criteria": out}
