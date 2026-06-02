"""Normalize a BiomniBench task rubric into the judge's internal schema.

Real per-task rubrics are either:
  (a) a dict with a "criteria" (or "rubric") key listing criterion objects, or
  (b) a plain-text string. BiomniBench-DA ships rubric.txt as STRUCTURED text:
      a `CRITERIA (N):` header followed by repeated `Criterion K: <title>`
      blocks, each with a `Levels: A=<wA> B=<wB> C=<wC>` line whose A-weights
      sum to 100 across the scored criteria (the trailing "Source Reliability"
      criterion is a penalty with `A=0 B=-5 C=-10`). Each level maps to its
      ABSOLUTE points (including the negative penalty points); the scorer sums
      all criteria and clamps the total to [0, 100]. A perfect run scores 100;
      bad sourcing subtracts up to 10.

      FIDELITY NOTE: this is faithful to the PAPER's documented penalty, but it
      DIVERGES from the dataset's shipped reference scorer
      (`<task>/tests/llm_judge.py`) — whose `Levels:` regex `[A-Z]=\\d+` cannot
      capture negative level values, so it silently drops the B=-5/C=-10 penalty
      (treating them as 0). That reference scorer produced the published 73.34, so
      ECAA scores up to 10 pts BELOW the published number on any imperfect-sourcing
      run. We keep the paper-faithful penalty deliberately; the gap is documented
      here and in the scorecard meta, NOT silently absorbed. We store the absolute
      per-level points in `levels` and flag the rubric `scoring="absolute"` so
      `parse_verdict` reproduces sum-and-clamp (NO division). For the legacy dict-rubric path,
      the holistic fallback, and any structured rubric whose A-weights do NOT sum
      to ~100, we flag `scoring="fraction"` and keep the percentage formula so
      synthetic rubrics still land on 0–100.

      Only when the text has NO parseable `Criterion`/`Levels` structure do we
      fall back to a single holistic criterion.

Maps input to
  {"scoring": "absolute"|"fraction",
   "dimension_source": "heuristic_title_match",
   "criteria": [{"id","dimension","points","levels":{"A","B","C"}}]}.
The per-criterion `dimension` is a HEURISTIC (title-keyword match); the dataset
defines no dimensions, hence the `dimension_source` marker.
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
# The discriminating per-level prose the dataset rubric.txt spells out after the
# `Levels:` line, e.g. `[A]: Correctly loads ...` / `[A] (8 points): ...`. Each
# block runs until the next `[A|B|C]:` line or end of the criterion body.
_LEVEL_PROSE = re.compile(
    r"\[([ABCabc])\]\s*(?:\([^)]*\))?\s*:?\s*(.*?)(?=\n\s*\[[ABCabc]\]\s*(?:\([^)]*\))?\s*:|\Z)",
    re.DOTALL,
)


def _parse_level_prose(body: str) -> dict[str, str]:
    """Extract the verbatim [A]/[B]/[C] level descriptions from a criterion body.

    The dataset reference scorer injects these prose lines verbatim and the judge
    is told to pick the level "based purely on which level description best
    describes the agent's work" — so retaining them (vs a generic level line) is
    what makes the judge's A/B/C assignment faithful. Returns {} when absent."""
    prose: dict[str, str] = {}
    for m in _LEVEL_PROSE.finditer(body):
        letter = m.group(1).upper()
        text = m.group(2).strip()
        if text and letter not in prose:
            prose[letter] = text
    return prose


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


# A-weights of a real BiomniBench rubric.txt sum to 100 across all scored
# criteria (the source-reliability penalty contributes A=0). When the summed
# A-weights land in this band we treat the rubric as the dataset's ABSOLUTE
# sum-and-clamp model; otherwise (synthetic / partial rubrics) we keep the
# fraction model so the headline still lands on 0–100.
_ABSOLUTE_AWEIGHT_LO = 99.0
_ABSOLUTE_AWEIGHT_HI = 101.0


def _levels_from_weights(weights: dict[str, int]) -> tuple[float, dict[str, float]]:
    """Return (points, levels) for one criterion from its A/B/C weights.

    `points` is the A-weight (the criterion's max, for reference). `levels` are
    the ABSOLUTE per-level points straight off the `Levels:` line — including
    negatives, e.g. the source-reliability penalty `{A:0, B:-5, C:-10}`. The
    absolute-mode scorer sums these and clamps to [0, 100]."""
    wa = float(weights.get("A", 0))
    wb = float(weights.get("B", 0))
    wc = float(weights.get("C", 0))
    return wa, {"A": wa, "B": wb, "C": wc}


def _parse_structured_text(raw: str) -> list[dict] | None:
    """Parse a structured rubric.txt into criterion dicts, or None if it has no
    parseable `Criterion`/`Levels` structure. Levels carry ABSOLUTE per-level
    points (see `_levels_from_weights`)."""
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
            # Verbatim [A]/[B]/[C] prose so the judge grades against the author's
            # discriminating level descriptions, not a generic level line.
            "level_text": _parse_level_prose(body),
        })
    return out or None


def _wrap(criteria: list[dict], scoring: str) -> dict:
    """Wrap criteria with the scoring mode + the dimension-honesty marker.

    `dimension_source` flags that per-criterion `dimension` is a title-keyword
    heuristic, NOT a benchmark-defined axis — downstream per-dimension metrics
    must carry that caveat."""
    return {
        "scoring": scoring,
        "dimension_source": "heuristic_title_match",
        "criteria": criteria,
    }


def normalize_rubric(raw) -> dict:
    """Normalize a rubric (dict or plain text) into the judge's internal schema.

    The returned dict carries a `scoring` flag: `"absolute"` when the parsed
    structured rubric's A-weights sum to ~100 (the real BiomniBench rubric.txt,
    scored by summing absolute per-level points then clamping to [0,100]), else
    `"fraction"` (dict rubrics, holistic fallback, synthetic structured rubrics
    whose weights do not sum to ~100 — scored as a weighted percentage)."""
    if isinstance(raw, str):
        structured = _parse_structured_text(raw)
        if structured:
            # The penalty criterion has A=0; summing all criteria's A-weight
            # tells us whether this is the dataset's 100-point absolute rubric.
            aweight_sum = sum(c["points"] for c in structured)
            if _ABSOLUTE_AWEIGHT_LO <= aweight_sum <= _ABSOLUTE_AWEIGHT_HI:
                return _wrap(structured, "absolute")
            # Synthetic / partial structured rubric: scored as a fraction. Convert
            # absolute levels to per-criterion fractions so the percentage formula
            # in parse_verdict stays faithful (A=1.0, B=wB/wA, C=0.0).
            for c in structured:
                wa = c["points"]
                wb = c["levels"].get("B", 0.0)
                c["levels"] = {"A": 1.0, "B": (wb / wa if wa else 0.5), "C": 0.0}
            return _wrap(structured, "fraction")
        # No parseable structure: wrap the whole text as one holistic criterion
        # so the judge still emits a parseable `overall: A` line.
        return _wrap([{
            "id": "overall",
            "dimension": "scientific_reasoning",
            "points": 10.0,
            "levels": {"A": 1.0, "B": 0.5, "C": 0.0},
            "text": raw.strip(),
        }], "fraction")
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
    return _wrap(out, "fraction")
