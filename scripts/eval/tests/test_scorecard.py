# scripts/eval/tests/test_scorecard.py
import json
from pathlib import Path
from scripts.eval.benchmark import Score, Scorecard
from scripts.eval.services.scorecard import (
    write_scorecard,
    collect_guard_outcomes,
    paired_delta_summary,
    _aggregate_guard_outcomes,
    _bootstrap_ci,
)

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


def test_dimensions_and_judge_agreement_rendered(tmp_path):
    """meta with dimensions + published_best + judge_agreement renders all three sections."""
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None, "gemini-3.1-pro"),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard(
        "biomnibench",
        rows,
        meta={
            "dimensions": {
                "ecaa": {"method_selection": 60.0},
                "claude-direct": {"method_selection": 50.0},
            },
            "published_best": "X=73.34",
            "judge_agreement": {"exact": 0.9, "kappa": 0.8},
        },
    )
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()

    # Per-dimension section present with expected content.
    assert "Per-dimension" in md
    assert "method_selection" in md
    # delta = 60.0 - 50.0 = +10.0
    assert "+10.0" in md
    # Published best line.
    assert "73.34" in md
    # Judge agreement line.
    assert "0.8" in md


def test_biomnibench_shaped_scorecard_renders_without_error(tmp_path):
    """BiomniBench-shaped card: multi-criterion dimensions, a partial-judging row
    with no judge_exact/judge_kappa, a row carrying incomplete_reason, and meta
    with dimension_note/dimension_source. Must render md + json without crashing."""
    rows = [
        Score("t1", "ecaa", 0, 80.0,
              {"method_selection": 60.0, "result_correctness": 75.0},
              None, None, "gemini-3.1-pro",
              extra={"judge_exact": 0.9, "judge_kappa": 0.8}),
        Score("t2", "ecaa", 0, 55.0,
              {"method_selection": 40.0, "result_correctness": 50.0},
              None, None, "gemini-3.1-pro",
              extra={"partial_judging": True}),  # no judge_exact / judge_kappa
        Score("t3", "claude-direct", 0, 65.0,
              {"method_selection": 50.0, "result_correctness": 55.0},
              None, None, "gemini-3.1-pro",
              extra={"incomplete_reason": "2/3 tasks completed; terminal missing"}),
    ]
    card = Scorecard(
        "biomnibench",
        rows,
        meta={
            "dimensions": {
                "ecaa": {"method_selection": 50.0, "result_correctness": 62.5},
                "claude-direct": {"method_selection": 50.0, "result_correctness": 55.0},
            },
            "dimension_source": "heuristic_title_match",
            "dimension_note": "Per-dimension means are a heuristic; only the overall score is benchmark-faithful.",
            "judge_agreement": {"exact": 0.9},  # kappa intentionally absent
        },
    )
    out = write_scorecard(card, tmp_path)

    md_path = out / "scorecard.md"
    json_path = out / "scorecard.json"
    assert md_path.exists() and json_path.exists()
    md = md_path.read_text()
    assert md.strip()  # non-empty

    data = json.loads(json_path.read_text())
    assert data["benchmark"] == "biomnibench"
    assert len(data["rows"]) == 3

    # Expected sections.
    assert "scorecard" in md
    assert "Per-dimension" in md
    assert "method_selection" in md and "result_correctness" in md
    # dimension_note is rendered.
    assert "heuristic" in md
    # Inter-judge agreement section present even though kappa is missing.
    assert "Inter-judge agreement" in md


def test_error_matrix_and_cost_partial_meta_renders(tmp_path):
    """Error-matrix / cost meta with missing optional keys must not KeyError."""
    rows = [
        Score("t1", "ecaa", 0, 90.0, {}, None, None, "deterministic"),
        Score("t1", "claude-direct", 0, 80.0, {}, None, None, "deterministic"),
    ]
    card = Scorecard(
        "nekrutenko",
        rows,
        meta={
            "error_matrix": {
                # Entry missing diagnose_rate and n_cells.
                "ecaa": {"recover_rate": 0.5},
            },
            "cost": {},  # judge_usd absent
        },
    )
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert md.strip()
    assert "Error matrix" in md


# ── eval-02: guard-outcome collector + per-arm dimension ─────────────────────

def _blocked_state(reason: str) -> dict:
    """The serialized shape of TaskState::Blocked
    (crates/core/src/dag.rs: tag="status", snake_case)."""
    return {"status": "blocked", "record": {"reason": reason, "attempts": []}}


def _completed_state() -> dict:
    return {"status": "completed", "result": {}}


def _write_pkg_with_guards(tmp_path) -> Path:
    pkg = tmp_path / "pkg"
    runtime = pkg / "runtime"
    runtime.mkdir(parents=True)
    # WORKFLOW.json: one missing-artifact re-block, one validation re-block,
    # one empty-result sentinel re-block, one clean completion.
    wf = {"tasks": {
        "data_acquisition": {"state": _blocked_state(
            "[missing_artifact] task=data_acquisition paths=de.csv — missing")},
        "reporting": {"state": _blocked_state(
            "[validation_failed] task=reporting p_value_in_unit_interval — failed")},
        "review_prior_work": {"state": _blocked_state(
            "Harness guard: agent marked review_prior_work completed with empty "
            "output (overall_*_not_run: true). Re-blocked.")},
        "final_reporting": {"state": _completed_state()},
    }}
    (pkg / "WORKFLOW.json").write_text(json.dumps(wf))
    # validation-reports.jsonl: 2 failed/errored rows + 1 passed (not counted).
    (runtime / "validation-reports.jsonl").write_text(
        json.dumps({"task_id": "reporting", "obligation_id": "p_value_in_unit_interval",
                    "outcome": "failed:p=1.4 out of [0,1]"}) + "\n"
        + json.dumps({"task_id": "de", "obligation_id": "gene_id_in_annotation",
                      "outcome": "errored:annotation file missing"}) + "\n"
        + json.dumps({"task_id": "qc", "obligation_id": "row_count_positive",
                      "outcome": "passed"}) + "\n"
    )
    # claim-verification.json: 2 mismatches at top level, 1 per-task.
    (runtime / "claim-verification.json").write_text(json.dumps(
        {"n_checked": 5, "n_verified": 2, "n_mismatch": 2, "n_unverifiable": 1,
         "verdicts": []}))
    per_task = runtime / "outputs" / "final_reporting"
    per_task.mkdir(parents=True)
    (per_task / "claim-verification.json").write_text(json.dumps(
        {"n_checked": 2, "n_verified": 1, "n_mismatch": 1, "n_unverifiable": 0,
         "verdicts": []}))
    return pkg


def test_collect_guard_outcomes_counts_all_three_guard_classes(tmp_path):
    pkg = _write_pkg_with_guards(tmp_path)
    go = collect_guard_outcomes(pkg)
    assert go["blocked_by_guard"] == 3
    assert set(go["blocked_tasks"]) == {
        "data_acquisition", "reporting", "review_prior_work"}
    assert go["blocked_by_kind"]["missing_artifact"] == 1
    assert go["blocked_by_kind"]["validation_failed"] == 1
    assert go["blocked_by_kind"]["empty_result_sentinel"] == 1
    assert go["validation_failures"] == 2          # failed: + errored:, not passed
    assert go["claim_mismatches"] == 3             # 2 top-level + 1 per-task
    assert go["corrections"] == 3 + 2


def test_collect_guard_outcomes_clean_package_is_all_zero(tmp_path):
    pkg = tmp_path / "pkg"
    (pkg / "runtime").mkdir(parents=True)
    (pkg / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "data_acquisition": {"state": _completed_state()},
        "final_reporting": {"state": _completed_state()},
    }}))
    go = collect_guard_outcomes(pkg)
    assert go == {
        "blocked_by_guard": 0, "blocked_by_kind": {}, "blocked_tasks": [],
        "validation_failures": 0, "claim_mismatches": 0, "corrections": 0,
    }


def test_collect_guard_outcomes_missing_package_does_not_raise(tmp_path):
    go = collect_guard_outcomes(tmp_path / "does-not-exist")
    assert go["blocked_by_guard"] == 0
    assert go["corrections"] == 0


def test_collect_guard_outcomes_non_guard_block_not_counted(tmp_path):
    """A task blocked for a non-guard reason (e.g. an unresolved input blocker)
    must NOT be counted as a guard catch."""
    pkg = tmp_path / "pkg"
    (pkg / "runtime").mkdir(parents=True)
    (pkg / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "data_acquisition": {"state": _blocked_state(
            "waiting on the SME to supply a reference genome path")},
    }}))
    go = collect_guard_outcomes(pkg)
    assert go["blocked_by_guard"] == 0


def test_guard_outcomes_render_in_scorecard(tmp_path):
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro",
              extra={"guard_outcomes": {"blocked_by_guard": 2,
                                        "validation_failures": 1,
                                        "claim_mismatches": 3, "corrections": 3}}),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard("biomnibench", rows)
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "Guard outcomes" in md
    assert "blocked-by-guard" in md
    data = json.loads((out / "scorecard.json").read_text())
    go = data["meta"]["guard_outcomes"]["ecaa"]
    assert go["blocked_by_guard"] == 2
    assert go["claim_mismatches"] == 3


def test_aggregate_guard_outcomes_empty_when_no_rows_carry_evidence():
    card = Scorecard("biomnibench", [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "j"),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "j"),
    ])
    assert _aggregate_guard_outcomes(card) == {}


# ── eval-04: paired delta + bootstrap CI ─────────────────────────────────────

def test_paired_delta_pairs_on_task_and_trial():
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "j"),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "j"),
        Score("t2", "ecaa", 0, 60.0, {}, None, None, "j"),
        Score("t2", "claude-direct", 0, 55.0, {}, None, None, "j"),
        # unpaired: ecaa-only trial 1 must NOT enter the paired set
        Score("t1", "ecaa", 1, 90.0, {}, None, None, "j"),
    ]
    summary = paired_delta_summary(Scorecard("b", rows))
    assert summary["n_pairs"] == 2
    assert abs(summary["mean_paired_delta"] - 7.5) < 1e-9  # (10 + 5) / 2


def test_paired_delta_none_when_single_arm():
    rows = [Score("t1", "ecaa", 0, 80.0, {}, None, None, "j")]
    assert paired_delta_summary(Scorecard("b", rows)) is None


def test_paired_delta_clear_separation_is_significant():
    # ecaa consistently +20 over direct across many trials -> CI excludes 0.
    rows = []
    for tr in range(8):
        rows.append(Score("t1", "ecaa", tr, 80.0, {}, None, None, "j"))
        rows.append(Score("t1", "claude-direct", tr, 60.0, {}, None, None, "j"))
    summary = paired_delta_summary(Scorecard("b", rows))
    assert summary["significant"] is True
    assert summary["ci_lower"] > 0.0
    assert abs(summary["mean_paired_delta"] - 20.0) < 1e-9


def test_paired_delta_overlapping_arms_not_significant():
    # Deltas straddle zero -> CI crosses 0 -> not significant.
    deltas = [5.0, -4.0, 3.0, -6.0, 2.0, -1.0]
    rows = []
    for tr, d in enumerate(deltas):
        rows.append(Score("t1", "ecaa", tr, 50.0 + d, {}, None, None, "j"))
        rows.append(Score("t1", "claude-direct", tr, 50.0, {}, None, None, "j"))
    summary = paired_delta_summary(Scorecard("b", rows))
    assert summary["significant"] is False
    assert summary["ci_lower"] < 0.0 < summary["ci_upper"]


def test_paired_delta_renders_n_and_ci_and_significance_note(tmp_path):
    deltas = [5.0, -4.0, 3.0, -6.0]
    rows = []
    for tr, d in enumerate(deltas):
        rows.append(Score("t1", "ecaa", tr, 50.0 + d, {}, None, None, "j"))
        rows.append(Score("t1", "claude-direct", tr, 50.0, {}, None, None, "j"))
    card = Scorecard("biomnibench", rows)
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "Paired delta" in md
    assert "n (paired task/trial):** 4" in md
    assert "bootstrap CI" in md
    assert "NOT significant at n=4 (CI crosses 0)" in md
    data = json.loads((out / "scorecard.json").read_text())
    pd = data["meta"]["paired_delta"]
    assert pd["n_pairs"] == 4
    assert "ci_lower" in pd and "ci_upper" in pd


def test_bootstrap_ci_is_deterministic():
    deltas = [10.0, 12.0, 8.0, 9.0, 11.0, 7.0]
    a = _bootstrap_ci(deltas)
    b = _bootstrap_ci(deltas)
    assert a == b  # fixed seed -> reproducible


def test_bootstrap_ci_degenerate_inputs():
    assert _bootstrap_ci([]) == (0.0, 0.0)
    assert _bootstrap_ci([4.2]) == (4.2, 4.2)


def test_session_metrics_section_renders_for_ecaa_only(tmp_path):
    from scripts.eval.services.scorecard import (
        _aggregate_session_metrics, write_scorecard)
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro",
              extra={"session_metrics": {"followup_count": 2, "time_to_emit_ms": 4000,
                                         "method_recommendation_requests": 1,
                                         "is_ambiguous": False}}),
        Score("t2", "ecaa", 0, 82.0, {}, None, None, "gemini-3.1-pro",
              extra={"session_metrics": {"followup_count": 4, "time_to_emit_ms": 6000,
                                         "method_recommendation_requests": 0,
                                         "is_ambiguous": True}}),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard("biomnibench", rows)
    per_arm = _aggregate_session_metrics(card)
    assert "ecaa" in per_arm
    assert "claude-direct" not in per_arm  # bare arm has no session metrics
    assert per_arm["ecaa"]["median_followup_count"] == 3.0
    assert per_arm["ecaa"]["median_time_to_emit_ms"] == 5000.0
    assert per_arm["ecaa"]["n_sessions"] == 2
    assert per_arm["ecaa"]["method_recommendation_requests_total"] == 1
    out = write_scorecard(card, tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "## Session metrics (SME friction, harvested from /metrics)" in md
    data = json.loads((out / "scorecard.json").read_text())
    assert data["meta"]["session_metrics"]["ecaa"]["n_sessions"] == 2


def test_session_metrics_section_absent_when_no_metrics(tmp_path):
    from scripts.eval.services.scorecard import write_scorecard
    rows = [Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro"),
            Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro")]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "## Session metrics" not in md


def test_public_scorecard_stamps_provenance_and_redacts_cost(tmp_path):
    from scripts.eval.services.scorecard import write_public_scorecard
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro",
              extra={"session_metrics": {"followup_count": 2, "time_to_emit_ms": 4000}}),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro"),
    ]
    card = Scorecard("biomnibench", rows,
                     meta={"cost": {"judge_usd": 1.23, "total_cost_usd": 99.0},
                           "total_cost_usd": 99.0, "wall_secs": 4242.0})
    out = write_public_scorecard(card, tmp_path, git_head="abc1234",
                                 datasets_lock="nek=rev1;bbench=rev2",
                                 seed=1729, arms=["ecaa", "claude-direct"], trials=3)
    pub = json.loads((out / "scorecard.public.json").read_text())
    # Provenance present.
    assert pub["provenance"]["git_head"] == "abc1234"
    assert pub["provenance"]["datasets_lock"] == "nek=rev1;bbench=rev2"
    assert pub["provenance"]["seed"] == 1729
    assert pub["provenance"]["arms"] == ["ecaa", "claude-direct"]
    assert pub["provenance"]["trials"] == 3
    # Cost + wall-clock redacted everywhere.
    blob = json.dumps(pub)
    assert "total_cost_usd" not in blob
    assert "wall_secs" not in blob
    assert "99.0" not in blob
    # Substantive sections retained.
    assert pub["meta"]["session_metrics"]["ecaa"]["n_sessions"] == 1
    assert (out / "scorecard.public.md").exists()
    md = (out / "scorecard.public.md").read_text()
    assert "git_head: abc1234" in md
    assert "## Session metrics" in md


def test_three_arm_card_renders_all_arms(tmp_path):
    from scripts.eval.services.scorecard import write_scorecard
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro"),
        Score("t1", "ecaa-ungated", 0, 75.0, {}, None, None, "gemini-3.1-pro"),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro"),
    ]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text()
    for arm in ("ecaa", "ecaa-ungated", "claude-direct"):
        assert arm in md
    # The arm table has three data rows (plus header + separator).
    data = json.loads((out / "scorecard.json").read_text())
    assert len({r["arm"] for r in data["rows"]}) == 3


# ── RCA #3: source-penalty-stripped companion section ────────────────────────

def test_penalty_stripped_section_renders_and_headline_unchanged(tmp_path):
    """The penalty-stripped section renders with both columns; the headline
    arm-means table is unchanged; meta carries no_penalty_mean >= penalized_mean.

    ecaa pays a B-level source penalty (headline 95, stripped 100); claude-direct
    pays a C-level penalty (headline 80, stripped 90)."""
    from scripts.eval.services.scorecard import write_scorecard
    rows = [
        Score("t1", "ecaa", 0, 95.0, {}, None, None, "gemini-3.1-pro",
              extra={"overall_no_source_penalty": 100.0}),
        Score("t1", "claude-direct", 0, 80.0, {}, None, None, "gemini-3.1-pro",
              extra={"overall_no_source_penalty": 90.0}),
    ]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text()
    # New section present.
    assert "Source-penalty-stripped score" in md
    assert "apples-to-apples vs published 73.34" in md
    assert "penalty paid" in md
    # The note states the headline is unchanged + 73.34 is a loose reference.
    assert "UNCHANGED" in md
    assert "loose reference" in md
    # Headline arm-means table is unchanged: the penalized means still show.
    assert "| ecaa | 1 | 95.0 |" in md
    assert "| claude-direct | 1 | 80.0 |" in md

    data = json.loads((out / "scorecard.json").read_text())
    sps = data["meta"]["source_penalty_stripped"]
    assert sps["ecaa"]["penalized_mean"] == 95.0
    assert sps["ecaa"]["no_penalty_mean"] == 100.0
    assert sps["ecaa"]["no_penalty_mean"] >= sps["ecaa"]["penalized_mean"]
    assert sps["ecaa"]["penalty_paid"] == 5.0
    assert sps["claude-direct"]["penalty_paid"] == 10.0
    # The headline rows' `overall` is untouched.
    overalls = {r["arm"]: r["overall"] for r in data["rows"]}
    assert overalls["ecaa"] == 95.0
    assert overalls["claude-direct"] == 80.0
    # The rich key is not also printed as a stray scalar bullet.
    assert "- **source_penalty_stripped:**" not in md


def test_penalty_stripped_fallback_when_key_absent(tmp_path):
    """Rows WITHOUT overall_no_source_penalty (older cards / fraction mode) still
    render the section with penalty_paid 0 — no crash, the stripped mean falls
    back to the penalized overall."""
    from scripts.eval.services.scorecard import write_scorecard
    rows = [
        Score("t1", "ecaa", 0, 80.0, {}, None, None, "gemini-3.1-pro"),
        Score("t1", "claude-direct", 0, 70.0, {}, None, None, "gemini-3.1-pro"),
    ]
    out = write_scorecard(Scorecard("biomnibench", rows), tmp_path)
    md = (out / "scorecard.md").read_text()
    assert "Source-penalty-stripped score" in md
    data = json.loads((out / "scorecard.json").read_text())
    sps = data["meta"]["source_penalty_stripped"]
    # Fallback: stripped == penalized, penalty paid 0.
    assert sps["ecaa"]["penalized_mean"] == 80.0
    assert sps["ecaa"]["no_penalty_mean"] == 80.0
    assert sps["ecaa"]["penalty_paid"] == 0.0
    assert sps["claude-direct"]["penalty_paid"] == 0.0
