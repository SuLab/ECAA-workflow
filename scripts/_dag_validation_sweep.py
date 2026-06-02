#!/usr/bin/env python3
"""Comprehensive DAG-correctness validation sweep over the blinded corpus.

For every scenario in the DAG-correctness corpus this:
  1. emits a package via the deterministic `ecaa-workflow intake` path with
     ECAA_VALIDATE_ON_EMIT=full (writes validation-report.json +
     audit-proof-report.json sidecars),
  2. runs scripts/audit_dag.py over WORKFLOW.json (stranded / cycle / terminal),
  3. runs `ecaa-workflow-harness --plan-only` under ECAA_EXECUTOR_MODE=mock
     (the dry-run executor: validate_dag_typed + safety enforcement, exit
     0 clean / 2 dag-fail / 3 safety-blocked),
  4. reads the ECAA validation-report + audit-proof-report verdicts.

Writes a per-scenario row and a summary. Zero LLM, zero agent tokens.

Usage:
    python3 scripts/_dag_validation_sweep.py [--filter SUBSTR] [--out DIR]
"""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

import yaml

REPO = Path(__file__).resolve().parents[1]
MANIFEST = REPO / "testdata" / "dag-correctness-corpus" / "MANIFEST.yaml"
# Honor CARGO_TARGET_DIR — builds are commonly redirected off the repo
# tree (e.g. onto a larger/faster volume), in which case REPO/target is
# empty and the binaries live under $CARGO_TARGET_DIR/release.
_TARGET = Path(os.environ["CARGO_TARGET_DIR"]) if os.environ.get("CARGO_TARGET_DIR") else REPO / "target"
CLI = _TARGET / "release" / "ecaa-workflow"
HARNESS = _TARGET / "release" / "ecaa-workflow-harness"
AUDIT = REPO / "scripts" / "audit_dag.py"
CONFIG = REPO / "config"


def emit(scenario, outdir):
    pf = outdir / (scenario["id"] + ".intake.txt")
    pf.write_text(scenario["blinded_prompt"])
    pkg = outdir / ("pkg_" + scenario["id"])
    env = dict(os.environ)
    env["ECAA_VALIDATE_ON_EMIT"] = "full"
    r = subprocess.run(
        [str(CLI), "intake", "--input", str(pf), "--output", str(pkg),
         "--config", str(CONFIG)],
        capture_output=True, text=True, timeout=300, env=env,
    )
    return pkg, r


def run_audit(wf_path):
    r = subprocess.run([sys.executable, str(AUDIT), str(wf_path)],
                       capture_output=True, text=True)
    return ("PASS" if "RESULT: PASS" in r.stdout else "FAIL"), r.stdout


def run_plan_only(pkg):
    env = dict(os.environ)
    env["ECAA_EXECUTOR_MODE"] = "mock"
    r = subprocess.run(
        [str(HARNESS), "--package", str(pkg), "--agent", "/bin/true", "--plan-only"],
        capture_output=True, text=True, timeout=120, env=env,
    )
    validate_line = ""
    for line in r.stdout.splitlines():
        if line.startswith("validate_dag:"):
            validate_line = line.split(":", 1)[1].strip()
            break
    return r.returncode, validate_line, r.stdout, r.stderr


def read_validation_report(pkg):
    cand = pkg / "runtime" / "validation-summary.json"
    if cand.exists():
        try:
            return json.loads(cand.read_text())
        except Exception as e:
            return {"_error": str(e)}
    return None


def read_audit_proof(pkg):
    for cand in (pkg / "runtime" / "audit-proof-report.json",
                 pkg / "audit-proof-report.json"):
        if cand.exists():
            try:
                return json.loads(cand.read_text())
            except Exception as e:
                return {"_error": str(e)}
    return None


def summarize_validation(rep):
    if rep is None:
        return "NO-REPORT"
    sv = rep.get("schema_validation") or {}
    failed = sv.get("failed") or []
    passed = sv.get("passed", 0)
    if failed:
        return f"FAIL({len(failed)})"
    return f"ok/{passed}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--filter", default="")
    ap.add_argument("--out", default="/tmp/dag_sweep")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    scenarios = yaml.safe_load(MANIFEST.read_text())["scenarios"]
    if args.filter:
        scenarios = [s for s in scenarios if args.filter in s["id"] or args.filter == s.get("tier")]
    if args.limit:
        scenarios = scenarios[: args.limit]

    outdir = Path(args.out)
    outdir.mkdir(parents=True, exist_ok=True)
    rows = []
    print(f"{'id':<42} {'tier':<4} {'emit':<5} {'audit':<5} {'plan':<10} {'valid':<8} {'aproof':<7} tasks")
    print("-" * 100)
    for s in scenarios:
        sid = s["id"]
        tier = s.get("tier")
        pkg, r = emit(s, outdir)
        wf = pkg / "WORKFLOW.json"
        if r.returncode != 0 or not wf.exists():
            tail = (r.stderr or r.stdout).strip().splitlines()[-1:]
            print(f"{sid:<42} {tier:<4} EMIT-FAIL {tail}")
            rows.append({"id": sid, "tier": tier, "emit": "FAIL", "tail": tail})
            continue
        ntasks = len(json.loads(wf.read_text()).get("tasks", {}))
        audit_verdict, audit_out = run_audit(wf)
        rc, vline, plan_out, plan_err = run_plan_only(pkg)
        plan_verdict = {0: "OK", 2: "DAG-FAIL", 3: "SAFETY-BLK"}.get(rc, f"rc={rc}")
        vrep = read_validation_report(pkg)
        vsum = summarize_validation(vrep)
        aproof = read_audit_proof(pkg)
        aproof_sum = "n/a"
        if aproof is not None:
            atxt = json.dumps(aproof).lower()
            aproof_sum = "FAIL" if ('"fail"' in atxt or '"failed"' in atxt) else "PASS"
        flag = ""
        if audit_verdict != "PASS" or plan_verdict not in ("OK", "SAFETY-BLK") or vline != "ok":
            flag = "  <<<"
        print(f"{sid:<42} {tier:<4} {'ok':<5} {audit_verdict:<5} {plan_verdict:<10} {vsum:<8} {aproof_sum:<7} {ntasks}{flag}")
        rows.append({
            "id": sid, "tier": tier, "emit": "ok", "tasks": ntasks,
            "audit": audit_verdict, "audit_out": audit_out,
            "plan_rc": rc, "plan_verdict": plan_verdict, "validate_dag": vline,
            "plan_err": plan_err[-2000:] if plan_verdict not in ("OK", "SAFETY-BLK") else "",
            "validation_summary": vsum, "audit_proof": aproof_sum,
            "pkg": str(pkg),
        })

    (outdir / "_results.json").write_text(json.dumps(rows, indent=2))
    # Summary
    emit_fail = [r for r in rows if r.get("emit") == "FAIL"]
    audit_fail = [r for r in rows if r.get("audit") == "FAIL"]
    plan_fail = [r for r in rows if r.get("plan_verdict") not in ("OK", "SAFETY-BLK", None)]
    valid_fail = [r for r in rows if r.get("validate_dag") not in ("ok", None)]
    schema_fail = [r for r in rows if str(r.get("validation_summary", "")).startswith("FAIL")]
    aproof_fail = [r for r in rows if r.get("audit_proof") == "FAIL"]
    print("\n=== SWEEP SUMMARY ===")
    print(f"scenarios:        {len(rows)}")
    print(f"emit failures:    {len(emit_fail)} {[r['id'] for r in emit_fail]}")
    print(f"audit_dag FAIL:   {len(audit_fail)} {[r['id'] for r in audit_fail]}")
    print(f"plan-only FAIL:   {len(plan_fail)} {[(r['id'], r['plan_verdict']) for r in plan_fail]}")
    print(f"validate_dag !ok: {len(valid_fail)} {[(r['id'], r['validate_dag']) for r in valid_fail]}")
    print(f"schema-valid FAIL:{len(schema_fail)} {[(r['id'], r['validation_summary']) for r in schema_fail]}")
    print(f"audit-proof FAIL: {len(aproof_fail)} {[r['id'] for r in aproof_fail]}")
    print(f"results json:     {outdir / '_results.json'}")
    return 1 if (emit_fail or audit_fail or plan_fail or valid_fail or schema_fail or aproof_fail) else 0


if __name__ == "__main__":
    raise SystemExit(main())
