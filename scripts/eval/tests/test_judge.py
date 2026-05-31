from scripts.eval.services.judge import parse_verdict

RUBRIC = {"criteria": [
    {"id": "c1", "dimension": "method_selection", "points": 4, "levels": {"A":1.0,"B":0.5,"C":0.0}},
    {"id": "c2", "dimension": "method_selection", "points": 2, "levels": {"A":1.0,"B":0.5,"C":0.0}},
    {"id": "c3", "dimension": "source_reliability", "points": 4, "levels": {"A":1.0,"B":0.5,"C":0.0}},
]}

def test_parse_full_marks():
    out = parse_verdict(RUBRIC, "c1: A\nc2: A\nc3: A")
    assert out["overall"] == 100.0
    assert out["dimensions"]["method_selection"] == 100.0

def test_parse_partial_and_dimension_rollup():
    # c1 B(=0.5*4=2), c2 A(=2), c3 C(=0): earned=4 of 10 -> 40.0
    out = parse_verdict(RUBRIC, "- c1: B\n- c2: A\n- c3: C")
    assert out["overall"] == 40.0
    # method_selection: (2+2)/(4+2)=66.7; source_reliability: 0/4=0
    assert round(out["dimensions"]["method_selection"], 1) == 66.7
    assert out["dimensions"]["source_reliability"] == 0.0

def test_missing_criterion_scores_zero():
    out = parse_verdict(RUBRIC, "c1: A")  # c2,c3 absent
    assert out["overall"] == 40.0  # only c1: 4 of 10
