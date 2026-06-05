"""Re-score an already-completed eval run — JUDGING ONLY, no re-execution.

Reads a finished run's journal (the per-base-run `trace.md` / `answer.txt` it
captured), re-runs the judge phase over those traces, and rewrites the
scorecard. It NEVER starts a chat server, NEVER launches the harness, and NEVER
re-executes an agent — so it cannot re-run an incomplete/stranded task. The
judge layer is sha256-cached by (judge_id, rubric, trace, answer), so re-scoring
an already-judged run hits the cache and costs nothing; a run that lost one
judge (e.g. a provider outage) fetches only the missing verdicts.

Use it to (a) regenerate a scorecard after a scorecard-rendering change (e.g.
the two-judge per-arm means), or (b) fold in a judge that was absent at run
time. Unlike `eval_runner --resume`, it does no execution, so a partially-
complete run is re-scored on exactly the rows that DID finish.

Usage: python -m scripts.eval.rescore <run_dir> [--smoke] [--trials N]
"""
from __future__ import annotations
import argparse
import json
import sys
from pathlib import Path

from scripts.eval.benchmark import Arm, Output
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services import judge as judge_mod
from scripts.eval.services.datasets import cache_root
from scripts.eval.services.scorecard import (collect_guard_outcomes,
                                             write_scorecard,
                                             write_public_scorecard)

PLUGINS = {"biomnibench": BiomniBench, "nekrutenko": Nekrutenko}


def _base_key(task_id: str, arm: str, trial: int) -> str:
    return f"{task_id}:{arm}:{trial}"


def _load_base_records(run_dir: Path) -> list[dict]:
    """The kind=='base' journal records (one per completed (task,arm,trial))."""
    jl = run_dir / "journal.jsonl"
    if not jl.exists():
        raise SystemExit(f"no journal at {jl}")
    out = []
    for line in jl.read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("kind") == "base":
            out.append(r)
    return out


def _infer_benchmark(run_dir: Path) -> str:
    name = run_dir.name
    for b in PLUGINS:
        if name.startswith(b):
            return b
    # Fall back to the scorecard's benchmark field.
    sc = run_dir / "scorecard.json"
    if sc.exists():
        try:
            return json.loads(sc.read_text()).get("benchmark", "biomnibench")
        except ValueError:
            pass
    return "biomnibench"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("run_dir")
    ap.add_argument("--benchmark", default=None, choices=list(PLUGINS))
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--trials", type=int, default=1)
    args = ap.parse_args(argv)

    run_dir = Path(args.run_dir)
    benchmark = args.benchmark or _infer_benchmark(run_dir)
    plugin = PLUGINS[benchmark]()

    base_recs = _load_base_records(run_dir)
    if not base_recs:
        print(f"no base records in {run_dir}/journal.jsonl — nothing to re-score",
              file=sys.stderr)
        return 1

    # Reconstruct each completed run's Output from the journaled trace/answer.
    out_by_key: dict[str, Output] = {}
    pkg_by_key: dict[str, str] = {}
    sm_by_key: dict[str, dict] = {}
    arms_seen: list[str] = []
    trials_seen = 0
    for r in base_recs:
        bk = r.get("key")
        if not bk:
            continue
        out_by_key[bk] = Output(r.get("trace_md", ""), r.get("answer_txt", ""),
                                {}, bool(r.get("exit_ok")), float(r.get("wall_secs") or 0.0))
        if r.get("package_dir"):
            pkg_by_key[bk] = r["package_dir"]
        if isinstance(r.get("session_metrics"), dict):
            sm_by_key[bk] = r["session_metrics"]
        if r.get("arm") and r["arm"] not in arms_seen:
            arms_seen.append(r["arm"])
        trials_seen = max(trials_seen, int(r.get("trial", 0)) + 1)

    trials = trials_seen or args.trials
    # Rebuild the Task objects (rubrics live on them) from the cached dataset.
    handle = plugin.fetch(cache_root())
    tasks = plugin.tasks(handle, smoke=args.smoke or True)
    task_by_id = {t.task_id: t for t in tasks}
    arms = [Arm(a) for a in arms_seen] or [Arm.ECAA_WORKFLOW, Arm.CLAUDE_CODE_DIRECT]

    # ---- Judge phase (BOTH judges; cache-backed) ----
    ordered = [(t, a, tr) for t in tasks for a in arms for tr in range(trials)]
    all_requests = []
    idx_by_key: dict[str, int] = {}
    for i, (t, a, tr) in enumerate(ordered):
        bk = _base_key(t.task_id, a.value, tr)
        idx_by_key[bk] = i
        out = out_by_key.get(bk)
        if out is None:
            continue
        for req in plugin.judge_requests(t, a, out):
            all_requests.append({**req, "key": f"{i}:{req['role']}"})

    verdicts = judge_mod.judge_batch(all_requests) if all_requests else {}

    scores = []
    for (t, a, tr) in ordered:
        bk = _base_key(t.task_id, a.value, tr)
        out = out_by_key.get(bk)
        if out is None:
            continue
        i = idx_by_key[bk]
        vd = {}
        for req in plugin.judge_requests(t, a, out):
            key = f"{i}:{req['role']}"
            if key in verdicts:
                vd[req["role"]] = verdicts[key]
        if not vd:
            print(f"[rescore] no verdict for {bk} — skipped", file=sys.stderr)
            continue
        s = plugin.assemble_score(t, a, out, tr, vd)
        # Re-attach guard outcomes + session metrics from the journaled run so
        # the rescored card matches the live one's auxiliary sections.
        pkg = pkg_by_key.get(bk)
        if a == Arm.ECAA_WORKFLOW and pkg and Path(pkg).exists():
            s.extra = dict(s.extra or {})
            s.extra["guard_outcomes"] = collect_guard_outcomes(Path(pkg))
        sm = sm_by_key.get(bk)
        if a == Arm.ECAA_WORKFLOW and isinstance(sm, dict) and sm:
            from scripts.eval.eval_runner import _HARVESTED_METRIC_KEYS
            s.extra = dict(s.extra or {})
            s.extra["session_metrics"] = {k: sm.get(k) for k in _HARVESTED_METRIC_KEYS}
        scores.append(s)

    if not scores:
        print("[rescore] produced zero score rows", file=sys.stderr)
        return 1

    card = plugin.report(scores)
    # Resolve a representative executed ECAA package for the F12 devacuifier probe.
    ref_pkg = next((Path(p) for k, p in pkg_by_key.items()
                    if ":ecaa:" in k and Path(p).exists()), None)
    write_scorecard(card, run_dir, plugin=plugin, package_dir=ref_pkg)
    print(f"wrote {run_dir}/scorecard.md")

    # Refresh the public copy too, with the same provenance the run carried.
    from scripts.eval.services.datasets import datasets_lock_revisions
    import subprocess
    try:
        git_head = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    except Exception:  # noqa: BLE001
        git_head = "unknown"
    write_public_scorecard(card, run_dir, git_head=git_head,
                           datasets_lock=datasets_lock_revisions(), seed=1729,
                           arms=[a.value for a in arms], trials=trials)
    print(f"wrote {run_dir}/scorecard.public.md")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
