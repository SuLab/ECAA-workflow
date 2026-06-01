# scripts/eval/tests/test_flatten.py
import json
from pathlib import Path
from scripts.eval.scoring.flatten import flatten_outputs, completion_status, _narrative


def _pkg(tmp_path):
    wf = {"tasks": [
        {"id": "load", "stage": "data_acquisition", "depends_on": []},
        {"id": "de", "stage": "differential_expression", "depends_on": ["load"]},
        {"id": "report", "stage": "final_reporting", "depends_on": ["de"]},
    ]}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid, txt in [("load", "loaded 4 samples"), ("de", "2018 sig genes"),
                     ("report", "Treatment reduces recovery time.")]:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\n{txt}\n")
    return tmp_path


def test_flatten_orders_and_picks_terminal(tmp_path):
    pkg = _pkg(tmp_path)
    trace, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert trace.index("load") < trace.index("de") < trace.index("report")
    assert "Treatment reduces recovery time." in answer


# --- completion_status: incomplete-run detection ---

def _partial_pkg(tmp_path, populated_ids):
    """A 3-task workflow (load -> de -> report) whose runtime/outputs only
    contains narratives for ``populated_ids``."""
    wf = {"tasks": [
        {"id": "load", "stage": "data_acquisition", "depends_on": []},
        {"id": "de", "stage": "differential_expression", "depends_on": ["load"]},
        {"id": "report", "stage": "final_reporting", "depends_on": ["de"]},
    ]}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    out.mkdir(parents=True)
    for tid in populated_ids:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\noutput for {tid}\n")
    return tmp_path


def test_completion_status_full(tmp_path):
    """A fully-populated package reports total == with_output and a live terminal."""
    pkg = _pkg(tmp_path)
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status == {"total": 3, "with_output": 3, "terminal_has_output": True}


def test_completion_status_terminal_missing(tmp_path):
    """Only the first task ran: shortfall reported and terminal has no output."""
    pkg = _partial_pkg(tmp_path, populated_ids=["load"])
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["total"] == 3
    assert status["with_output"] == 1
    assert status["terminal_has_output"] is False


def test_completion_status_empty_outputs_dir(tmp_path):
    """No task produced output at all (the stalled-at-2/27 shape)."""
    pkg = _partial_pkg(tmp_path, populated_ids=[])
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["total"] == 3
    assert status["with_output"] == 0
    assert status["terminal_has_output"] is False


def test_completion_status_empty_narrative_not_counted(tmp_path):
    """A task dir that exists but holds only whitespace is not counted as output."""
    pkg = _partial_pkg(tmp_path, populated_ids=["load", "de"])
    blank = pkg / "runtime" / "outputs" / "report"
    blank.mkdir(parents=True)
    (blank / "report.md").write_text("   \n\t\n")
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["with_output"] == 2
    assert status["terminal_has_output"] is False


def test_completion_status_missing_workflow_json_does_not_raise(tmp_path):
    """A missing/unreadable WORKFLOW.json yields an all-zero status, not an error."""
    status = completion_status(tmp_path / "runtime" / "outputs",
                               tmp_path / "WORKFLOW.json")
    assert status == {"total": 0, "with_output": 0, "terminal_has_output": False}


# --- _narrative unit tests ---

def test_narrative_result_json_narrative_field(tmp_path):
    """result.json with a `narrative` field is used as narrative text."""
    d = tmp_path / "task1"
    d.mkdir()
    (d / "result.json").write_text(json.dumps({
        "status": "completed",
        "narrative": "Identified 2018 differentially expressed genes at FDR<0.05.",
    }))
    text = _narrative(d)
    assert "2018 differentially expressed genes" in text


def test_narrative_result_json_no_known_field_falls_back_to_json_dump(tmp_path):
    """result.json with no recognised narrative key falls back to json.dumps."""
    d = tmp_path / "task2"
    d.mkdir()
    data = {"status": "completed", "metrics": {"n_sig": 42}}
    (d / "result.json").write_text(json.dumps(data))
    text = _narrative(d)
    # The full JSON dump is returned; at minimum the key should appear.
    assert "n_sig" in text
    assert "42" in text


def test_narrative_progress_log_fallback(tmp_path):
    """When no result.json or .md files exist, progress.log is returned."""
    d = tmp_path / "task3"
    d.mkdir()
    (d / "progress.log").write_text("Step 1 done\nStep 2 done\n")
    text = _narrative(d)
    assert "Step 1 done" in text
    assert "Step 2 done" in text


def test_narrative_real_agent_result_json_shape(tmp_path):
    """result.json with the full shape written by AGENT-EXECUTOR.md is handled correctly.

    The AGENT-EXECUTOR.md template (crates/core/templates/AGENT-EXECUTOR.md, line 52-57)
    instructs the agent to write result.json with: task_id, status, claims (list with
    evidence paths), figures (list of paths), and `narrative` (human-readable summary).
    The `narrative` key must be extracted as the task narrative; structured fields like
    `claims`, `figures`, and `status` must not be mistaken for it.
    """
    d = tmp_path / "differential_expression"
    d.mkdir()
    (d / "result.json").write_text(json.dumps({
        "task_id": "differential_expression",
        "status": "completed",
        "claims": [
            {
                "claim_id": "c-001",
                "narrative_text": "2018 genes are differentially expressed at FDR<0.05.",
                "supported_by": ["differential_expression/de_results.csv"],
            }
        ],
        "figures": ["differential_expression/figures/volcano.png"],
        "narrative": (
            "DESeq2 analysis identified 2018 differentially expressed genes "
            "between treatment and control at FDR < 0.05 (padj threshold)."
        ),
    }))
    text = _narrative(d)
    assert "DESeq2 analysis identified 2018 differentially expressed genes" in text
    # structured fields must not surface as the narrative
    assert "claim_id" not in text
    assert "supported_by" not in text
