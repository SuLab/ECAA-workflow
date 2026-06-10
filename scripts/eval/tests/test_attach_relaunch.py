"""H2: the eval_runner threading that surfaces the per-row ECAA relaunch budget
onto Score.extra (which the scorecard then aggregates). Complements
test_relaunch_budget_pin.py (which covers the pin + RunResult field + scorecard
aggregation) by testing the run-loop attach step that connects them.
"""
from scripts.eval.benchmark import Score
from scripts.eval.eval_runner import _attach_relaunch, _base_key


def _row(arm: str, tid: str, extra=None) -> Score:
    return Score(task_id=tid, arm=arm, trial=0, overall=50.0, dimensions={},
                 jaccard=None, error_cells=None, judge_id="deterministic",
                 extra=dict(extra or {}))


def test_attach_relaunch_stamps_ecaa_rows_and_skips_bare():
    scores = [_row("ecaa", "t1"), _row("ecaa", "t2"), _row("claude-direct", "t1")]
    by_key = {
        _base_key("t1", "ecaa", 0): {"relaunch_count": 0, "resolved_blocks": []},
        _base_key("t2", "ecaa", 0): {"relaunch_count": 2, "resolved_blocks": ["a", "b"]},
        # A bare-arm entry must be ignored even if present — the bare arm has no
        # relaunch loop, so its rows carry nothing.
        _base_key("t1", "claude-direct", 0): {"relaunch_count": 5, "resolved_blocks": ["x"]},
    }
    _attach_relaunch(scores, by_key)
    ecaa = {s.task_id: s for s in scores if s.arm == "ecaa"}
    assert ecaa["t1"].extra["relaunch_count"] == 0
    assert ecaa["t1"].extra["resolved_blocks"] == []
    assert ecaa["t2"].extra["relaunch_count"] == 2
    assert ecaa["t2"].extra["resolved_blocks"] == ["a", "b"]
    bare = next(s for s in scores if s.arm == "claude-direct")
    assert "relaunch_count" not in bare.extra
    assert "resolved_blocks" not in bare.extra


def test_attach_relaunch_leaves_unkeyed_rows_untouched():
    scores = [_row("ecaa", "t3")]
    _attach_relaunch(scores, {})  # no entry for t3
    assert "relaunch_count" not in scores[0].extra


def test_attach_relaunch_coerces_count_and_defaults_blocks():
    scores = [_row("ecaa", "t4")]
    # relaunch_count arrives as a str-ish / None resolved_blocks from a journal rec.
    _attach_relaunch(scores, {_base_key("t4", "ecaa", 0):
                              {"relaunch_count": "3", "resolved_blocks": None}})
    assert scores[0].extra["relaunch_count"] == 3
    assert scores[0].extra["resolved_blocks"] == []
