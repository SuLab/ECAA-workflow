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


def _isolated_pkg_copy(src_pkg: Path, dest: Path) -> Path:
    """Copy an emitted package tree to a fresh dir so each error-matrix cell
    runs on a clean, re-runnable package (avoids completed-task state bleed)."""
    import shutil
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(src_pkg, dest)
    return dest


def _stage_inputs(pkg_dir: Path, inputs: dict[str, Path]) -> None:
    """Copy each task input file into pkg_dir/inputs/ so the agent has data.

    Missing source files are silently skipped so a partially-staged task still
    runs (the agent will surface the missing file as an error, not a harness
    crash).
    """
    import shutil
    dest = pkg_dir / "inputs"
    dest.mkdir(parents=True, exist_ok=True)
    for _name, src in inputs.items():
        if src.exists():
            shutil.copy2(src, dest / src.name)


def _run_one(plugin, task, arm: Arm, trial: int, workdir: Path, max_iter: int,
             error_matrix: bool = False):
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
        _stage_inputs(pkg, task.inputs)
        res = agent_runner.run_ecaa_package(pkg, max_iterations=max_iter)
        out = plugin.collect(spec, pkg)
    else:
        res = agent_runner.run_bare(workdir, spec.instruction)
        (workdir / "agent-stdout.json").write_text(res.stdout or "")
        out = plugin.collect(spec, workdir)
    out.exit_ok, out.wall_secs = res.exit_ok, res.wall_secs

    if error_matrix:
        # Inject a real run_fn closure so the plugin can re-run the arm under
        # each PATH-shim without knowing which arm is active.
        def _live_run_fn(cell_workdir, env):
            if spec.kind == "ecaa_package":
                pkg_copy = _isolated_pkg_copy(spec.package_dir,
                                              cell_workdir / "pkg")
                r = agent_runner.run_ecaa_package(pkg_copy,
                                                  max_iterations=max_iter,
                                                  env=env)
            else:
                r = agent_runner.run_bare(cell_workdir, spec.instruction,
                                          env=env)
            return r

        cells = plugin.error_matrix(task, arm, workdir, _live_run_fn)
        if cells is not None:
            out.artifacts["error_cells"] = cells

    return plugin.score(task, arm, out, trial)


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("benchmark", choices=list(PLUGINS))
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--arms", default="ecaa,claude-direct")
    ap.add_argument("--trials", type=int, default=3)
    ap.add_argument("--max-iterations", type=int, default=20)
    ap.add_argument("--error-matrix", action="store_true",
                    help="Run the 36-cell PATH-shim fault-injection matrix "
                         "(Nekrutenko only; ignored by other plugins).")
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
                    scores.append(_run_one(plugin, task, arm, trial, wd,
                                           args.max_iterations,
                                           error_matrix=args.error_matrix))

    for arm in arms:
        arm_rows = [s for s in scores if s.arm == arm.value]
        if not arm_rows:
            print(f"ERROR: arm '{arm.value}' produced zero score rows — "
                  "check plugin or task list", file=sys.stderr)
            return 1

    card = plugin.report(scores)
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    out_dir = REPO_ROOT / "runtime" / "eval-runs" / f"{args.benchmark}-{stamp}"
    write_scorecard(card, out_dir)
    print(f"wrote {out_dir}/scorecard.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
