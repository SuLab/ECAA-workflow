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
