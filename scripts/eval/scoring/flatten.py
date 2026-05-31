"""Generic flattener: emitted package runtime outputs -> trace.md + answer.txt.

trace.md  = per-task narratives concatenated in topological DAG order.
answer.txt = the narrative of the terminal reporting task (stage contains
'report'/'review', else the last task in topo order).
"""
from __future__ import annotations
import json
from pathlib import Path

_NARRATIVE_NAMES = ("report.md", "interpretation.md", "summary.md", "result.md")


def _topo(tasks: list[dict]) -> list[str]:
    by_id = {t["id"]: t for t in tasks}
    seen, order = set(), []

    def visit(tid):
        if tid in seen:
            return
        for dep in by_id.get(tid, {}).get("depends_on", []):
            visit(dep)
        seen.add(tid)
        order.append(tid)

    for t in tasks:
        visit(t["id"])
    return order


def _narrative(task_dir: Path) -> str:
    for name in _NARRATIVE_NAMES:
        p = task_dir / name
        if p.exists():
            return p.read_text()
    mds = sorted(task_dir.glob("*.md"))
    return mds[0].read_text() if mds else ""


def flatten_outputs(outputs_dir: Path, workflow_json: Path) -> tuple[str, str]:
    tasks = json.loads(Path(workflow_json).read_text())["tasks"]
    order = _topo(tasks)
    stage = {t["id"]: t.get("stage", "") for t in tasks}
    sections, terminal_id = [], order[-1] if order else None
    for tid in order:
        narr = _narrative(outputs_dir / tid)
        if narr:
            sections.append(f"## Task: {tid} ({stage.get(tid,'')})\n\n{narr}")
        if any(k in stage.get(tid, "") for k in ("report", "review")):
            terminal_id = tid
    trace = "\n\n".join(sections)
    answer = _narrative(outputs_dir / terminal_id) if terminal_id else ""
    return trace, answer
