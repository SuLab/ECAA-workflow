from scripts.eval.services.judge import parse_verdict, _judge_cost_usd

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


def test_lowercase_levels_scored_identically_to_uppercase():
    """Judge output with lowercase a/b/c must score the same as uppercase A/B/C."""
    upper = parse_verdict(RUBRIC, "c1: A\nc2: B\nc3: C")
    lower = parse_verdict(RUBRIC, "c1: a\nc2: b\nc3: c")
    assert upper["overall"] == lower["overall"]
    assert upper["dimensions"] == lower["dimensions"]
    assert upper["levels"] == lower["levels"]


def test_judge_cost_math():
    # gemini-3.1-pro: $1.25/MTok in + $5.00/MTok out = $6.25 per 1M+1M tokens
    assert _judge_cost_usd("gemini-3.1-pro", 1_000_000, 1_000_000) == 6.25
    # anthropic-opus: $15/MTok in + $75/MTok out = $90.00 per 1M+1M tokens
    assert _judge_cost_usd("anthropic-opus", 1_000_000, 1_000_000) == 90.0
    # unknown judge id returns 0.0
    assert _judge_cost_usd("unknown", 5, 5) == 0.0
