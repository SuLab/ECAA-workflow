from scripts.eval.benchmark import Score
from scripts.eval.plugins.nekrutenko import Nekrutenko

def test_report_aggregates_jaccard_by_arm():
    rows = [
        Score("mtdna","ecaa",0, 100.0, {}, jaccard=1.0, error_cells=[], judge_id="deterministic"),
        Score("mtdna","claude-direct",0, 50.0, {}, jaccard=0.5, error_cells=[], judge_id="deterministic"),
    ]
    card = Nekrutenko().report(rows)
    assert card.benchmark == "nekrutenko"
    assert len(card.rows) == 2
