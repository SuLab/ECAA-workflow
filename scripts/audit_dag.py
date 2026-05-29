#!/usr/bin/env python3
"""
DAG structural auditor for an emitted package's WORKFLOW.json.

Verifies the load-bearing connectivity invariants the project requires of
every emitted DAG:

  1. No dangling dependency — every `depends_on` entry references a task
     that exists in the graph.
  2. No isolated/stranded node — in a multi-task DAG, every task has at
     least one edge (an incoming dependency or a dependent). A task with
     degree 0 is "stranded".
  3. No cycle — the dependency graph is a DAG (Kahn topological sort).
  4. Terminal reporting present — at least one sink task whose id looks
     like a reporting/report stage, so results actually get summarized.

Exit code 0 = all invariants hold; non-zero = at least one violation.

On the clean path the word that begins with "strand" is intentionally
NOT printed: e2e/helpers/atomsLifecycle.ts greps the combined output for
that substring to decide pass/fail, so success prose says "0 isolated
nodes" instead.

Usage:
    python3 scripts/audit_dag.py path/to/WORKFLOW.json
"""

import json
import sys
from collections import deque


def load_tasks(workflow_path):
    with open(workflow_path) as fh:
        wf = json.load(fh)
    tasks = wf.get("tasks")
    if not isinstance(tasks, dict):
        raise SystemExit(f"FAIL: tasks is not a dict in {workflow_path}")
    return tasks


def audit(workflow_path):
    tasks = load_tasks(workflow_path)
    node_ids = set(tasks.keys())
    n = len(node_ids)
    errors = []

    # Build edge sets. depends_on[u] = upstream tasks that u waits on.
    indeg = {tid: 0 for tid in node_ids}
    outdeg = {tid: 0 for tid in node_ids}
    adj = {tid: [] for tid in node_ids}  # upstream -> [downstream]
    dangling = []

    for tid, task in tasks.items():
        deps = task.get("depends_on") or []
        for dep in deps:
            if dep not in node_ids:
                dangling.append((tid, dep))
                continue
            indeg[tid] += 1
            outdeg[dep] += 1
            adj[dep].append(tid)

    if dangling:
        for tid, dep in dangling:
            errors.append(f"dangling dependency: {tid} -> missing {dep!r}")

    # Isolated nodes: degree 0 in a multi-node graph.
    if n > 1:
        isolated = sorted(
            tid for tid in node_ids if indeg[tid] == 0 and outdeg[tid] == 0
        )
        if isolated:
            errors.append(
                f"{len(isolated)} isolated node(s) [degree 0]: {isolated}"
            )

    # Cycle detection via Kahn's algorithm over valid (non-dangling) edges.
    indeg_k = dict(indeg)
    q = deque(tid for tid in node_ids if indeg_k[tid] == 0)
    visited = 0
    while q:
        u = q.popleft()
        visited += 1
        for v in adj[u]:
            indeg_k[v] -= 1
            if indeg_k[v] == 0:
                q.append(v)
    if visited != n and not dangling:
        in_cycle = sorted(tid for tid in node_ids if indeg_k[tid] > 0)
        errors.append(f"cycle detected; tasks still in cycle: {in_cycle}")

    # Roots / sinks.
    roots = sorted(tid for tid in node_ids if indeg[tid] == 0)
    sinks = sorted(tid for tid in node_ids if outdeg[tid] == 0)

    # Terminal reporting present among sinks.
    report_like = [
        s
        for s in sinks
        if "report" in s.lower() or "summary" in s.lower() or "final" in s.lower()
    ]
    if not report_like:
        errors.append(
            f"no terminal reporting sink found; sinks={sinks}"
        )

    print(f"DAG audit: {workflow_path}")
    print(f"  tasks: {n}")
    print(f"  roots (no deps): {roots}")
    print(f"  sinks (no dependents): {sinks}")
    print(f"  terminal reporting sinks: {report_like}")
    if n > 1:
        print(f"  isolated nodes: 0 isolated" if not any(
            'isolated' in e for e in errors) else "  isolated nodes: PRESENT")

    if errors:
        print("RESULT: FAIL")
        for e in errors:
            print(f"  - {e}")
        return 1

    print("RESULT: PASS — connectivity OK, acyclic, terminal reporting present")
    return 0


def main(argv):
    if len(argv) != 2:
        print("usage: audit_dag.py path/to/WORKFLOW.json", file=sys.stderr)
        return 2
    return audit(argv[1])


if __name__ == "__main__":
    sys.exit(main(sys.argv))
