# scripts/eval/tests/test_scorecard.py
import json
from pathlib import Path
from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import write_scorecard

def test_write_emits_json_and_md(tmp_path):
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None, "gemini-3.1-pro"),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard("biomnibench", rows, meta={"dataset_revision": "abc123"})
    out = write_scorecard(card, tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    assert data["benchmark"] == "biomnibench"
    assert len(data["rows"]) == 2
    md = (out / "scorecard.md").read_text()
    assert "ecaa" in md and "claude-direct" in md
    # delta line present: ecaa - direct = +10.0
    assert "+10.0" in md or "10.0" in md


def test_dimensions_and_judge_agreement_rendered(tmp_path):
    """meta with dimensions + published_best + judge_agreement renders all three sections."""
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None, "gemini-3.1-pro"),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard(
        "biomnibench",
        rows,
        meta={
            "dimensions": {
                "ecaa": {"method_selection": 60.0},
                "claude-direct": {"method_selection": 50.0},
            },
            "published_best": "X=73.34",
            "judge_agreement": {"exact": 0.9, "kappa": 0.8},
        },
    )
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()

    # Per-dimension section present with expected content.
    assert "Per-dimension" in md
    assert "method_selection" in md
    # delta = 60.0 - 50.0 = +10.0
    assert "+10.0" in md
    # Published best line.
    assert "73.34" in md
    # Judge agreement line.
    assert "0.8" in md


def test_biomnibench_shaped_scorecard_renders_without_error(tmp_path):
    """BiomniBench-shaped card: multi-criterion dimensions, a partial-judging row
    with no judge_exact/judge_kappa, a row carrying incomplete_reason, and meta
    with dimension_note/dimension_source. Must render md + json without crashing."""
    rows = [
        Score("t1", "ecaa", 0, 80.0,
              {"method_selection": 60.0, "result_correctness": 75.0},
              None, None, "gemini-3.1-pro",
              extra={"judge_exact": 0.9, "judge_kappa": 0.8}),
        Score("t2", "ecaa", 0, 55.0,
              {"method_selection": 40.0, "result_correctness": 50.0},
              None, None, "gemini-3.1-pro",
              extra={"partial_judging": True}),  # no judge_exact / judge_kappa
        Score("t3", "claude-direct", 0, 65.0,
              {"method_selection": 50.0, "result_correctness": 55.0},
              None, None, "gemini-3.1-pro",
              extra={"incomplete_reason": "2/3 tasks completed; terminal missing"}),
    ]
    card = Scorecard(
        "biomnibench",
        rows,
        meta={
            "dimensions": {
                "ecaa": {"method_selection": 50.0, "result_correctness": 62.5},
                "claude-direct": {"method_selection": 50.0, "result_correctness": 55.0},
            },
            "dimension_source": "heuristic_title_match",
            "dimension_note": "Per-dimension means are a heuristic; only the overall score is benchmark-faithful.",
            "judge_agreement": {"exact": 0.9},  # kappa intentionally absent
        },
    )
    out = write_scorecard(card, tmp_path)

    md_path = out / "scorecard.md"
    json_path = out / "scorecard.json"
    assert md_path.exists() and json_path.exists()
    md = md_path.read_text()
    assert md.strip()  # non-empty

    data = json.loads(json_path.read_text())
    assert data["benchmark"] == "biomnibench"
    assert len(data["rows"]) == 3

    # Expected sections.
    assert "scorecard" in md
    assert "Per-dimension" in md
    assert "method_selection" in md and "result_correctness" in md
    # dimension_note is rendered.
    assert "heuristic" in md
    # Inter-judge agreement section present even though kappa is missing.
    assert "Inter-judge agreement" in md


def test_error_matrix_and_cost_partial_meta_renders(tmp_path):
    """Error-matrix / cost meta with missing optional keys must not KeyError."""
    rows = [
        Score("t1", "ecaa", 0, 90.0, {}, None, None, "deterministic"),
        Score("t1", "claude-direct", 0, 80.0, {}, None, None, "deterministic"),
    ]
    card = Scorecard(
        "nekrutenko",
        rows,
        meta={
            "error_matrix": {
                # Entry missing diagnose_rate and n_cells.
                "ecaa": {"recover_rate": 0.5},
            },
            "cost": {},  # judge_usd absent
        },
    )
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert md.strip()
    assert "Error matrix" in md
