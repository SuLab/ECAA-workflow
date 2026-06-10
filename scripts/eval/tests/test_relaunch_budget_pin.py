from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services import agent_runner
from scripts.eval.services.scorecard import _aggregate_relaunch


def test_scored_run_pins_relaunch_to_zero(monkeypatch):
    # Operator tries to grant the ECAA arm extra relaunches.
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "3")
    # Scored runs are the default; the pin overrides the operator value.
    monkeypatch.delenv("ECAA_EVAL_ALLOW_RELAUNCH", raising=False)
    assert agent_runner._eval_max_relaunch() == 0


def test_relaunch_allowed_only_with_explicit_diagnostic_opt_in(monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "3")
    monkeypatch.setenv("ECAA_EVAL_ALLOW_RELAUNCH", "1")
    assert agent_runner._eval_max_relaunch() == 3


def test_relaunch_zero_when_unset(monkeypatch):
    monkeypatch.delenv("ECAA_EVAL_MAX_RELAUNCH", raising=False)
    monkeypatch.delenv("ECAA_EVAL_ALLOW_RELAUNCH", raising=False)
    assert agent_runner._eval_max_relaunch() == 0


def test_relaunch_opt_in_with_bad_value_falls_back_to_zero(monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_ALLOW_RELAUNCH", "1")
    monkeypatch.setenv("ECAA_EVAL_MAX_RELAUNCH", "not-a-number")
    assert agent_runner._eval_max_relaunch() == 0


def test_runresult_carries_relaunch_count():
    rr = agent_runner.RunResult(
        exit_ok=True, wall_secs=1.0, run_dir=None,
        resolved_blocks=["t1", "t2"], relaunch_count=2)
    assert rr.relaunch_count == 2
    assert rr.resolved_blocks == ["t1", "t2"]


def test_relaunch_count_defaults_zero_in_runresult():
    rr = agent_runner.RunResult(exit_ok=True, wall_secs=0.0, run_dir=None)
    assert rr.relaunch_count == 0
    assert rr.resolved_blocks == []


def _row(arm: str, tid: str, *, relaunch_count=None, resolved_blocks=None):
    extra: dict = {}
    if relaunch_count is not None:
        extra["relaunch_count"] = relaunch_count
    if resolved_blocks is not None:
        extra["resolved_blocks"] = resolved_blocks
    return Score(task_id=tid, arm=arm, trial=0, overall=50.0, dimensions={},
                 jaccard=None, error_cells=None, judge_id="deterministic",
                 extra=extra)


def test_scorecard_surfaces_per_row_relaunch_count():
    card = Scorecard(benchmark="nekrutenko", rows=[
        _row("ecaa", "t1", relaunch_count=0, resolved_blocks=[]),
        _row("ecaa", "t2", relaunch_count=2, resolved_blocks=["a", "b"]),
        _row("claude-direct", "t1", relaunch_count=0, resolved_blocks=[]),
    ], meta={})
    agg = _aggregate_relaunch(card)
    assert agg["ecaa"]["total_relaunches"] == 2
    assert agg["ecaa"]["rows_with_relaunch"] == 1
    assert agg["ecaa"]["resolved_blocks"] == 2
    assert agg["ecaa"]["n_rows"] == 2
    assert agg["claude-direct"]["total_relaunches"] == 0


def test_scorecard_relaunch_empty_when_no_row_carries_it():
    card = Scorecard(benchmark="nekrutenko", rows=[
        Score(task_id="t", arm="ecaa", trial=0, overall=50.0, dimensions={},
              jaccard=None, error_cells=None, judge_id="deterministic", extra={}),
    ], meta={})
    assert _aggregate_relaunch(card) == {}
