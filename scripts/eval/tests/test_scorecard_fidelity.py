# scripts/eval/tests/test_scorecard_fidelity.py
#
# Fidelity fixes:
#  - partial_judging rows (Gemini failed -> Opus-only fallback) must NOT pollute
#    the Gemini headline aggregates (_by_arm / paired delta / dimensions).
#  - the paired CI must carry a power guardrail: never "significant" at n==1, and
#    an `underpowered` flag below _MIN_POWER_PAIRS.
#  - the Nekrutenko handle-category histogram must render.
import json
from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import (
    _by_arm, paired_delta_summary, write_scorecard, _MIN_POWER_PAIRS,
)


def _row(arm, trial, overall, partial=False):
    extra = {"partial_judging": True} if partial else {}
    return Score("t1", arm, trial, overall, {}, None, None, "gemini-3.1-pro", extra=extra)


# ── partial_judging exclusion from the Gemini headline ───────────────────────

def test_by_arm_excludes_partial_judging_rows():
    rows = [_row("ecaa", 0, 80.0), _row("ecaa", 1, 20.0, partial=True)]
    by = _by_arm(Scorecard("b", rows))
    assert by["ecaa"] == [80.0]  # the Opus-only fallback row is not in the headline


def test_paired_delta_excludes_partial_judging_pairs():
    rows = [
        _row("ecaa", 0, 80.0), _row("claude-direct", 0, 70.0),
        _row("ecaa", 1, 90.0, partial=True), _row("claude-direct", 1, 10.0),
    ]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["n_pairs"] == 1  # trial-1 ecaa is partial-judging -> that pair drops


def test_partial_judging_caveat_rendered(tmp_path):
    rows = [_row("ecaa", 0, 80.0), _row("ecaa", 1, 20.0, partial=True),
            _row("claude-direct", 0, 70.0)]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text().lower()
    assert "partial-judging" in md or "partial judging" in md
    data = json.loads((out / "scorecard.json").read_text())
    assert data["meta"]["partial_judging_excluded"] == 1


# ── power guardrail on the paired CI ─────────────────────────────────────────

def test_single_pair_is_never_significant():
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["n_pairs"] == 1
    assert s["significant"] is False     # a degenerate n==1 CI cannot be significant
    assert s["underpowered"] is True


def test_underpowered_flag_below_threshold():
    rows = []
    for t in range(_MIN_POWER_PAIRS - 1):
        rows += [_row("ecaa", t, 80.0), _row("claude-direct", t, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["underpowered"] is True


def test_not_underpowered_at_threshold():
    rows = []
    for t in range(_MIN_POWER_PAIRS + 2):
        rows += [_row("ecaa", t, 80.0), _row("claude-direct", t, 50.0)]
    s = paired_delta_summary(Scorecard("b", rows))
    assert s["underpowered"] is False


def test_underpowered_banner_in_markdown(tmp_path):
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 50.0)]
    md = (write_scorecard(Scorecard("biomnibench", rows), tmp_path) / "scorecard.md").read_text()
    assert "UNDERPOWERED" in md


# ── Nekrutenko handle-category histogram renders ─────────────────────────────

def test_handle_histogram_renders(tmp_path):
    card = Scorecard("nekrutenko", [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, None, "deterministic")],
        meta={"error_matrix": {"ecaa": {
            "recover_rate": 0.5, "diagnose_rate": 0.5, "n_cells": 4,
            "handle_counts": {"recover": 2, "partial": 1, "propagate": 0, "crash": 1},
            "by_pattern": {}}}})
    md = (write_scorecard(card, tmp_path) / "scorecard.md").read_text()
    assert "recover" in md and "crash" in md
    # The four-tuple signature is rendered for the arm.
    assert "2/1/0/1" in md or ("recover 2" in md and "crash 1" in md)
