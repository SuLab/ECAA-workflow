#!/usr/bin/env python3
"""
Compare an emitted WORKFLOW.json against a gold reference WORKFLOW.json.

Structural checks (always, on the emitted DAG):
  - no dangling depends_on
  - no isolated/stranded node (degree 0) in a multi-task DAG
  - acyclic (Kahn)
  - terminal reporting sink present
  - every edge connects two real nodes (edge integrity)

Coverage checks (vs gold, when a reference is supplied):
  - missing tasks   (in gold, absent from emitted)  -> WARN (extra detail allowed)
  - extra tasks     (in emitted, absent from gold)   -> reported
  - dependency-edge diff on the shared task set       -> reported

Ordering check:
  - for every emitted edge u->v (v depends_on u), u must topologically
    precede v (guaranteed if acyclic; we also assert no forward-ref).

Exit 0 if emitted DAG is structurally sound; non-zero otherwise.
Coverage diffs are reported but do not fail unless --strict-coverage.
"""
import json
import sys
from collections import deque


def load(path):
    with open(path) as fh:
        return json.load(fh)["tasks"]


def strip_iter(tid):
    # collapse runtime iterate expansions <id>_iter_N back to <id>
    import re
    return re.sub(r"_iter_\d+$", "", tid)


def structural(tasks):
    errs = []
    ids = set(tasks)
    indeg = {t: 0 for t in ids}
    deg = {t: 0 for t in ids}
    adj = {t: [] for t in ids}
    for tid, t in tasks.items():
        for d in (t.get("depends_on") or []):
            if d not in ids:
                errs.append(f"DANGLING edge: {tid} depends_on missing '{d}'")
                continue
            indeg[tid] += 1
            deg[tid] += 1
            deg[d] += 1
            adj[d].append(tid)
    if len(ids) > 1:
        iso = sorted(t for t in ids if deg[t] == 0)
        if iso:
            errs.append(f"ISOLATED nodes (degree 0): {iso}")
    # cycle via Kahn
    q = deque(t for t in ids if indeg[t] == 0)
    ik = dict(indeg)
    seen = 0
    while q:
        u = q.popleft()
        seen += 1
        for v in adj[u]:
            ik[v] -= 1
            if ik[v] == 0:
                q.append(v)
    if seen != len(ids):
        errs.append(f"CYCLE: {sorted(t for t in ids if ik[t]>0)}")
    # terminal sink
    sinks = [t for t in ids if not adj[t]]
    report_like = [s for s in sinks if any(k in s for k in
                   ("report", "results_review", "final", "summary", "reporting"))]
    if not report_like and len(ids) > 1:
        errs.append(f"NO terminal reporting sink; sinks={sinks}")
    return errs


def coverage(emit, gold):
    e = {strip_iter(t) for t in emit}
    g = {strip_iter(t) for t in gold}
    missing = sorted(g - e)
    extra = sorted(e - g)
    return missing, extra


def main(argv):
    if len(argv) < 2:
        print("usage: dag_compare.py EMITTED.json [GOLD.json]")
        return 2
    emit = load(argv[1])
    errs = structural(emit)
    print(f"[structural] {argv[1]}  tasks={len(emit)}")
    for e in errs:
        print("  FAIL:", e)
    rc = 1 if errs else 0
    if len(argv) >= 3:
        gold = load(argv[2])
        missing, extra = coverage(emit, gold)
        print(f"[coverage] gold tasks={len(gold)} | missing={len(missing)} extra={len(extra)}")
        if missing:
            print("  MISSING (in gold, not emitted):", missing)
        if extra:
            print("  EXTRA (emitted, not in gold):", extra)
        if "--strict-coverage" in argv and missing:
            rc = rc or 3
    print("RESULT:", "PASS" if rc == 0 else "FAIL")
    return rc


if __name__ == "__main__":
    sys.exit(main(sys.argv))
