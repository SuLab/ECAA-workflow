"""Offline tests for the scorecard claim-groundedness visibility metric (WS-3)
plus the granular per-dimension comparator and the public-scorecard
datasets_lock_details provenance breakout (WS-4 surface in scorecard.py).

All assertions run against in-memory Scorecards + tmp_path writes; no network,
no live API, no git.
"""
import json

from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import (
    _aggregate_claim_groundedness,
    _render_claim_groundedness,
    _dimension_pair_counts,
    _parse_datasets_lock_details,
    _markdown,
    write_scorecard,
    write_public_scorecard,
)


def _cg_row(arm, verified, total, ref="result_row", trial=0, tid="t1"):
    return Score(tid, arm, trial, 0.0, {}, None, None, "gemini-3.1-pro",
                 extra={"claim_groundedness": {
                     "verified_count": verified, "total_claims": total,
                     "verified_pct": round(100.0 * verified / total, 1) if total else 0.0,
                     "reference_type": ref}})


# ── _aggregate_claim_groundedness ────────────────────────────────────────────

def test_aggregate_claim_groundedness_rolls_per_arm():
    card = Scorecard("biomnibench", [
        _cg_row("ecaa", 8, 10, tid="t1"),
        _cg_row("ecaa", 3, 6, ref="pmid", tid="t2"),
        _cg_row("claude-direct", 2, 10, tid="t1"),
    ])
    agg = _aggregate_claim_groundedness(card)
    # ecaa: 11 verified / 16 total across 2 rows.
    assert agg["ecaa"]["verified_count"] == 11
    assert agg["ecaa"]["total_claims"] == 16
    assert agg["ecaa"]["verified_pct"] == round(100.0 * 11 / 16, 1)
    assert agg["ecaa"]["n_rows"] == 2
    # Mixed reference types collapse to "mixed".
    assert agg["ecaa"]["reference_type"] == "mixed"
    # claude-direct: 2 / 10.
    assert agg["claude-direct"]["verified_count"] == 2
    assert agg["claude-direct"]["reference_type"] == "result_row"


def test_aggregate_claim_groundedness_empty_when_no_rows_carry_it():
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro")])
    assert _aggregate_claim_groundedness(card) == {}


def test_aggregate_claim_groundedness_zero_total_safe():
    card = Scorecard("biomnibench", [_cg_row("ecaa", 0, 0)])
    agg = _aggregate_claim_groundedness(card)
    assert agg["ecaa"]["total_claims"] == 0
    assert agg["ecaa"]["verified_pct"] == 0.0


def test_aggregate_claim_groundedness_pmid_only_reports_pmid():
    card = Scorecard("biomnibench", [
        _cg_row("ecaa", 4, 5, ref="pmid", tid="t1"),
        _cg_row("ecaa", 2, 3, ref="pmid", tid="t2"),
    ])
    agg = _aggregate_claim_groundedness(card)
    assert agg["ecaa"]["reference_type"] == "pmid"


def test_aggregate_claim_groundedness_default_placeholder_dropped_for_richer():
    # An arm with both the default result_row and a richer pmid type reports the
    # richer type when it's the only meaningful one.
    card = Scorecard("biomnibench", [
        _cg_row("ecaa", 1, 2, ref="result_row", tid="t1"),
        _cg_row("ecaa", 1, 2, ref="pmid", tid="t2"),
    ])
    agg = _aggregate_claim_groundedness(card)
    assert agg["ecaa"]["reference_type"] == "pmid"


# ── _render_claim_groundedness ───────────────────────────────────────────────

def test_render_claim_groundedness_table():
    per_arm = {
        "ecaa": {"verified_count": 11, "total_claims": 16, "verified_pct": 68.8,
                 "reference_type": "mixed", "n_rows": 2},
        "claude-direct": {"verified_count": 2, "total_claims": 10,
                          "verified_pct": 20.0, "reference_type": "result_row",
                          "n_rows": 1},
    }
    lines = _render_claim_groundedness(per_arm)
    md = "\n".join(lines)
    assert "Claim groundedness" in md
    assert "HEURISTIC" in md  # noise caveat is loud
    assert "ecaa" in md and "claude-direct" in md
    assert "68.8" in md and "20.0" in md
    assert "11/16" in md and "2/10" in md
    assert "mixed" in md and "result_row" in md


def test_markdown_renders_groundedness_alongside_headline():
    card = Scorecard("biomnibench", [
        _cg_row("ecaa", 8, 10),
        _cg_row("claude-direct", 2, 10),
    ])
    md = _markdown(card)
    # Headline arm-mean table AND the groundedness section both present.
    assert "| arm | n (trials) | mean | sd |" in md
    assert "Claim groundedness" in md
    # Groundedness sits after the headline table (alongside, not replacing it).
    assert md.index("| arm | n (trials) | mean | sd |") < md.index("Claim groundedness")


def test_markdown_omits_groundedness_when_no_rows_carry_it():
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro"),
    ])
    md = _markdown(card)
    assert "Claim groundedness" not in md


# ── write_scorecard meta injection ───────────────────────────────────────────

def test_write_scorecard_injects_groundedness_into_json_meta(tmp_path):
    card = Scorecard("biomnibench", [
        _cg_row("ecaa", 8, 10),
        _cg_row("claude-direct", 2, 10),
    ])
    out = write_scorecard(card, tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    cg = data["meta"]["claim_groundedness"]
    assert cg["ecaa"]["verified_count"] == 8
    assert cg["claude-direct"]["verified_count"] == 2
    # The rich object is NOT also dumped as a stray scalar bullet in the md.
    md = (out / "scorecard.md").read_text()
    assert "- **claim_groundedness:**" not in md
    # But the rendered section is present.
    assert "Claim groundedness" in md


def test_write_scorecard_respects_preset_groundedness_meta(tmp_path):
    preset = {"ecaa": {"verified_count": 99, "total_claims": 100,
                       "verified_pct": 99.0, "reference_type": "pmid", "n_rows": 1}}
    card = Scorecard("biomnibench", [_cg_row("ecaa", 8, 10)],
                     meta={"claim_groundedness": preset})
    out = write_scorecard(card, tmp_path)
    data = json.loads((out / "scorecard.json").read_text())
    # A pre-set meta value wins (consistent with paired_delta / guard_outcomes).
    assert data["meta"]["claim_groundedness"]["ecaa"]["verified_count"] == 99


# ── _dimension_pair_counts + granular per-dimension comparator ────────────────

def test_dimension_pair_counts_per_arm_per_dimension():
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0, "result_correctness": 70.0},
              None, None, "gemini-3.1-pro", extra={"judge_exact": 0.9}),
        Score("t2", "ecaa", 0, 50.0, {"method_selection": 40.0},
              None, None, "gemini-3.1-pro", extra={"judge_exact": 0.8}),
        # partial-judging row excluded from dimension counts (matches report()).
        Score("t3", "ecaa", 0, 0.0, {"method_selection": 0.0},
              None, None, "anthropic-opus", extra={"partial_judging": True}),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0},
              None, None, "gemini-3.1-pro", extra={"judge_exact": 0.7}),
    ])
    counts = _dimension_pair_counts(card)
    # method_selection: ecaa has 2 headline rows, partial excluded.
    assert counts["ecaa"]["method_selection"] == 2
    assert counts["ecaa"]["result_correctness"] == 1
    assert counts["claude-direct"]["method_selection"] == 1


def test_render_dimensions_shows_n_and_attribution(tmp_path):
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None,
              "gemini-3.1-pro", extra={"judge_exact": 0.9}),
        Score("t2", "ecaa", 0, 50.0, {"method_selection": 40.0}, None, None,
              "gemini-3.1-pro", extra={"judge_exact": 0.8}),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0}, None,
              None, "gemini-3.1-pro", extra={"judge_exact": 0.7}),
    ]
    card = Scorecard("biomnibench", rows, meta={
        "dimensions": {"ecaa": {"method_selection": 50.0},
                       "claude-direct": {"method_selection": 50.0}},
        "dimension_source": "heuristic_title_match",
    })
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    # New per-dimension columns: n for each arm.
    assert "ecaa n" in md and "claude-direct n" in md
    # The granular row carries the counts (ecaa=2 headline rows, direct=1).
    header_to_end = md[md.index("| dimension |"):]
    assert "| method_selection |" in header_to_end
    # Attribution line: the delta is Gemini-headline-derived, partial-excluded.
    assert "Gemini-headline" in md
    assert "partial-judging rows excluded" in md


# ── datasets_lock_details provenance breakout ────────────────────────────────

def test_parse_datasets_lock_details_structured():
    lock = "biomnibench=810b6c54a81e98019bb6c36bdbdc1d4e93dd46d1;mtdna=abc123"
    details = _parse_datasets_lock_details(lock)
    assert details == [
        {"name": "biomnibench", "revision": "810b6c54a81e98019bb6c36bdbdc1d4e93dd46d1"},
        {"name": "mtdna", "revision": "abc123"},
    ]


def test_parse_datasets_lock_details_empty_and_unknown():
    assert _parse_datasets_lock_details("") == []
    assert _parse_datasets_lock_details("unknown") == []


def test_parse_datasets_lock_details_token_without_equals():
    # A malformed / fallback token (no '=') round-trips with an empty revision.
    details = _parse_datasets_lock_details("rawline; name=rev ")
    assert details == [
        {"name": "rawline", "revision": ""},
        {"name": "name", "revision": "rev"},
    ]


def test_write_public_scorecard_adds_lock_details_to_provenance(tmp_path):
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None,
              "gemini-3.1-pro"),
    ])
    out = write_public_scorecard(
        card, tmp_path, git_head="a" * 40,
        datasets_lock="biomnibench=deadbeef;mtdna=cafef00d",
        seed=1729, arms=["ecaa"], trials=1)
    data = json.loads((out / "scorecard.public.json").read_text())
    prov = data["provenance"]
    # Flat one-liner still present for back-compat.
    assert prov["datasets_lock"] == "biomnibench=deadbeef;mtdna=cafef00d"
    # Structured breakout added alongside it.
    assert prov["datasets_lock_details"] == [
        {"name": "biomnibench", "revision": "deadbeef"},
        {"name": "mtdna", "revision": "cafef00d"},
    ]
    # The markdown preamble surfaces the per-entry breakout too.
    md = (out / "scorecard.public.md").read_text()
    assert "datasets_lock_details:" in md
    assert "biomnibench = deadbeef" in md
    assert "mtdna = cafef00d" in md


def test_write_public_scorecard_empty_lock_details_when_unknown(tmp_path):
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro"),
    ])
    out = write_public_scorecard(
        card, tmp_path, git_head="b" * 40, datasets_lock="unknown",
        seed=1729, arms=["ecaa"], trials=1)
    data = json.loads((out / "scorecard.public.json").read_text())
    assert data["provenance"]["datasets_lock_details"] == []
    md = (out / "scorecard.public.md").read_text()
    # No per-entry breakout block when there are no details.
    assert "datasets_lock_details:" not in md
