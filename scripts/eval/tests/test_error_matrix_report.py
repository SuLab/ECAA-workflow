# scripts/eval/tests/test_error_matrix_report.py
from scripts.eval.benchmark import Score
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services.scorecard import write_scorecard


def _cells(rec):
    return [
        {
            "pattern": "flake_first_call",
            "tool": "bwa",
            "seed": s,
            "handle": "recover" if rec else "crash",
            "recover": rec,
            "diagnose": rec,
        }
        for s in (42, 43, 44)
    ]


def test_error_matrix_rollup_and_render(tmp_path):
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, _cells(True), "deterministic"),
        Score("mtdna", "claude-direct", 0, 50.0, {}, 0.5, _cells(False), "deterministic"),
    ]
    card = Nekrutenko().report(rows)
    em = card.meta["error_matrix"]
    assert em["ecaa"]["recover_rate"] == 1.0 and em["claude-direct"]["recover_rate"] == 0.0
    md = (write_scorecard(card, tmp_path) / "scorecard.md").read_text()
    assert "Error matrix" in md and "flake_first_call" in md
