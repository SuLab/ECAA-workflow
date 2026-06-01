import json
from pathlib import Path
from scripts.eval.rubric_normalize import normalize_rubric
from scripts.eval.services.judge import parse_verdict

RAW = json.loads((Path(__file__).parent / "fixtures" / "bbench_rubric.json").read_text())


def test_every_criterion_has_required_fields():
    norm = normalize_rubric(RAW)
    assert len(norm["criteria"]) == 3
    for c in norm["criteria"]:
        assert set(c) >= {"id", "dimension", "points", "levels"}
        assert c["levels"] == {"A": 1.0, "B": 0.5, "C": 0.0}


def test_dimensions_canonicalized():
    dims = {c["dimension"] for c in normalize_rubric(RAW)["criteria"]}
    assert dims == {"method_selection", "statistical_rigor", "source_reliability"}


def test_fallback_aliases_and_all_A_scores_100():
    norm = normalize_rubric(RAW)
    out = parse_verdict(norm, "k1: A\nk2: A\nk3: A")
    assert out["overall"] == 100.0
