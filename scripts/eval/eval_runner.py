"""Operator-run benchmark driver. NOT wired to CI.

Usage: python -m scripts.eval.eval_runner <benchmark> [--smoke]
       [--arms ecaa,claude-direct] [--trials N] [--max-iterations N]
Requires ECAA_EVAL_LIVE=1 plus GEMINI_API_KEY / ECAA_ANTHROPIC_API_KEY
(biomnibench) to actually run; otherwise prints SKIP and exits 0.
"""
from __future__ import annotations
import argparse
import os
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

from scripts.eval.benchmark import Arm
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services import agent_runner
from scripts.eval.services.datasets import cache_root
from scripts.eval.services.scorecard import write_scorecard

REPO_ROOT = Path(__file__).resolve().parents[2]
PLUGINS = {"biomnibench": BiomniBench, "nekrutenko": Nekrutenko}


def _run_one(plugin, task, arm: Arm, trial: int, workdir: Path, max_iter: int):
    spec = plugin.build_run(task, arm, workdir)
    if spec.kind == "ecaa_package":
        intake = workdir / "intake.txt"
        intake.write_text(spec.instruction)
        pkg = workdir / "pkg"
        import subprocess
        subprocess.run(["ecaa-workflow", "intake", "-i", str(intake),
                        "-o", str(pkg), "--config", "config"],
                       cwd=str(REPO_ROOT), check=True)
        spec.package_dir = pkg
        res = agent_runner.run_ecaa_package(pkg, max_iterations=max_iter)
        out = plugin.collect(spec, pkg)
    else:
        res = agent_runner.run_bare(workdir, spec.instruction)
        out = plugin.collect(spec, workdir)
    out.exit_ok, out.wall_secs = res.exit_ok, res.wall_secs
    return plugin.score(task, arm, out, trial)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("benchmark", choices=list(PLUGINS))
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--arms", default="ecaa,claude-direct")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--max-iterations", type=int, default=20)
    args = ap.parse_args(argv)

    if os.environ.get("ECAA_EVAL_LIVE") != "1":
        print("SKIP: set ECAA_EVAL_LIVE=1 (+ GEMINI_API_KEY/ECAA_ANTHROPIC_API_KEY) "
              "to run live benchmarks. This harness is operator-run and never in CI.")
        return 0

    plugin = PLUGINS[args.benchmark]()
    arms = [Arm(a) for a in args.arms.split(",")]
    trials = 1 if args.smoke else args.trials
    handle = plugin.fetch(cache_root())
    tasks = plugin.tasks(handle, smoke=args.smoke)

    scores = []
    with tempfile.TemporaryDirectory() as td:
        base = Path(td)
        for task in tasks:
            for arm in arms:
                for trial in range(trials):
                    wd = base / f"{task.task_id}-{arm.value}-{trial}"
                    scores.append(_run_one(plugin, task, arm, trial, wd, args.max_iterations))

    card = plugin.report(scores)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = REPO_ROOT / "runtime" / "eval-runs" / f"{args.benchmark}-{stamp}"
    write_scorecard(card, out_dir)
    print(f"wrote {out_dir}/scorecard.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
