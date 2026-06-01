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

# Text-format rubric (BiomniBench-DA real format: rubric.txt is plain text)
_TEXT_RUBRIC = json.loads(
    (Path(__file__).parent / "fixtures" / "bbench_rubric.json").read_text()
)


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


def test_text_rubric_produces_single_overall_criterion():
    """BiomniBench-DA rubric.txt is plain text — normalize wraps it as one criterion."""
    assert isinstance(_TEXT_RUBRIC, str), "bbench_rubric.json must contain a JSON string"
    norm = normalize_rubric(_TEXT_RUBRIC)
    assert len(norm["criteria"]) == 1
    c = norm["criteria"][0]
    assert c["id"] == "overall"
    assert c["dimension"] == "scientific_reasoning"
    assert c["points"] == 10.0
    assert c["levels"] == {"A": 1.0, "B": 0.5, "C": 0.0}
    assert len(c["text"]) > 20


def test_text_rubric_all_A_scores_100():
    norm = normalize_rubric(_TEXT_RUBRIC)
    out = parse_verdict(norm, "overall: A")
    assert out["overall"] == 100.0


def test_text_rubric_B_scores_50():
    norm = normalize_rubric(_TEXT_RUBRIC)
    out = parse_verdict(norm, "overall: B")
    assert out["overall"] == 50.0
