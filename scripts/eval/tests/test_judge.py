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


def test_prompt_emits_verbatim_level_prose_when_present():
    """When a criterion carries `level_text` (the dataset's per-level [A]/[B]/[C]
    prose), `_prompt` must surface it verbatim — not a generic level line — so the
    judge grades against the author's discriminating descriptions."""
    rubric = {"scoring": "absolute", "criteria": [
        {"id": "criterion_1", "dimension": "data_handling", "points": 100,
         "text": "Data Loading and QC",
         "levels": {"A": 100, "B": 50, "C": 0},
         "level_text": {"A": "Loads correctly and filters well.",
                        "B": "Loads but QC is partial.",
                        "C": "Fails to load."}}]}
    p = _prompt(rubric, "t", "a")
    assert "Loads correctly and filters well." in p
    assert "Loads but QC is partial." in p
    assert "Fails to load." in p
    # Still carries the point values alongside the prose.
    assert "(100 points)" in p


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


def test_parse_verdict_surfaces_judge_rationale():
    """The judge's per-criterion free-text reasons + overall_reasoning must be
    surfaced under `rationales` so they can be persisted into the scorecard —
    regression for the lost-rationale bug where parse_verdict read only `level`
    and silently discarded `reason`, leaving the *why* of every score
    unrecoverable from the scorecard."""
    judge_json = json.dumps({
        "criteria": {
            "criterion_1": {"level": "A", "reason": "loaded and filtered well"},
            "criterion_2": {"level": "B", "reason": "multiple-testing correction unclear"},
            "criterion_3": {"level": "C",
                            "reason": "fabricated PubMed citation that does not resolve"},
        },
        "overall_reasoning": "solid analysis, weak sourcing",
    })
    out = parse_verdict(ABS_RUBRIC, judge_json)
    r = out["rationales"]
    assert r["criterion_1"] == "loaded and filtered well"
    assert r["criterion_3"] == "fabricated PubMed citation that does not resolve"
    assert r["overall_reasoning"] == "solid analysis, weak sourcing"


def test_parse_verdict_rationale_empty_for_legacy_line_format():
    """Legacy `id: A` line verdicts carry no reasons -> rationales is an empty
    dict (no crash, no fabricated text)."""
    out = parse_verdict(ABS_RUBRIC, "criterion_1: A\ncriterion_2: A\ncriterion_3: A")
    assert out["rationales"] == {}


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


def test_absolute_penalty_dimension_reports_satisfaction_not_zero():
    """The A=0/B=-5/C=-10 source-reliability penalty dimension must report a
    satisfaction rate (A->100, B->50, C->0), not a misleading flat 0.0%."""
    from scripts.eval.services.judge import parse_verdict
    # criterion_3 in ABS_RUBRIC is source_reliability (points=0, A=0/B=-5/C=-10).
    a = parse_verdict(ABS_RUBRIC, "criterion_1: A\ncriterion_2: A\ncriterion_3: A")
    assert a["dimensions"]["source_reliability"] == 100.0
    b = parse_verdict(ABS_RUBRIC, "criterion_1: A\ncriterion_2: A\ncriterion_3: B")
    assert b["dimensions"]["source_reliability"] == 50.0
    c = parse_verdict(ABS_RUBRIC, "criterion_1: A\ncriterion_2: A\ncriterion_3: C")
    assert c["dimensions"]["source_reliability"] == 0.0


def test_absolute_normal_dimension_value_unchanged():
    """The generalized formula must reproduce the prior value for normal dims."""
    from scripts.eval.services.judge import parse_verdict
    out = parse_verdict(ABS_RUBRIC, "criterion_1: B\ncriterion_2: A\ncriterion_3: A")
    # data_handling: B=15 of A=30 best, worst 0 -> 50.0
    assert out["dimensions"]["data_handling"] == 50.0
    # statistical_rigor: A=70 of 70 -> 100.0
    assert out["dimensions"]["statistical_rigor"] == 100.0


def test_prompt_level_suffix_tracks_scoring_mode():
    """The per-level suffix must say "points" in absolute mode and "weight" in
    fraction mode — a fraction level value (0.5) is a weight, not a point count.

    These assertions are discriminating: if `_criterion_block` ignored the mode
    and always rendered " (N points)", the fraction-mode prompt would contain
    "(0.5 points)" and the `"weight" in fp` / `"(0.5 points)" not in fp` checks
    would both fail.
    """
    from scripts.eval.rubric_normalize import normalize_rubric

    # (a) Absolute mode: the point labelling is byte-stable.
    ap = _prompt(ABS_RUBRIC, "t", "a")
    assert "(30 points)" in ap   # criterion_1 A=30
    assert "(70 points)" in ap   # criterion_2 A=70
    assert "weight" not in ap    # absolute mode never emits the weight label

    # (b) Fraction mode: build a structured rubric whose A-weights do NOT sum to
    # ~100, so normalize_rubric flags scoring=="fraction" and the levels become
    # fractions of each criterion's max.
    raw = (
        "CRITERIA (2):\n"
        "Criterion 1: Data Loading and QC\n"
        "Levels: A=10 B=5 C=0\n"
        "Criterion 2: Statistical Testing\n"
        "Levels: A=10 B=5 C=0\n"
    )
    frac = normalize_rubric(raw)
    assert frac["scoring"] == "fraction", "A-weights summing to 20 must be fraction-mode"
    # Levels are fractions (1.0/0.5/0.0), not absolute points.
    assert frac["criteria"][0]["levels"]["A"] == 1.0
    assert frac["criteria"][0]["levels"]["B"] == 0.5

    fp = _prompt(frac, "t", "a")
    assert "weight" in fp, "fraction-mode prompt must label levels as weights"
    assert "(weight 1)" in fp     # A weight 1.0 -> "(weight 1)"
    assert "(weight 0.5)" in fp   # B weight 0.5 -> "(weight 0.5)"
    # A 0.5 fraction must NOT be mislabelled as half a point, and NO level suffix
    # may render the " (N points)" form in fraction mode. (The static prompt body
    # still says "Do not output numerical points", so we target the level suffix
    # pattern, not the bare word "points".)
    assert "(0.5 points)" not in fp
    assert "(1 points)" not in fp
    assert " points)" not in fp   # the level-suffix points form is absent entirely
