import json
from pathlib import Path
from scripts.eval.rubric_normalize import normalize_rubric
from scripts.eval.services.judge import parse_verdict

# Dict-format rubric (legacy fixture — tests the dict-rubric path)
_DICT_RUBRIC = {"criteria":[
  {"id":"k1","dimension":"method selection","points":4,"text":"chose an appropriate DE method"},
  {"id":"k2","dimension":"statistical rigor","weight":2,"description":"multiple-testing correction applied"},
  {"criterion_id":"k3","axis":"source reliability","max_points":3,"criterion":"cites the primary dataset"}
]}

# Unstructured-text rubric (BiomniBench-DA fixture: a numbered list with NO
# `Criterion K:` / `Levels:` structure — exercises the holistic fallback).
_UNSTRUCTURED_TEXT_RUBRIC = json.loads(
    (Path(__file__).parent / "fixtures" / "bbench_rubric.json").read_text()
)

# Structured-text rubric (the real BiomniBench-DA rubric.txt format): a
# `CRITERIA (N):` header then repeated `Criterion K: <title>` blocks each with a
# `Levels: A=<wA> B=<wB> C=<wC>` line. Final penalty criterion has A=0.
_STRUCTURED_TEXT_RUBRIC = """RUBRIC: Synthetic Differential Expression Task

Total Points: 100/100

Notes: A full-score answer loads, tests, and interprets the data correctly.

CRITERIA (4):

Criterion 1: Data Loading and Quality Control

    Description: Loads the count matrix and applies QC filtering.
    Levels: A=30 B=15 C=0
      [A]: Loads correctly and filters well.
      [B]: Loads but QC is partial.
      [C]: Fails to load.

Criterion 2: Statistical Testing and Multiple Testing Correction

    Description: Runs a paired test with FDR correction.
    Levels: A=40 B=20 C=0
      [A]: Paired test with BH correction.
      [B]: Unpaired or unclear correction.
      [C]: No statistical test.

Criterion 3: Biological Interpretation of Results

    Description: Interprets the findings biologically.
    Levels: A=30 B=15 C=0
      [A]: Clear mechanistic interpretation.
      [B]: Vague interpretation.
      [C]: No interpretation.

Criterion 4: Source Reliability

    Description: Grounds claims in identifiable sources.
    Levels: A=0 B=-5 C=-10
      [A]: Fully traceable.
      [B]: Mostly traceable.
      [C]: Unsourced claims.
"""


def test_dict_rubric_every_criterion_has_required_fields():
    norm = normalize_rubric(_DICT_RUBRIC)
    assert len(norm["criteria"]) == 3
    for c in norm["criteria"]:
        assert set(c) >= {"id", "dimension", "points", "levels"}
        assert c["levels"] == {"A": 1.0, "B": 0.5, "C": 0.0}


def test_dict_rubric_dimensions_canonicalized():
    dims = {c["dimension"] for c in normalize_rubric(_DICT_RUBRIC)["criteria"]}
    assert dims == {"method_selection", "statistical_rigor", "source_reliability"}


def test_dict_rubric_all_A_scores_100():
    norm = normalize_rubric(_DICT_RUBRIC)
    out = parse_verdict(norm, "k1: A\nk2: A\nk3: A")
    assert out["overall"] == 100.0


def test_structured_text_rubric_produces_n_weighted_criteria():
    """A structured rubric.txt yields one criterion per `Criterion K` block,
    with A-weight points summing to ~100 (the source-reliability penalty
    criterion carries A=0 so it does not add to the total)."""
    norm = normalize_rubric(_STRUCTURED_TEXT_RUBRIC)
    crits = norm["criteria"]
    assert len(crits) == 4
    assert [c["id"] for c in crits] == [
        "criterion_1", "criterion_2", "criterion_3", "criterion_4"
    ]
    total = sum(c["points"] for c in crits)
    assert abs(total - 100.0) <= 1.0
    # Per-criterion A-weights are faithful to `Levels:`.
    by_id = {c["id"]: c for c in crits}
    assert by_id["criterion_1"]["points"] == 30.0
    assert by_id["criterion_2"]["points"] == 40.0
    assert by_id["criterion_3"]["points"] == 30.0
    assert by_id["criterion_4"]["points"] == 0.0  # source-reliability A=0


def test_structured_text_rubric_partial_weight_is_faithful():
    """B level maps to wB/wA so a B on a 40-point criterion earns 20."""
    norm = normalize_rubric(_STRUCTURED_TEXT_RUBRIC)
    by_id = {c["id"]: c for c in norm["criteria"]}
    # Criterion 2: A=40 B=20 -> B fraction = 20/40 = 0.5
    assert by_id["criterion_2"]["levels"]["B"] == 0.5
    assert by_id["criterion_2"]["levels"]["A"] == 1.0
    assert by_id["criterion_2"]["levels"]["C"] == 0.0


def test_structured_text_rubric_dimensions_assigned():
    """Each criterion is mapped to one of the 6 dimensions by title keyword."""
    norm = normalize_rubric(_STRUCTURED_TEXT_RUBRIC)
    by_id = {c["id"]: c for c in norm["criteria"]}
    assert by_id["criterion_1"]["dimension"] == "data_handling"
    assert by_id["criterion_2"]["dimension"] == "statistical_rigor"
    assert by_id["criterion_3"]["dimension"] == "biological_interpretation"
    assert by_id["criterion_4"]["dimension"] == "source_reliability"


def test_structured_text_rubric_all_A_scores_100():
    norm = normalize_rubric(_STRUCTURED_TEXT_RUBRIC)
    out = parse_verdict(norm, "criterion_1: A\ncriterion_2: A\ncriterion_3: A\ncriterion_4: A")
    assert out["overall"] == 100.0


def test_structured_text_rubric_partial_weighted_total():
    norm = normalize_rubric(_STRUCTURED_TEXT_RUBRIC)
    # c1 A(30) + c2 B(20) + c3 C(0) + c4 A(0): earned 50 of 100 -> 50.0
    out = parse_verdict(norm, "criterion_1: A\ncriterion_2: B\ncriterion_3: C\ncriterion_4: A")
    assert out["overall"] == 50.0


def test_unstructured_text_rubric_falls_back_to_single_criterion():
    """Plain text with NO `Criterion`/`Levels` structure collapses to one
    holistic criterion (the only case where the holistic fallback applies)."""
    assert isinstance(_UNSTRUCTURED_TEXT_RUBRIC, str)
    norm = normalize_rubric(_UNSTRUCTURED_TEXT_RUBRIC)
    assert len(norm["criteria"]) == 1
    c = norm["criteria"][0]
    assert c["id"] == "overall"
    assert c["dimension"] == "scientific_reasoning"
    assert c["levels"] == {"A": 1.0, "B": 0.5, "C": 0.0}
    assert len(c["text"]) > 20


def test_unstructured_text_rubric_all_A_scores_100():
    norm = normalize_rubric(_UNSTRUCTURED_TEXT_RUBRIC)
    out = parse_verdict(norm, "overall: A")
    assert out["overall"] == 100.0
