# scripts/eval/tests/test_flatten.py
import json
from pathlib import Path
import pytest
from scripts.eval.scoring.flatten import (
    flatten_outputs,
    completion_status,
    _narrative,
    _normalize_tasks,
    _topo,
    _terminal_id,
    _stage_of,
    _executed_code_block,
)

REPO_ROOT = Path(__file__).resolve().parents[3]
EMITTED_PACKAGES = REPO_ROOT / "testdata" / "emitted-packages"


# --- WORKFLOW.json shape: object-keyed tasks (the real emitter shape) ---
#
# The Rust emitter serialises ``tasks`` as a JSON OBJECT keyed by task id
# (``BTreeMap<TaskId, Task>``); per-task objects have NO ``id``/``stage`` field,
# only kind/state/depends_on/assignee/description. The task id (map key) IS the
# stage name. These fixtures mirror that shape.


def _task(depends_on, *, description="desc"):
    """A per-task object in the emitter's object-keyed shape (no id/stage)."""
    return {
        "assignee": "agent",
        "depends_on": list(depends_on),
        "description": description,
        "kind": "computation",
        "resource_class": "cpu_heavy",
        "state": {"status": "pending"},
    }


def _pkg(tmp_path):
    """3-task object-keyed workflow: data_acquisition -> de -> final_reporting."""
    wf = {"tasks": {
        "data_acquisition": _task([]),
        "differential_expression": _task(["data_acquisition"]),
        "final_reporting": _task(["differential_expression"]),
    }}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid, txt in [("data_acquisition", "loaded 4 samples"),
                     ("differential_expression", "2018 sig genes"),
                     ("final_reporting", "Treatment reduces recovery time.")]:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\n{txt}\n")
    return tmp_path


def test_flatten_does_not_crash_on_object_keyed_tasks(tmp_path):
    """Regression for eval-01: the previous list-shaped reader raised
    `TypeError: string indices must be integers` on the real object shape."""
    pkg = _pkg(tmp_path)
    trace, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert trace  # non-empty, no exception
    assert answer


def test_flatten_falls_back_to_richest_narrative_when_terminal_empty(tmp_path):
    """da-5-1 regression: a complete cross-omics-style DAG with NO reporting
    terminal (ends at an analytical node + its validate_* companion) must NOT
    yield an empty answer. The substance lives in the analytical tail's output;
    the fallback surfaces it so a correctly-computed analysis is never scored 0
    purely because the answer channel was empty."""
    wf = {"tasks": {
        "data_acquisition": _task([]),
        "druggable_target_prioritization": _task(["data_acquisition"]),
        "validate_druggable_target_prioritization": _task(
            ["druggable_target_prioritization"]),
    }}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    (out / "data_acquisition").mkdir(parents=True)
    (out / "data_acquisition" / "report.md").write_text("# data_acquisition\nloaded CPTAC\n")
    (out / "druggable_target_prioritization").mkdir(parents=True)
    (out / "druggable_target_prioritization" / "result.json").write_text(
        json.dumps({"narrative": "353 dual-evidence targets; 13 Tier-1 including KRAS"}))
    (out / "validate_druggable_target_prioritization").mkdir(parents=True)  # empty

    _, answer = flatten_outputs(out, tmp_path / "WORKFLOW.json")
    assert answer.strip(), "a complete trace must never yield an empty answer"
    assert "353 dual-evidence targets" in answer
    assert "KRAS" in answer


def test_flatten_fallback_does_not_fire_when_terminal_has_content(tmp_path):
    """Regression guard: the empty-answer fallback must NOT override a terminal
    that produced a real narrative (the normal path)."""
    pkg = _pkg(tmp_path)
    _, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert "Treatment reduces recovery time." in answer  # the final_reporting terminal


def test_flatten_orders_and_picks_terminal(tmp_path):
    pkg = _pkg(tmp_path)
    trace, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert (trace.index("data_acquisition")
            < trace.index("differential_expression")
            < trace.index("final_reporting"))
    assert "Treatment reduces recovery time." in answer


def test_normalize_tasks_object_shape_uses_key_as_id():
    tasks = _normalize_tasks({
        "alignment": _task(["data_acquisition"]),
        "data_acquisition": _task([]),
    })
    assert set(tasks) == {"alignment", "data_acquisition"}
    assert tasks["alignment"]["depends_on"] == ["data_acquisition"]


def test_stage_of_object_shape_falls_back_to_task_id():
    """Object-keyed tasks have no `stage` field; the id is the stage."""
    assert _stage_of("final_reporting", _task([])) == "final_reporting"


def test_stage_of_honours_spec_stage_class():
    t = _task([])
    t["spec"] = {"stage_class": "differential_expression"}
    assert _stage_of("de_xyz", t) == "differential_expression"


def test_terminal_prefers_final_reporting_over_validators():
    """validate_* tasks sort after their target in topo order; the terminal
    must still resolve to the substantive reporting task, not a validator."""
    tasks = {
        "data_acquisition": _task([]),
        "reporting": _task(["data_acquisition"]),
        "final_reporting": _task(["reporting"]),
        "validate_reporting": _task(["reporting"]),
        "validate_final_reporting": _task(["final_reporting"]),
    }
    order = _topo(tasks)
    stage = {tid: _stage_of(tid, t) for tid, t in tasks.items()}
    assert _terminal_id(order, stage) == "final_reporting"


def test_terminal_falls_back_to_review_when_no_report():
    tasks = {
        "data_acquisition": _task([]),
        "review_prior_work": _task(["data_acquisition"]),
        "validate_review_prior_work": _task(["review_prior_work"]),
    }
    order = _topo(tasks)
    stage = {tid: _stage_of(tid, t) for tid, t in tasks.items()}
    assert _terminal_id(order, stage) == "review_prior_work"


# --- legacy list shape is still tolerated (back-compat) ---

def test_flatten_tolerates_legacy_list_shape(tmp_path):
    wf = {"tasks": [
        {"id": "load", "stage": "data_acquisition", "depends_on": []},
        {"id": "de", "stage": "differential_expression", "depends_on": ["load"]},
        {"id": "report", "stage": "final_reporting", "depends_on": ["de"]},
    ]}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid, txt in [("load", "loaded"), ("de", "sig genes"),
                     ("report", "Final answer here.")]:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\n{txt}\n")
    trace, answer = flatten_outputs(out, tmp_path / "WORKFLOW.json")
    assert trace.index("load") < trace.index("de") < trace.index("report")
    assert "Final answer here." in answer


# --- contract test against a REAL emitted WORKFLOW.json ---

def _emitted_workflows():
    if not EMITTED_PACKAGES.is_dir():
        return []
    return sorted(EMITTED_PACKAGES.glob("*/WORKFLOW.json"))


@pytest.mark.skipif(not _emitted_workflows(),
                    reason="no emitted packages under testdata/emitted-packages/")
@pytest.mark.parametrize("wf_path", _emitted_workflows(),
                         ids=lambda p: p.parent.name)
def test_real_emitted_workflow_flattens_without_crash(wf_path, tmp_path):
    """Contract test: every committed emitted package must flatten cleanly.

    This is the exact shape `eval_runner.py` reads (`tasks` as an object keyed
    by task id, per-task objects without id/stage). Topo order must place every
    dependency before its dependents, the terminal must be a substantive
    reporting task (never a `validate_*`/`discover_*` stub), and
    completion_status must return total == declared task count without raising.
    """
    data = json.loads(wf_path.read_text())
    tasks = _normalize_tasks(data["tasks"])
    assert tasks, f"{wf_path} produced no tasks after normalize"

    order = _topo(tasks)
    assert set(order) == set(tasks), "topo dropped or duplicated tasks"
    pos = {tid: i for i, tid in enumerate(order)}
    for tid, t in tasks.items():
        for dep in t.get("depends_on", []) or []:
            assert pos[str(dep)] < pos[tid], (
                f"{wf_path.parent.name}: dep {dep} sorted after dependent {tid}"
            )

    stage = {tid: _stage_of(tid, t) for tid, t in tasks.items()}
    terminal = _terminal_id(order, stage)
    assert terminal is not None
    assert not terminal.startswith(("validate_", "discover_")), (
        f"{wf_path.parent.name}: terminal {terminal} is a validator/discover stub"
    )

    # completion_status against an absent outputs dir: no crash, all-zero output.
    status = completion_status(tmp_path / "missing-outputs", wf_path)
    assert status["total"] == len(tasks)
    assert status["with_output"] == 0
    assert status["terminal_has_output"] is False


# --- completion_status: incomplete-run detection (object-keyed shape) ---

def _partial_pkg(tmp_path, populated_ids):
    wf = {"tasks": {
        "data_acquisition": _task([]),
        "differential_expression": _task(["data_acquisition"]),
        "final_reporting": _task(["differential_expression"]),
    }}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    out.mkdir(parents=True)
    for tid in populated_ids:
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\noutput for {tid}\n")
    return tmp_path


def test_completion_status_full(tmp_path):
    pkg = _pkg(tmp_path)
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status == {"total": 3, "with_output": 3, "terminal_has_output": True}


def test_completion_status_terminal_missing(tmp_path):
    pkg = _partial_pkg(tmp_path, populated_ids=["data_acquisition"])
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["total"] == 3
    assert status["with_output"] == 1
    assert status["terminal_has_output"] is False


def test_completion_status_empty_outputs_dir(tmp_path):
    pkg = _partial_pkg(tmp_path, populated_ids=[])
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["total"] == 3
    assert status["with_output"] == 0
    assert status["terminal_has_output"] is False


def test_completion_status_empty_narrative_not_counted(tmp_path):
    pkg = _partial_pkg(tmp_path, populated_ids=["data_acquisition",
                                               "differential_expression"])
    blank = pkg / "runtime" / "outputs" / "final_reporting"
    blank.mkdir(parents=True)
    (blank / "report.md").write_text("   \n\t\n")
    status = completion_status(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert status["with_output"] == 2
    assert status["terminal_has_output"] is False


def test_completion_status_missing_workflow_json_does_not_raise(tmp_path):
    status = completion_status(tmp_path / "runtime" / "outputs",
                               tmp_path / "WORKFLOW.json")
    assert status == {"total": 0, "with_output": 0, "terminal_has_output": False}


def test_completion_status_tasks_not_a_collection_returns_zero(tmp_path):
    """A WORKFLOW.json whose `tasks` is a scalar (corrupt) yields all-zero,
    never a crash."""
    (tmp_path / "WORKFLOW.json").write_text(json.dumps({"tasks": 42}))
    status = completion_status(tmp_path / "runtime" / "outputs",
                               tmp_path / "WORKFLOW.json")
    assert status == {"total": 0, "with_output": 0, "terminal_has_output": False}


# --- _narrative unit tests ---

def test_narrative_result_json_narrative_field(tmp_path):
    d = tmp_path / "task1"
    d.mkdir()
    (d / "result.json").write_text(json.dumps({
        "status": "completed",
        "narrative": "Identified 2018 differentially expressed genes at FDR<0.05.",
    }))
    text = _narrative(d)
    assert "2018 differentially expressed genes" in text


def test_narrative_result_json_no_known_field_falls_back_to_json_dump(tmp_path):
    d = tmp_path / "task2"
    d.mkdir()
    data = {"status": "completed", "metrics": {"n_sig": 42}}
    (d / "result.json").write_text(json.dumps(data))
    text = _narrative(d)
    assert "n_sig" in text
    assert "42" in text


def test_narrative_progress_log_fallback(tmp_path):
    d = tmp_path / "task3"
    d.mkdir()
    (d / "progress.log").write_text("Step 1 done\nStep 2 done\n")
    text = _narrative(d)
    assert "Step 1 done" in text
    assert "Step 2 done" in text


def test_narrative_real_agent_result_json_shape(tmp_path):
    """result.json with the full AGENT-EXECUTOR.md shape: the `narrative` key is
    extracted; structured fields (claims/figures/status) are not."""
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
    assert "claim_id" not in text
    assert "supported_by" not in text


# --- executed-code surfacing into the trace ---
#
# The ECAA arm executes real code per task and persists it under
# runtime/outputs/<tid>/scripts/*.{py,R,sh}, but the assembled trace was prose
# only — costing rubric code-mechanics points the work genuinely earned. These
# tests assert the agent's OWN executed code now flows into the graded trace.

_DESEQ2_SCRIPT = (
    "#!/usr/bin/env Rscript\n"
    "# Tool: DESeq2 1.50.2\n"
    "library(DESeq2)\n"
    "df <- df[complete.cases(df), ]  # dropna\n"
    "stopifnot(nrow(df) >= 5)        # n>=5\n"
    "dds <- DESeqDataSetFromMatrix(counts, coldata, ~condition)\n"
)


def _code_pkg(tmp_path, scripts_by_task):
    """3-task workflow with report.md prose plus per-task scripts/ files.

    ``scripts_by_task`` maps task id -> {filename: contents}. Tasks always get a
    report.md so the prose narrative is present alongside the surfaced code.
    """
    wf = {"tasks": {
        "data_acquisition": _task([]),
        "differential_expression": _task(["data_acquisition"]),
        "final_reporting": _task(["differential_expression"]),
    }}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid in ("data_acquisition", "differential_expression", "final_reporting"):
        d = out / tid
        d.mkdir(parents=True)
        (d / "report.md").write_text(f"# {tid}\nprose for {tid}\n")
    for tid, files in scripts_by_task.items():
        sd = out / tid / "scripts"
        sd.mkdir(parents=True)
        for name, contents in files.items():
            (sd / name).write_text(contents)
    return tmp_path


def test_executed_code_block_reads_ondisk_scripts(tmp_path):
    """The on-disk scripts/*.R is surfaced as an ```r fence in the trace; a
    coexisting .log sibling is excluded by the suffix allowlist."""
    pkg = _code_pkg(tmp_path, {
        "differential_expression": {
            "01_deseq2_de_analysis.R": _DESEQ2_SCRIPT,
            "00_install.log": "INSTALL LOG ===> conda install r-deseq2 (noise)\n",
        }
    })
    trace, _ = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert "### Executed code" in trace
    assert "```r" in trace
    assert "library(DESeq2)" in trace
    assert "complete.cases(df)" in trace  # dropna mechanism (C6)
    assert "nrow(df) >= 5" in trace       # n>=5 mechanism (C6)
    # the .log sibling must NOT bleed into the rendered block
    assert "INSTALL LOG" not in trace


def test_executed_code_multiple_scripts_sorted(tmp_path):
    """Multiple scripts of mixed language are each fenced, in sorted order, with
    the per-extension fence language."""
    pkg = _code_pkg(tmp_path, {
        "differential_expression": {
            "02_run_fgsea.R": "library(fgsea)\nfgseaRes <- fgsea(pathways, ranks)\n",
            "01_prep_ranks.py": "import pandas as pd\nranks = df['stat'].dropna()\n",
        }
    })
    trace, _ = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert "```python" in trace
    assert "```r" in trace
    # sorted() on the path puts 01_*.py before 02_*.R
    assert trace.index("import pandas as pd") < trace.index("library(fgsea)")


def test_executed_code_fallback_to_agent_code_json(tmp_path):
    """When no scripts/ dir exists, a non-empty agent-code.json executed_code is
    surfaced, fenced by its language tag."""
    wf = {"tasks": {"differential_expression": _task([])}}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    d = tmp_path / "runtime" / "outputs" / "differential_expression"
    d.mkdir(parents=True)
    (d / "report.md").write_text("# differential_expression\nprose\n")
    (d / "agent-code.json").write_text(json.dumps({
        "prompt": "p",
        "response_text": "",
        "executed_code": "import numpy as np\nbaseline = pre.mean()\npeak = post.max()\n",
        "language": "Python",
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": "2026-01-01T00:01:00Z",
    }))
    trace, _ = flatten_outputs(tmp_path / "runtime" / "outputs", tmp_path / "WORKFLOW.json")
    assert "### Executed code" in trace
    assert "```python" in trace
    assert "baseline = pre.mean()" in trace
    assert "peak = post.max()" in trace


def test_executed_code_block_empty_when_no_source(tmp_path):
    """Neither scripts/ nor a populated agent-code.json yields no code header.

    Mirrors the real-package case: agent-code.json's executed_code is empty in
    practice, so the fallback is a no-op and the trace stays prose-only."""
    wf = {"tasks": {"differential_expression": _task([])}}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    d = tmp_path / "runtime" / "outputs" / "differential_expression"
    d.mkdir(parents=True)
    (d / "report.md").write_text("# differential_expression\nprose only\n")
    (d / "agent-code.json").write_text(json.dumps({
        "prompt": "p",
        "response_text": "",
        "executed_code": "",
        "language": "unknown",
        "started_at": "2026-01-01T00:00:00Z",
        "completed_at": "2026-01-01T00:01:00Z",
    }))
    trace, _ = flatten_outputs(tmp_path / "runtime" / "outputs", tmp_path / "WORKFLOW.json")
    assert "### Executed code" not in trace
    # the empty-executed_code agent-code.json fallback is a no-op
    assert _executed_code_block(d) == ""


def test_existing_prose_trace_unchanged_when_no_code(tmp_path):
    """Regression: the prose-only fixture (_pkg, report.md only, no scripts/ and
    no agent-code.json) yields a byte-identical trace with the code path in
    place — no regression to current prose-only packages."""
    pkg = _pkg(tmp_path)
    trace, _ = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert "### Executed code" not in trace
    # exact expected trace: three prose sections, no code injected
    expected = "\n\n".join(
        f"## Task: {tid} ({tid})\n\n# {tid}\n{txt}\n"
        for tid, txt in [
            ("data_acquisition", "loaded 4 samples"),
            ("differential_expression", "2018 sig genes"),
            ("final_reporting", "Treatment reduces recovery time."),
        ]
    )
    assert trace == expected


def test_executed_code_in_trace_not_answer(tmp_path):
    """Code belongs in the per-step TRACE channel (mirroring the bare arm), not
    the ANSWER channel — the answer is the final-result string, unchanged."""
    pkg = _code_pkg(tmp_path, {
        "final_reporting": {"01_assemble.py": "print('done')\n"},
    })
    trace, answer = flatten_outputs(pkg / "runtime" / "outputs", pkg / "WORKFLOW.json")
    assert "### Executed code" in trace
    assert "print('done')" in trace
    # the answer channel carries only the terminal narrative, no code block
    assert "### Executed code" not in answer
    assert "print('done')" not in answer


def test_empty_terminal_fallback_prefers_answer_txt_deliverable(tmp_path):
    """When the reporting terminal produced no narrative (dead-stalled DAG), the
    answer-channel fallback must surface the agent's OWN answer.txt deliverable
    (e.g. a long ranked-result table) rather than an incidental side-narrative
    whose result.json summary merely happens to be longer. Regression for the
    da-5-1 thin-answer bug: a 14KB drug-target answer.txt sat in the analysis
    task while a 1KB literature-mapping summary won on length."""
    outs = tmp_path / "runtime" / "outputs"
    outs.mkdir(parents=True)
    for t in ("final_reporting", "contextualize", "analysis"):
        (outs / t).mkdir()
    # final_reporting dead-stalled: no result.json/narrative.
    # contextualize: a LONG result.json summary (the incidental side-narrative).
    (outs / "contextualize" / "result.json").write_text(json.dumps({"summary": "L" * 1015}))
    # analysis: a SHORT result.json summary but a RICH answer.txt deliverable.
    (outs / "analysis" / "result.json").write_text(json.dumps({"summary": "short digest"}))
    (outs / "analysis" / "answer.txt").write_text("RANKED TARGETS\n" + "A" * 14000)
    (tmp_path / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "contextualize": {"depends_on": []},
        "analysis": {"depends_on": ["contextualize"]},
        "final_reporting": {"depends_on": ["analysis"]},
    }}))
    _trace, answer = flatten_outputs(outs, tmp_path / "WORKFLOW.json")
    assert answer.strip().startswith("RANKED TARGETS"), "fallback must surface the analysis answer.txt"
    assert len(answer) > 13000, "the rich 14KB deliverable, not the 1KB summary"
    assert "L" * 100 not in answer, "must NOT pick the longer literature side-narrative"
