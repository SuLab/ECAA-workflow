"""Offline coverage for folding ECAA per-task result.json into the cell exec_log
(Issue 2b).

The ECAA arm reports per-task failures into structured
``runtime/outputs/<task_id>/result.json`` narratives, NOT the harness top-level
stdout that classify_cell's diagnose scan reads. The bare arm prints the same
failures straight to stdout. run_error_cell merges the per-task narratives into
the exec_log so both arms hand classify_cell identical diagnose vocabulary, and
classify_cell itself stays unchanged. The bare arm has no runtime/outputs/ tree,
so the merge is a no-op there — the arm-fairness contract holds.
"""
import json
from pathlib import Path

from scripts.eval.plugins.nekrutenko import Nekrutenko, _collect_result_summaries


def test_collect_result_summaries_reads_narrative_keys(tmp_path):
    """Every per-task result.json narrative (summary / narrative / report / ...)
    under runtime/outputs/<tid>/ is concatenated into one blob."""
    out = tmp_path / "pkg" / "runtime" / "outputs"
    (out / "call_M117C1-ch").mkdir(parents=True)
    (out / "call_M117C1-ch" / "result.json").write_text(json.dumps(
        {"summary": "lofreq failed on sample M117C1-ch (truncated input)"}))
    (out / "align").mkdir(parents=True)
    (out / "align" / "result.json").write_text(json.dumps(
        {"narrative": "aligned 4/4 samples"}))

    merged = _collect_result_summaries(tmp_path / "pkg")
    assert "lofreq failed on sample M117C1-ch" in merged
    assert "4/4 samples" in merged


def test_collect_result_summaries_empty_when_no_outputs(tmp_path):
    assert _collect_result_summaries(tmp_path / "missing") == ""


def test_collect_result_summaries_skips_corrupt_and_non_dict(tmp_path):
    """A corrupt or non-dict result.json contributes nothing; valid siblings are
    still collected. Best-effort, never raises."""
    out = tmp_path / "pkg" / "runtime" / "outputs"
    (out / "bad").mkdir(parents=True)
    (out / "bad" / "result.json").write_text("{not valid json")
    (out / "list").mkdir(parents=True)
    (out / "list" / "result.json").write_text(json.dumps(["a", "b"]))
    (out / "good").mkdir(parents=True)
    (out / "good" / "result.json").write_text(json.dumps(
        {"report": "missing library error detected, skipped all samples"}))

    merged = _collect_result_summaries(tmp_path / "pkg")
    assert merged == "missing library error detected, skipped all samples"


def test_collect_result_summaries_ignores_non_string_and_blank_values(tmp_path):
    """Non-string narrative values and whitespace-only strings are skipped."""
    out = tmp_path / "pkg" / "runtime" / "outputs"
    (out / "t").mkdir(parents=True)
    (out / "t" / "result.json").write_text(json.dumps(
        {"summary": "   ", "narrative": 42, "report": "real prose here"}))
    assert _collect_result_summaries(tmp_path / "pkg") == "real prose here"


def test_run_error_cell_folds_summaries_into_exec_log(tmp_path, monkeypatch):
    """The ECAA-arm cell's classify_cell input exec_log must include both the
    harness top-level stdout AND the merged per-task summaries."""
    captured = {}

    def _fake_classify(**kw):
        captured.update(kw)
        return {"handle": "partial", "recover": True, "diagnose": True,
                "diagnose_signals": ["failures_log_populated"]}

    monkeypatch.setattr("scripts.eval.plugins.nekrutenko.classify_cell",
                        _fake_classify)

    plugin = Nekrutenko()
    task = type("T", (), {})()

    def _run_fn(cell_dir, env):
        # Simulate an ECAA harness run: write a per-task result.json summary +
        # the shim invocation marker, and return a captured-stdout result.
        cell_dir = Path(cell_dir)
        out = cell_dir / "runtime" / "outputs" / "call_M117C1-ch"
        out.mkdir(parents=True)
        out.joinpath("result.json").write_text(json.dumps(
            {"summary": "ERROR sample M117C1-ch aborted"}))
        Path(env["EVAL_INJECT_STATE"]).joinpath("invoked.lofreq").write_text("x")
        return type("R", (), {"exit_ok": True, "stdout": "harness top-level log"})()

    # run_error_cell makes its own tempdir; patch scratch_root to tmp_path so the
    # cell_dir lands under our control and _present_sample_count sees no VCFs.
    monkeypatch.setattr("scripts.eval.plugins.nekrutenko.scratch_root",
                        lambda: tmp_path)
    plugin.run_error_cell(task, ("missing_lib_error", "lofreq", 42), _run_fn)

    assert "harness top-level log" in captured["exec_log"]
    assert "ERROR sample M117C1-ch aborted" in captured["exec_log"], (
        "per-task result.json summary must be folded into the classify exec_log")


def test_run_error_cell_no_summaries_leaves_exec_log_as_stdout(tmp_path, monkeypatch):
    """With no runtime/outputs/ tree (the bare-arm shape), exec_log is exactly the
    harness stdout — the merge is a no-op, preserving arm fairness."""
    captured = {}

    def _fake_classify(**kw):
        captured.update(kw)
        return {"handle": "crash", "recover": False, "diagnose": False,
                "diagnose_signals": []}

    monkeypatch.setattr("scripts.eval.plugins.nekrutenko.classify_cell",
                        _fake_classify)
    monkeypatch.setattr("scripts.eval.plugins.nekrutenko.scratch_root",
                        lambda: tmp_path)

    plugin = Nekrutenko()
    task = type("T", (), {})()

    def _run_fn(cell_dir, env):
        Path(env["EVAL_INJECT_STATE"]).joinpath("invoked.bwa").write_text("x")
        return type("R", (), {"exit_ok": False, "stdout": "only stdout here"})()

    plugin.run_error_cell(task, ("flake_first_call", "bwa", 42), _run_fn)
    assert captured["exec_log"] == "only stdout here"
