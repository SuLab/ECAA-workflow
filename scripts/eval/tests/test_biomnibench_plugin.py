# scripts/eval/tests/test_biomnibench_plugin.py
import json
from pathlib import Path

from scripts.eval.benchmark import Arm, RunSpec, Score
from scripts.eval.plugins.biomnibench import BiomniBench


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
