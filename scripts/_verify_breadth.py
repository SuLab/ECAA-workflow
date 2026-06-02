#!/usr/bin/env python3
"""Read-only verifier over already-executed breadth packages under /tmp/breadth.

For each package: DAG audit (stranded/cycle/terminal via audit_dag.py), per-task
completion, required-figure coverage, and container-proof count. No execution,
no shared-file race — single sequential pass, prints one block per package.
"""
import json, os, subprocess, sys

ROOT = "/tmp/breadth"
REPO = "/home/a/scripps/ecaa-workflow"

def audit(wf):
    r = subprocess.run([sys.executable, f"{REPO}/scripts/audit_dag.py", wf],
                       capture_output=True, text=True)
    out = r.stdout
    verdict = "PASS" if "RESULT: PASS" in out else "FAIL"
    detail = ""
    if verdict == "FAIL":
        detail = " | ".join(l.strip() for l in out.splitlines()
                            if any(k in l for k in ("isolated", "cycle", "dangling", "no terminal")))
    return verdict, detail

def st(t):
    s = t.get("state", {})
    return s.get("status") if isinstance(s, dict) else s

def git_clean(pkg):
    rev = subprocess.run(["git", "-C", pkg, "rev-parse", "--is-inside-work-tree"],
                         capture_output=True, text=True)
    status = subprocess.run(["git", "-C", pkg, "status", "--porcelain"],
                            capture_output=True, text=True)
    if rev.returncode != 0 or rev.stdout.strip() != "true":
        return False, "not a git repository"
    if status.returncode != 0:
        return False, status.stderr.strip() or "git status failed"
    if status.stdout.strip():
        return False, f"dirty tree: {status.stdout.strip()}"
    return True, ""

names = sorted(d for d in os.listdir(ROOT)
               if os.path.isdir(os.path.join(ROOT, d)) and not d.startswith("_"))
summary = []
for name in names:
    pkg = os.path.join(ROOT, name)
    wf_path = os.path.join(pkg, "WORKFLOW.json")
    if not os.path.exists(wf_path):
        print(f"### {name}: NO WORKFLOW.json")
        summary.append((name, "NO-PKG", 0, 0, 0, 0, []))
        continue
    wf = json.load(open(wf_path))
    T = wf["tasks"]
    averdict, adetail = audit(wf_path)
    git_ok, git_detail = git_clean(pkg)
    inc = [k for k, t in T.items() if st(t) != "completed"]
    figs = 0; miss = []; cproof = 0; plot_tasks = 0
    for k, t in T.items():
        spec = t.get("spec") or {}
        req = spec.get("required_figures") or []
        if req:
            plot_tasks += 1
        fd = os.path.join(pkg, "runtime", "outputs", k, "figures")
        have = set(os.listdir(fd)) if os.path.isdir(fd) else set()
        for f in req:
            if any(h == f + ".png" or h.startswith(f + ".") for h in have):
                figs += 1
            else:
                miss.append(f"{k}/{f}")
        if os.path.exists(os.path.join(pkg, "runtime", "outputs", k, "container-proof.json")):
            cproof += 1
    ok = averdict == "PASS" and git_ok and not inc and not miss
    tag = "PASS" if ok else "FAIL"
    print(f"### {name}: {tag}")
    print(f"    tasks={len(T)} audit={averdict}{(' ['+adetail+']') if adetail else ''}")
    print(f"    package_git={'clean' if git_ok else 'FAIL: ' + git_detail}")
    print(f"    plot_tasks={plot_tasks} figures_rendered={figs} container_proofs={cproof}/{len(T)}")
    if inc:
        print(f"    INCOMPLETE={inc}")
    if miss:
        print(f"    MISSING_FIGS={miss}")
    summary.append((name, tag, len(T), figs, cproof, len(inc), miss, git_ok))

print("\n===== SUMMARY =====")
npass = sum(1 for s in summary if s[1] == "PASS")
tot_tasks = sum(s[2] for s in summary)
tot_figs = sum(s[3] for s in summary)
tot_cproof = sum(s[4] for s in summary)
for s in summary:
    print(f"{s[1]:4} {s[0]:26} tasks={s[2]:2} figs={s[3]:2} cproof={s[4]:2}"
          + (f" MISS={s[6]}" if s[6] else "")
          + (f" INC={s[5]}" if s[5] else "")
          + ("" if s[7] else " GIT=FAIL"))
print(f"\n{npass}/{len(summary)} scenarios fully PASS | "
      f"{tot_tasks} tasks | {tot_figs} figures | {tot_cproof} container-proofs")
