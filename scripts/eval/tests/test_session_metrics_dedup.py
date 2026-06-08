"""Offline tests for the session-metrics dedup fix (WS-0 Issue 3).

A single chat intake can feed N error-matrix cells / trials, so multiple Score
rows carry the SAME session_id. ``_aggregate_session_metrics`` must count UNIQUE
session_ids (rows without one each count once) so the SME-friction session count
isn't inflated by the matrix fan-out, and the per-session method-rec rate divides
by the deduped count.
"""
from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import _aggregate_session_metrics


def _row(arm, trial, sid, followups, *, method_req=0):
    sm = {"followup_count": followups}
    if sid is not None:
        sm["session_id"] = sid
    if method_req:
        sm["method_recommendation_requests"] = method_req
    return Score("mtdna", arm, trial, 90.0, {}, None, None, "deterministic",
                 {"session_metrics": sm})


def test_aggregate_session_metrics_dedups_by_session_id():
    """Two score rows can carry the SAME session (one chat intake feeds N
    error-matrix cells / trials). n_sessions must count UNIQUE session_ids, not
    rows, or the friction metric is inflated by the matrix fan-out."""
    card = Scorecard("nekrutenko", [
        _row("ecaa", 0, "sid-A", 3),
        _row("ecaa", 1, "sid-A", 3),   # same session, second trial
        _row("ecaa", 2, "sid-B", 5),   # distinct session
    ], {})
    out = _aggregate_session_metrics(card)
    assert out["ecaa"]["n_sessions"] == 2, "must dedup by session_id"


def test_aggregate_session_metrics_counts_rows_without_session_id():
    """Rows lacking a session_id can't be deduped — count each (back-compat)."""
    card = Scorecard("nekrutenko", [
        Score("mtdna", "ecaa", 0, 90.0, {}, None, None, "d",
              {"session_metrics": {"followup_count": 1}}),
        Score("mtdna", "ecaa", 1, 90.0, {}, None, None, "d",
              {"session_metrics": {"followup_count": 2}}),
    ], {})
    out = _aggregate_session_metrics(card)
    assert out["ecaa"]["n_sessions"] == 2


def test_aggregate_session_metrics_mixed_sid_and_no_sid():
    """A bucket with both deduped session_ids and id-less rows sums the unique
    session_ids plus each id-less row."""
    card = Scorecard("nekrutenko", [
        _row("ecaa", 0, "sid-A", 1),
        _row("ecaa", 1, "sid-A", 1),   # dup -> 1 unique
        _row("ecaa", 2, None, 2),       # id-less -> counts once
        _row("ecaa", 3, None, 2),       # id-less -> counts once
    ], {})
    out = _aggregate_session_metrics(card)
    # 1 unique session_id + 2 id-less rows = 3.
    assert out["ecaa"]["n_sessions"] == 3


def test_aggregate_session_metrics_method_rate_divides_by_deduped_count():
    """The per-session method-rec rate must divide by the DEDUPED session count,
    not the raw row count, so a fanned-out intake doesn't deflate the rate."""
    card = Scorecard("nekrutenko", [
        _row("ecaa", 0, "sid-A", 1, method_req=2),
        _row("ecaa", 1, "sid-A", 1, method_req=0),   # same session
    ], {})
    out = _aggregate_session_metrics(card)
    a = out["ecaa"]
    assert a["n_sessions"] == 1
    assert a["method_recommendation_requests_total"] == 2
    # 2 requests / 1 deduped session = 2.0 (not 2/2 = 1.0).
    assert a["method_recommendation_request_rate"] == 2.0


def test_aggregate_session_metrics_empty_when_no_session_metrics():
    card = Scorecard("nekrutenko", [
        Score("mtdna", "ecaa", 0, 90.0, {}, None, None, "d", {}),
    ], {})
    assert _aggregate_session_metrics(card) == {}


def test_aggregate_session_metrics_per_arm_isolated():
    """Two arms sharing a (coincidentally) equal session_id string still dedup
    within their own bucket, not across arms."""
    card = Scorecard("nekrutenko", [
        _row("ecaa", 0, "sid-A", 1),
        _row("claude-direct", 0, "sid-A", 1),
    ], {})
    out = _aggregate_session_metrics(card)
    assert out["ecaa"]["n_sessions"] == 1
    assert out["claude-direct"]["n_sessions"] == 1
