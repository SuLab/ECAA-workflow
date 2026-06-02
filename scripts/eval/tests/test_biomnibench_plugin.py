# scripts/eval/tests/test_biomnibench_plugin.py
import json
from pathlib import Path

from scripts.eval.benchmark import Arm, Output, RunSpec, Score, Task
from scripts.eval.plugins.biomnibench import BiomniBench

# Shared fixtures for batch-scoring tests.
_RUBRIC = {
    "criteria": [
        {"id": "c1", "dimension": "method_selection", "points": 4,
         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}},
        {"id": "c2", "dimension": "source_reliability", "points": 2,
         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}},
    ]
}

def _make_task(task_id="t1"):
    return Task(task_id=task_id, prompt="do analysis", inputs={},
                rubric=_RUBRIC, answer_key=None)

def _make_output():
    return Output(trace_md="trace text", answer_txt="answer text",
                  artifacts={}, exit_ok=True, wall_secs=1.0)


def test_report_groups_dimension_means():
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0, "source_reliability": 90.0},
              None, None, "gemini-3.1-pro",
              extra={"judge_exact": 0.9, "judge_kappa": 0.8}),
        Score("t1", "claude-direct", 0, 70.0, {"method_selection": 50.0, "source_reliability": 88.0},
              None, None, "gemini-3.1-pro",
              extra={"judge_exact": 0.7, "judge_kappa": 0.6}),
    ]
    card = BiomniBench().report(rows)
    assert card.benchmark == "biomnibench"
    assert card.meta["dimensions"]  # dimension means present
    assert "method_selection" in card.meta["dimensions"]["ecaa"]

    # judge_agreement rollup
    ja = card.meta["judge_agreement"]
    assert "exact" in ja and "kappa" in ja
    # exact mean = (0.9 + 0.7) / 2 = 0.8; kappa mean = (0.8 + 0.6) / 2 = 0.7
    assert abs(ja["exact"] - 0.8) < 1e-9
    assert abs(ja["kappa"] - 0.7) < 1e-9


def test_report_judge_agreement_zero_rows():
    """When no scores carry judge_exact/kappa, rollup should be 0.0 / 0.0."""
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0}, None, None, "gemini-3.1-pro"),
    ]
    card = BiomniBench().report(rows)
    ja = card.meta["judge_agreement"]
    assert ja == {"exact": 0.0, "kappa": 0.0}


def test_report_excludes_partial_judging_rows_from_dimension_means():
    """Opus-only fallback (partial_judging) rows are dropped from the
    Gemini-headline dimension means. A complete row (60.0) and a partial row
    (0.0) for the same arm/dimension must yield the mean 60.0 (partial excluded),
    NOT 30.0 (the regression where both are averaged)."""
    rows = [
        Score("t1", "ecaa", 0, 80.0, {"method_selection": 60.0},
              None, None, "gemini-3.1-pro",
              extra={"judge_exact": 0.9, "judge_kappa": 0.8}),
        Score("t2", "ecaa", 0, 0.0, {"method_selection": 0.0},
              None, None, "anthropic-opus",
              extra={"partial_judging": True}),
    ]
    card = BiomniBench().report(rows)
    assert card.meta["dimensions"]["ecaa"]["method_selection"] == 60.0


def test_collect_bare_reads_agent_stdout_json(tmp_path):
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "agent-stdout.json").write_text(json.dumps({"result": "FINAL ANSWER"}))

    spec = RunSpec(arm=Arm.CLAUDE_CODE_DIRECT, workdir=run_dir, kind="bare",
                   instruction="some prompt")
    output = BiomniBench().collect(spec, run_dir)

    assert output.answer_txt == "FINAL ANSWER"
    assert output.trace_md  # non-empty (the raw file text)


def test_collect_bare_reads_agent_stdout_json_text_fallback(tmp_path):
    """Falls back to 'text' key when 'result' is absent."""
    run_dir = tmp_path / "run"
    run_dir.mkdir()
    (run_dir / "agent-stdout.json").write_text(json.dumps({"text": "TEXT ANSWER"}))

    spec = RunSpec(arm=Arm.CLAUDE_CODE_DIRECT, workdir=run_dir, kind="bare",
                   instruction="some prompt")
    output = BiomniBench().collect(spec, run_dir)

    assert output.answer_txt == "TEXT ANSWER"


def test_collect_bare_empty_when_nothing_present(tmp_path):
    """No trace.md, answer.txt, or agent-stdout.json -> empty strings."""
    run_dir = tmp_path / "run"
    run_dir.mkdir()

    spec = RunSpec(arm=Arm.CLAUDE_CODE_DIRECT, workdir=run_dir, kind="bare",
                   instruction="some prompt")
    output = BiomniBench().collect(spec, run_dir)

    assert output.trace_md == ""
    assert output.answer_txt == ""


# ---------------------------------------------------------------------------
# ECAA-arm incomplete-run detection
# ---------------------------------------------------------------------------

def _ecaa_run(run_dir, populated_ids):
    """Build an ecaa-package run dir: 3-task workflow with only some outputs."""
    wf = {"tasks": [
        {"id": "load", "stage": "data_acquisition", "depends_on": []},
        {"id": "de", "stage": "differential_expression", "depends_on": ["load"]},
        {"id": "report", "stage": "final_reporting", "depends_on": ["de"]},
    ]}
    run_dir.mkdir(parents=True, exist_ok=True)
    (run_dir / "WORKFLOW.json").write_text(json.dumps(wf))
    out = run_dir / "runtime" / "outputs"
    out.mkdir(parents=True)
    for tid in populated_ids:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\noutput for {tid}\n")
    return RunSpec(arm=Arm.ECAA_WORKFLOW, workdir=run_dir, kind="ecaa_package",
                   instruction="some prompt")


def test_collect_ecaa_complete_has_no_incomplete_flag(tmp_path):
    """A fully-executed workflow carries no incomplete_reason."""
    run_dir = tmp_path / "run"
    spec = _ecaa_run(run_dir, populated_ids=["load", "de", "report"])
    output = BiomniBench().collect(spec, run_dir)

    assert "incomplete_reason" not in output.artifacts
    assert "output for report" in output.answer_txt


def test_collect_ecaa_terminal_missing_sets_incomplete_reason(tmp_path):
    """A workflow stalled before its terminal task flags incompleteness."""
    run_dir = tmp_path / "run"
    spec = _ecaa_run(run_dir, populated_ids=["load"])
    output = BiomniBench().collect(spec, run_dir)

    assert output.artifacts.get("incomplete_reason")
    reason = output.artifacts["incomplete_reason"]
    assert "terminal task produced no output" in reason
    assert "1/3 tasks completed" in reason


def test_collect_ecaa_partial_with_terminal_sets_incomplete_reason(tmp_path):
    """Terminal ran but an upstream task is missing -> still flagged."""
    run_dir = tmp_path / "run"
    spec = _ecaa_run(run_dir, populated_ids=["load", "report"])
    output = BiomniBench().collect(spec, run_dir)

    assert output.artifacts.get("incomplete_reason")
    assert "2/3 tasks completed" in output.artifacts["incomplete_reason"]


# ---------------------------------------------------------------------------
# Batch scoring: judge_requests + assemble_score
# ---------------------------------------------------------------------------

def test_judge_requests_returns_two_reqs():
    """judge_requests returns exactly 2 dicts with the right roles and judge_ids."""
    plugin = BiomniBench()
    task = _make_task()
    output = _make_output()
    reqs = plugin.judge_requests(task, Arm.ECAA_WORKFLOW, output)

    assert len(reqs) == 2
    roles = {r["role"] for r in reqs}
    assert roles == {"headline", "cross"}

    by_role = {r["role"]: r for r in reqs}
    assert by_role["headline"]["judge_id"] == "gemini-3.1-pro"
    assert by_role["cross"]["judge_id"] == "anthropic-opus"


def test_judge_requests_embeds_task_rubric_and_output():
    """judge_requests embeds task.rubric, output.trace_md, output.answer_txt."""
    plugin = BiomniBench()
    task = _make_task()
    output = _make_output()
    reqs = plugin.judge_requests(task, Arm.ECAA_WORKFLOW, output)

    for req in reqs:
        assert req["rubric"] is task.rubric
        assert req["trace"] == output.trace_md
        assert req["answer"] == output.answer_txt


def test_assemble_score_builds_correct_score():
    """assemble_score builds a Score with dimensions, overall, and extra fields."""
    plugin = BiomniBench()
    task = _make_task()
    output = _make_output()

    headline_verdict = {
        "overall": 75.0,
        "dimensions": {"method_selection": 80.0, "source_reliability": 60.0},
        "levels": {"c1": "A", "c2": "B"},
        "cost_usd": 0.02,
    }
    cross_verdict = {
        "overall": 70.0,
        "dimensions": {"method_selection": 75.0, "source_reliability": 55.0},
        "levels": {"c1": "A", "c2": "B"},
        "cost_usd": 0.10,
    }
    verdicts = {"headline": headline_verdict, "cross": cross_verdict}

    score = plugin.assemble_score(task, Arm.ECAA_WORKFLOW, output, trial=0, verdicts=verdicts)

    assert isinstance(score, Score)
    assert score.task_id == "t1"
    assert score.arm == Arm.ECAA_WORKFLOW.value
    assert score.trial == 0
    assert score.overall == 75.0
    assert score.dimensions == headline_verdict["dimensions"]
    assert score.judge_id == "gemini-3.1-pro"
    assert score.jaccard is None
    assert score.error_cells is None

    extra = score.extra
    assert abs(extra["cross_check"] - 70.0) < 1e-9
    assert abs(extra["judge_cost_usd"] - 0.12) < 1e-9
    # judge_exact and judge_kappa must be present (numeric).
    assert "judge_exact" in extra
    assert "judge_kappa" in extra
    assert isinstance(extra["judge_exact"], float)
    assert isinstance(extra["judge_kappa"], float)


def test_assemble_score_exact_agreement_when_identical_levels():
    """When headline and cross levels are identical, exact agreement should be 1.0."""
    plugin = BiomniBench()
    task = _make_task()
    output = _make_output()

    levels = {"c1": "A", "c2": "A"}
    verdict = {"overall": 100.0, "dimensions": {}, "levels": levels, "cost_usd": 0.0}
    score = plugin.assemble_score(task, Arm.ECAA_WORKFLOW, output, 0,
                                  {"headline": verdict, "cross": verdict})

    assert score.extra["judge_exact"] == 1.0
