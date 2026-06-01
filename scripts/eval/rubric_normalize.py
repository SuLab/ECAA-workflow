"""Normalize a BiomniBench task rubric into the judge's internal schema.

Real per-task rubrics are either:
  (a) a dict with a "criteria" (or "rubric") key listing criterion objects, or
  (b) a plain text string (BiomniBench-DA uses rubric.txt — free-form text for
      the LLM judge). In the text case the whole rubric is presented to the judge
      as a single criterion tagged to "scientific_reasoning" so the judge still
      produces a parseable id:A line; per-criterion breakdown is not available
      until the structured rubric format is released.

Maps input to {"criteria":[{"id","dimension","points","levels":{"A","B","C"}}]}.
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


def normalize_rubric(raw) -> dict:
    """Normalize a rubric (dict or plain text) into the judge's internal schema."""
    # Plain-text rubric (BiomniBench-DA rubric.txt): wrap as one holistic criterion.
    if isinstance(raw, str):
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
