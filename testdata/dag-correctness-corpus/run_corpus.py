#!/usr/bin/env python3
"""Blinded DAG-correctness corpus driver (offline composer test).

For each scenario in MANIFEST.yaml this:
  1. writes the blinded_prompt to a temp file,
  2. runs the deterministic compiler path `ecaa-workflow intake` against it
     (classify -> archetype -> build -> emit WORKFLOW.json; no LLM, no server),
  3. diffs the emitted DAG's task set against the scenario's expected_dag via
     _harness_lib.evaluate_dag (required present + no forbidden present),
  4. prints one PASS/FAIL line per scenario and a Tier-A / Tier-B summary.

This exercises the deterministic DAG composer only. Tier-B scenarios assert a
`propose_hypothesized_node` fallback that lives in the LLM-mediated conversation
layer, which the `intake` path does not run, so their proposal-capability check
is skipped here (atom coverage is still evaluated).

Usage:
    python3 testdata/dag-correctness-corpus/run_corpus.py [--filter SUBSTR]
                                                          [--bin PATH] [--limit N]
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import yaml
except ImportError:
    sys.exit("pip install pyyaml")

HERE = Path(__file__).resolve().parent
REPO_ROOT = HERE.parents[1]
sys.path.insert(0, str(HERE))
from _harness_lib import corpus_load_validator, evaluate_dag  # noqa: E402

MANIFEST = HERE / "MANIFEST.yaml"


def _find_binary() -> str:
    for profile in ("release", "debug"):
        cand = REPO_ROOT / "target" / profile / "ecaa-workflow"
        if cand.exists():
            return str(cand)
    found = subprocess.run(["bash", "-lc", "command -v ecaa-workflow"],
                           capture_output=True, text=True).stdout.strip()
    if found:
        return found
    sys.exit("ecaa-workflow binary not found — build it: "
             "cargo build -p ecaa-workflow-cli --bin ecaa-workflow")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default=str(MANIFEST))
    ap.add_argument("--bin", default="", help="path to the ecaa-workflow binary")
    ap.add_argument("--config", default=str(REPO_ROOT / "config"))
    ap.add_argument("--filter", default="", help="substring/tier filter on scenario id "
                    "(e.g. 'tier:A', 'archetype:bulk_rnaseq_de', or an id substring)")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--strict-structure", action="store_true",
                    help="treat task-count / structural-edge checks as failures, not warnings")
    args = ap.parse_args()

    scenarios = yaml.safe_load(open(args.manifest))["scenarios"]
    if args.filter.startswith("tier:"):
        scenarios = [s for s in scenarios if s.get("tier") == args.filter.split(":", 1)[1]]
    elif args.filter.startswith("archetype:"):
        scenarios = [s for s in scenarios if s.get("archetype") == args.filter.split(":", 1)[1]]
    elif args.filter:
        scenarios = [s for s in scenarios if args.filter in s["id"]]
    if args.limit:
        scenarios = scenarios[: args.limit]

    ve = corpus_load_validator(scenarios)
    if ve:
        for e in ve:
            print(f"[corpus-validator] {e}", file=sys.stderr)
        return 2

    binary = args.bin or _find_binary()
    wd = tempfile.mkdtemp(prefix="dag_corpus_")
    n = len(scenarios)
    pa = fa = pb = fb = 0
    for i, s in enumerate(scenarios):
        sid, tier = s["id"], s["tier"]
        pf = os.path.join(wd, sid + ".txt")
        Path(pf).write_text(s["blinded_prompt"])
        od = os.path.join(wd, "pkg_" + sid)
        r = subprocess.run([binary, "intake", "--input", pf, "--output", od,
                            "--config", args.config], capture_output=True, text=True, timeout=180)
        wfp = os.path.join(od, "WORKFLOW.json")
        if r.returncode != 0 or not os.path.exists(wfp):
            tail = (r.stderr or r.stdout).strip().splitlines()[-1:]
            print(f"[{i+1:>2}/{n}] EMIT-FAIL {tier} {sid}: {tail}")
            fa += tier == "A"
            fb += tier == "B"
            continue
        wf = json.load(open(wfp))
        ok, fails, warns = evaluate_dag(wf, s["expected_dag"],
                                        strict_structure=args.strict_structure,
                                        collected_caps=set())
        # Proposal-capability misses are an LLM-layer concern, not a composer
        # bug — drop them on this deterministic path.
        real = [f for f in fails if "proposal" not in f.lower()]
        tag = "PASS" if not real else "FAIL"
        mod = wf.get("meta", {}).get("modality_id")
        print(f"[{i+1:>2}/{n}] {tag:4} {tier} {sid:<42} tasks={len(wf.get('tasks', {})):<3} mod={mod}")
        for f in real:
            print(f"          x {f}")
        if tier == "A":
            pa += not real; fa += bool(real)
        else:
            pb += not real; fb += bool(real)

    print(f"\n=== ecaa-workflow composer: "
          f"Tier-A {pa}/{pa+fa} PASS ({fa} FAIL) | Tier-B {pb}/{pb+fb} PASS ===")
    return 1 if fa else 0


if __name__ == "__main__":
    raise SystemExit(main())
