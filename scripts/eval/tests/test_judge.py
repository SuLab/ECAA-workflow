import json

from scripts.eval.services.judge import parse_verdict, _judge_cost_usd, _prompt

RUBRIC = {"criteria": [
    {"id": "c1", "dimension": "method_selection", "points": 4, "levels": {"A":1.0,"B":0.5,"C":0.0}},
    {"id": "c2", "dimension": "method_selection", "points": 2, "levels": {"A":1.0,"B":0.5,"C":0.0}},
    {"id": "c3", "dimension": "source_reliability", "points": 4, "levels": {"A":1.0,"B":0.5,"C":0.0}},
]}

# A real-shaped (absolute) rubric mirroring the dataset rubric.txt: A-weights
# sum to 100 across scored criteria; the trailing penalty carries A=0.
ABS_RUBRIC = {"scoring": "absolute", "criteria": [
    {"id": "criterion_1", "dimension": "data_handling", "points": 30,
     "text": "Data Loading and QC: loads the matrix and filters.",
     "levels": {"A": 30, "B": 15, "C": 0}},
    {"id": "criterion_2", "dimension": "statistical_rigor", "points": 70,
     "text": "Statistical Testing: paired test with FDR correction.",
     "levels": {"A": 70, "B": 35, "C": 0}},
    {"id": "criterion_3", "dimension": "source_reliability", "points": 0,
     "text": "Source Reliability: grounds claims in identifiable sources.",
     "levels": {"A": 0, "B": -5, "C": -10}},
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


# ---------------------------------------------------------------------------
# Reference-scorer fidelity: prompt framing + JSON output parsing.
# The dataset scorer (<task>/tests/llm_judge.py) frames an expert evaluator,
# wraps trace/answer in <trace>/<answer> tags, presents each criterion's A/B/C
# levels, and requires a JSON verdict {"criteria": {"criterion_1": {"level": …}}}.
# ---------------------------------------------------------------------------

def test_prompt_presents_each_criterion_with_abc_levels():
    """`_prompt` must list every criterion id and surface its A/B/C level lines
    with point values, mirroring the rubric.txt the reference scorer injects."""
    p = _prompt(ABS_RUBRIC, "TRACE TEXT", "ANSWER TEXT")
    for c in ABS_RUBRIC["criteria"]:
        assert c["id"] in p, f"{c['id']} should appear in the prompt"
        assert c["text"] in p, "criterion description text should appear verbatim"
    # Each criterion presents all three levels with their point values.
    assert "[A]" in p and "[B]" in p and "[C]" in p
    assert "(30 points)" in p   # criterion_1 A=30
    assert "(70 points)" in p   # criterion_2 A=70
    assert "(-5 points)" in p   # penalty criterion B=-5
    assert "(-10 points)" in p  # penalty criterion C=-10


def test_prompt_matches_reference_scorer_framing():
    """Framing + output contract mirror the dataset reference scorer."""
    p = _prompt(ABS_RUBRIC, "TRACE TEXT", "ANSWER TEXT")
    assert "expert evaluator" in p
    # trace/answer wrapped in the reference scorer's tags
    assert "<trace>\nTRACE TEXT\n</trace>" in p
    assert "<answer>\nANSWER TEXT\n</answer>" in p
    # The exact JSON output contract is requested.
    assert '"criteria"' in p
    assert '"level"' in p
    assert "Only output the JSON object" in p
    # No numeric-point output requested from the judge (scored automatically).
    assert "Do not output numerical points" in p


def test_parse_verdict_parses_reference_scorer_json():
    """A representative reference-scorer JSON verdict scores faithfully.

    criterion_1 A(30) + criterion_2 B(35) + criterion_3 C(-10) = 55 -> clamp 55."""
    judge_json = json.dumps({
        "criteria": {
            "criterion_1": {"level": "A", "reason": "loaded and filtered well"},
            "criterion_2": {"level": "B", "reason": "test ok but correction unclear"},
            "criterion_3": {"level": "C", "reason": "no source attribution"},
        },
        "overall_reasoning": "solid analysis, weak sourcing",
    })
    out = parse_verdict(ABS_RUBRIC, judge_json)
    assert out["levels"] == {
        "criterion_1": "A", "criterion_2": "B", "criterion_3": "C"}
    assert out["overall"] == 55.0


def test_parse_verdict_json_with_surrounding_prose_and_fences():
    """JSON wrapped in prose / ```json fences (as a model may emit) still parses,
    matching the reference scorer's brace-balanced extraction."""
    wrapped = (
        "Here is my evaluation:\n```json\n"
        + json.dumps({"criteria": {
            "criterion_1": {"level": "A"},
            "criterion_2": {"level": "A"},
            "criterion_3": {"level": "A"},
        }, "overall_reasoning": "perfect"})
        + "\n```\nThat concludes my assessment."
    )
    out = parse_verdict(ABS_RUBRIC, wrapped)
    assert out["overall"] == 100.0  # all-A on absolute rubric


def test_parse_verdict_still_accepts_legacy_line_format():
    """Back-compat: existing `id: A` line verdicts (cached / fixtures) parse."""
    out = parse_verdict(ABS_RUBRIC, "criterion_1: A\ncriterion_2: A\ncriterion_3: A")
    assert out["overall"] == 100.0


def test_parse_verdict_json_levels_override_legacy_lines():
    """When both shapes are present, the JSON `level` wins (it is the contract
    the prompt requests); line matches only fill criteria the JSON omits."""
    mixed = (
        "criterion_1: C\ncriterion_2: C\n"  # legacy lines say all-C
        + json.dumps({"criteria": {
            "criterion_1": {"level": "A"},  # JSON overrides c1 -> A
            # criterion_2 omitted from JSON -> falls back to legacy line (C)
            "criterion_3": {"level": "A"},
        }})
    )
    out = parse_verdict(ABS_RUBRIC, mixed)
    assert out["levels"] == {
        "criterion_1": "A", "criterion_2": "C", "criterion_3": "A"}
    # c1 A(30) + c2 C(0) + c3 A(0) = 30
    assert out["overall"] == 30.0


def test_prompt_criterion_ids_round_trip_through_parse_verdict():
    """The ids `_prompt` presents must be exactly the ids `parse_verdict` credits.

    Build a JSON verdict keyed by the ids the prompt would show, and assert it
    scores as all-A (no id mismatch silently defaulting a criterion to C)."""
    p = _prompt(ABS_RUBRIC, "t", "a")
    verdict = json.dumps({"criteria": {
        c["id"]: {"level": "A"} for c in ABS_RUBRIC["criteria"]}})
    for c in ABS_RUBRIC["criteria"]:
        assert c["id"] in p
    out = parse_verdict(ABS_RUBRIC, verdict)
    assert out["overall"] == 100.0
    assert all(v == "A" for v in out["levels"].values())
