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


# ── F12 benchmarkable invariant set (readiness gate) ─────────────────────────

def test_benchmarkable_set_meta_default_excludes_vacuous():
    from scripts.eval.services.scorecard import benchmarkable_set_meta
    # Pre-Phase-1/3 live state: only the referential Inv 2/6 are benchmarkable;
    # Inv 1/5 (signed sink), Inv 4 (refs), Inv 3 (evidence) are excluded.
    m = benchmarkable_set_meta()
    assert m["ready"] == ["decision_justification", "substrate_validity"]
    assert set(m["excluded"]) == {
        "claim_completeness", "cross_graph_integrity",
        "equivalence_failure", "evidence_coverage",
    }
    # Each exclusion carries a phase-anchored reason.
    assert "Phase 1" in m["excluded"]["claim_completeness"]
    assert "04-C5" in m["excluded"]["equivalence_failure"]
    assert "04-C2" in m["excluded"]["evidence_coverage"]


def test_benchmarkable_set_meta_all_phases_done():
    from scripts.eval.services.scorecard import benchmarkable_set_meta
    m = benchmarkable_set_meta(signed_sink=True, refs_projected=True,
                               evidence_from_proofs=True)
    assert len(m["ready"]) == 6
    assert m["excluded"] == {}


def test_scorecard_injects_benchmarkable_set(tmp_path):
    rows = [_row("ecaa", 0, 80.0), _row("claude-direct", 0, 70.0)]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    bs = data["meta"]["benchmarkable_set"]
    assert bs["ready"] == ["decision_justification", "substrate_validity"]
    md = (out / "scorecard.md").read_text()
    assert "Benchmarkable invariant set" in md
    assert "decision_justification" in md
    # An excluded invariant's reason is rendered.
    assert "claim_completeness" in md
