"""Generic flattener: emitted package runtime outputs -> trace.md + answer.txt.

trace.md  = per-task narratives concatenated in topological DAG order.
answer.txt = the narrative of the terminal reporting task (stage contains
'report'/'review', else the last task in topo order).

WORKFLOW.json shape contract
-----------------------------
The Rust emitter serialises ``tasks`` as a JSON OBJECT keyed by task id
(Rust ``BTreeMap<TaskId, Task>``), and the per-task objects carry NO ``id``
or ``stage`` field — only ``kind``/``state``/``depends_on``/``assignee``/
``description`` (and occasionally ``spec``/``source_atom_id``/
``required_artifacts``/``container``). The task id (the map key) IS the stage
name (``alignment``, ``discover_alignment``, ``final_reporting``, …), so we
use the key as both the task id and the stage signal. ``eval_runner.py``
reads ``tasks`` the same way (``data.get("tasks", {}).items()``).
"""
from __future__ import annotations
import json
from pathlib import Path

_NARRATIVE_NAMES = ("final_report.md", "report.md", "interpretation.md", "summary.md", "result.md")
_RESULT_JSON_KEYS = ("narrative", "interpretation", "summary", "report", "answer", "text")


def _normalize_tasks(tasks) -> dict[str, dict]:
    """Coerce the WORKFLOW.json ``tasks`` payload into an ``{id: task}`` dict.

    The emitter writes an object keyed by task id (the canonical shape). A
    legacy/list shape (``[{"id": ..., "depends_on": ...}, ...]``) is tolerated
    so older fixtures and any hand-written WORKFLOW.json keep flattening — each
    entry's ``id`` becomes the key. Anything else yields an empty mapping.
    """
    if isinstance(tasks, dict):
        return {str(tid): (obj if isinstance(obj, dict) else {})
                for tid, obj in tasks.items()}
    if isinstance(tasks, list):
        out: dict[str, dict] = {}
        for entry in tasks:
            if isinstance(entry, dict) and "id" in entry:
                out[str(entry["id"])] = entry
        return out
    return {}


def _stage_of(task_id: str, task: dict) -> str:
    """Best-effort stage label for a task.

    The object-keyed shape has no ``stage`` field, so the task id is the
    stage (``alignment``, ``final_reporting``, …). A legacy ``stage`` field
    or a ``spec.stage_class`` is honoured when present.
    """
    stage = task.get("stage")
    if isinstance(stage, str) and stage:
        return stage
    spec = task.get("spec")
    if isinstance(spec, dict):
        sc = spec.get("stage_class")
        if isinstance(sc, str) and sc:
            return sc
    return task_id


def _topo(tasks: dict[str, dict]) -> list[str]:
    seen, order = set(), []

    def visit(tid):
        if tid in seen:
            return
        seen.add(tid)
        for dep in tasks.get(tid, {}).get("depends_on", []) or []:
            visit(str(dep))
        order.append(tid)

    for tid in tasks:
        visit(tid)
    return order


def _narrative(task_dir: Path) -> str:
    # (a) well-known report markdown FIRST — the agent's human-facing
    #     deliverable (full interpretation + code + mechanism), which is what
    #     the rubric grades. A reporting task writes both a rich
    #     `final_report.md`/`report.md` AND a `result.json` whose `narrative`
    #     field is a COMPRESSED operational digest; preferring the markdown
    #     surfaces the mechanism-deep report to the judge instead of the digest.
    #     Only reporting stages carry these files, so analytical tasks (which
    #     have only result.json) are unaffected and keep their result.json
    #     narrative. Skip an empty/whitespace markdown so it can't shadow a
    #     good result.json.
    for name in _NARRATIVE_NAMES:
        p = task_dir / name
        if p.exists():
            txt = p.read_text()
            if txt.strip():
                return txt

    # (b) result.json — known narrative keys, else full JSON dump.
    rj = task_dir / "result.json"
    if rj.exists():
        try:
            data = json.loads(rj.read_text())
            if isinstance(data, dict):
                for key in _RESULT_JSON_KEYS:
                    val = data.get(key)
                    if isinstance(val, str) and val.strip():
                        return val
                return json.dumps(data, indent=2)
        except (json.JSONDecodeError, OSError):
            pass

    # (c) any *.md in the dir (sorted)
    mds = sorted(task_dir.glob("*.md"))
    if mds:
        return mds[0].read_text()

    # (d) progress.log
    pl = task_dir / "progress.log"
    if pl.exists():
        return pl.read_text()

    return ""


def _terminal_id(order: list[str], stage: dict[str, str]) -> str | None:
    """Resolve the terminal task whose narrative is the workflow's answer.

    On the real emitted DAG the topo order interleaves leaf ``validate_*``
    tasks *after* the substantive reporting tasks they check (each validator
    depends on its target), so a naive "last task containing report/review"
    lands on ``validate_review_prior_work`` rather than ``final_reporting``.
    Resolution, in priority order:

    1. The last topo-ordered *reporting* task that is itself neither a
       ``validate_*`` validator nor a ``discover_*`` method-selection stub
       (``final_reporting`` / ``reporting``). This is the real answer-bearing
       task.
    2. Failing that, the last topo-ordered non-validator/non-discover task
       whose stage mentions ``review`` (e.g. a workflow whose only terminal is
       ``review_prior_work``).
    3. Failing that, the last non-validator/non-discover task in topo order.
    4. Failing that (degenerate DAG), the last task in topo order.
    """
    if not order:
        return None

    def _substantive(tid: str) -> bool:
        return not (tid.startswith("validate_") or tid.startswith("discover_"))

    def _last(pred) -> str | None:
        chosen = None
        for tid in order:
            if pred(tid):
                chosen = tid
        return chosen

    report = _last(
        lambda tid: _substantive(tid) and "report" in stage.get(tid, "")
    )
    if report is not None:
        return report

    review = _last(
        lambda tid: _substantive(tid) and "review" in stage.get(tid, "")
    )
    if review is not None:
        return review

    substantive_tail = _last(_substantive)
    if substantive_tail is not None:
        return substantive_tail

    return order[-1]


def _result_json_claims_block(task_dir: Path) -> str:
    """Render the terminal task's `result.json` `claims` as a compact block.

    The report markdown is prose; `result.json.claims` carry the structured,
    machine-checkable findings WITH per-claim evidence pointers (and exact
    intermediate counts the prose may omit, e.g. "89,408 baseline tumour
    cells"). Appending them to the terminal narrative surfaces that
    rubric-relevant detail (intermediate counts + traceability) to the judge
    without dropping the rich prose. Empty/absent claims yield ""."""
    rj = task_dir / "result.json"
    if not rj.exists():
        return ""
    try:
        data = json.loads(rj.read_text())
    except (json.JSONDecodeError, OSError):
        return ""
    claims = data.get("claims") if isinstance(data, dict) else None
    if not isinstance(claims, list) or not claims:
        return ""
    lines = ["", "### Structured claims (machine-checkable, with evidence)"]
    for c in claims:
        if isinstance(c, dict):
            txt = c.get("claim") or c.get("text") or ""
            ev = c.get("evidence") or ""
            if txt:
                lines.append(f"- {txt}" + (f"  [evidence: {ev}]" if ev else ""))
        elif isinstance(c, str) and c.strip():
            lines.append(f"- {c}")
    return "\n".join(lines) if len(lines) > 2 else ""


def flatten_outputs(outputs_dir: Path, workflow_json: Path) -> tuple[str, str]:
    tasks = _normalize_tasks(json.loads(Path(workflow_json).read_text())["tasks"])
    order = _topo(tasks)
    stage = {tid: _stage_of(tid, task) for tid, task in tasks.items()}
    sections = []
    terminal_id = _terminal_id(order, stage)
    for tid in order:
        narr = _narrative(outputs_dir / tid)
        # The terminal report also gets its structured claims appended (exact
        # intermediate counts + evidence pointers the prose report can omit).
        if tid == terminal_id:
            narr = (narr + _result_json_claims_block(outputs_dir / tid)).strip()
        if narr:
            sections.append(f"## Task: {tid} ({stage.get(tid,'')})\n\n{narr}")
    trace = "\n\n".join(sections)
    answer = ""
    if terminal_id:
        answer = (_narrative(outputs_dir / terminal_id)
                  + _result_json_claims_block(outputs_dir / terminal_id)).strip()
    return trace, answer


def completion_status(outputs_dir: Path, workflow_json: Path) -> dict:
    """Report how much of the workflow actually produced output.

    Returns ``{"total": N, "with_output": M, "terminal_has_output": bool}``:

    * ``total``    — number of tasks declared in WORKFLOW.json.
    * ``with_output`` — how many of those tasks have a non-empty narrative under
      ``runtime/outputs/<id>/`` (per ``_narrative``).
    * ``terminal_has_output`` — whether the resolved terminal/reporting task
      produced a non-empty narrative.

    Used to distinguish a workflow that stalled mid-run (and therefore has an
    empty/partial answer) from one that completed but scored poorly. Never
    raises; on a malformed/missing WORKFLOW.json returns an all-zero status.
    """
    try:
        tasks = _normalize_tasks(json.loads(Path(workflow_json).read_text())["tasks"])
    except (json.JSONDecodeError, OSError, KeyError, TypeError):
        return {"total": 0, "with_output": 0, "terminal_has_output": False}

    order = _topo(tasks)
    stage = {tid: _stage_of(tid, task) for tid, task in tasks.items()}
    with_output = sum(1 for tid in order if _narrative(outputs_dir / tid).strip())
    terminal_id = _terminal_id(order, stage)
    terminal_has_output = bool(
        terminal_id and _narrative(outputs_dir / terminal_id).strip()
    )
    return {
        "total": len(order),
        "with_output": with_output,
        "terminal_has_output": terminal_has_output,
    }
