"""Normalize a BiomniBench task rubric into the judge's internal schema.

Real per-task rubrics list criteria tagged to one of 6 evaluation dimensions
with curator-set points and ordinal A/B/C levels. This maps that shape into
{"criteria":[{"id","dimension","points","levels":{"A","B","C"},"text"}]}.
Field reads use tolerant .get() with fallbacks; adjust the key names here once
the real dataset schema is probed live.
"""
from __future__ import annotations

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


def _canon_dim(raw: str) -> str:
    key = str(raw or "").strip().lower()
    return _DIM_CANON.get(key, key.replace(" ", "_"))


def normalize_rubric(raw: dict) -> dict:
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
