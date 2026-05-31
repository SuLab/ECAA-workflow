# scripts/eval/tests/test_biomnibench_plugin.py
from scripts.eval.benchmark import Score
from scripts.eval.plugins.biomnibench import BiomniBench

def test_report_groups_dimension_means():
    rows = [
        Score("t1","ecaa",0, 80.0, {"method_selection":60.0,"source_reliability":90.0}, None, None, "gemini-3.1-pro"),
        Score("t1","claude-direct",0, 70.0, {"method_selection":50.0,"source_reliability":88.0}, None, None, "gemini-3.1-pro"),
    ]
    card = BiomniBench().report(rows)
    assert card.benchmark == "biomnibench"
    assert card.meta["dimensions"]  # dimension means present
    assert "method_selection" in card.meta["dimensions"]["ecaa"]
