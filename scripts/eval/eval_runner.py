"""Operator-run benchmark driver. NOT wired to CI.

Usage: python -m scripts.eval.eval_runner <benchmark> [--smoke]
       [--arms ecaa,claude-direct] [--trials N] [--max-iterations N]
       [--error-matrix] [--max-parallel N] [--resume <run_dir>]
Requires ECAA_EVAL_LIVE=1 plus GEMINI_API_KEY / ECAA_ANTHROPIC_API_KEY
(biomnibench) to actually run; otherwise prints SKIP and exits 0.
"""
from __future__ import annotations
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timezone
from pathlib import Path

from scripts.eval.benchmark import Arm, Output, RunSpec, Score
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.scheduler import run_phase
from scripts.eval.services import agent_runner
from scripts.eval.services import judge as judge_mod
from scripts.eval.services.chat_client import drive_chat_intake_with_metrics
from scripts.eval.services.chat_server import ChatServer
from scripts.eval.services.datasets import (cache_root, eval_runs_dir,
                                            scratch_root, stage_file)
from scripts.eval.services.journal import Journal
from scripts.eval.services.scorecard import collect_guard_outcomes, write_scorecard

REPO_ROOT = Path(__file__).resolve().parents[2]
PLUGINS = {"biomnibench": BiomniBench, "nekrutenko": Nekrutenko}


class UnrunnablePackageError(RuntimeError):
    """A package handed to an error-matrix cell has zero runnable (pending/ready)
    tasks, so the harness would never advance it and the cell would score against
    stale outputs. Raised instead of silently recovering."""


def _git_head() -> str:
    """Resolve HEAD for the public scorecard's provenance stamp. Best-effort."""
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "HEAD"], cwd=str(REPO_ROOT),
            text=True).strip()
    except (subprocess.CalledProcessError, OSError):
        return "unknown"


def _datasets_lock_path() -> Path:
    """Path to the committed dataset pin file. Indirected so tests can point at
    a fixture lock."""
    return REPO_ROOT / "scripts" / "eval" / "datasets.lock"


def _validate_datasets_lock() -> None:
    """Abort the campaign unless every dataset revision is a frozen 40-char SHA.

    Runs before any fetch / agent invocation so an unpinned lock never silently
    runs against a moving branch. Raises ValueError (caller maps to exit 2)."""
    from scripts.eval.services.datasets import load_lock, validate_lock_pins
    entries = load_lock(_datasets_lock_path())
    pinned = validate_lock_pins(entries)
    print(f"[freeze] datasets.lock pinned: "
          f"{', '.join(f'{n}={r[:12]}' for n, r in pinned)}", file=sys.stderr)


# Mirrors scorecard._BOOTSTRAP_SEED so the freeze record, the public scorecard,
# and the bootstrap CI all carry the same seed.
_CAMPAIGN_SEED = 1729


def _campaign_freeze_record(*, benchmark: str, arms: list[str], trials: int,
                            datasets_lock: str) -> dict:
    """The pre-run provenance freeze for a whole campaign.

    Captures the git HEAD the run launched from, the pinned dataset revisions,
    the arms/trials/seed, and a single UTC frozen_at stamp. Everything except
    frozen_at is deterministic from inputs."""
    return {
        "benchmark": benchmark,
        "git_head": _git_head(),
        "datasets_lock": datasets_lock,
        "arms": list(arms),
        "trials": trials,
        "seed": _CAMPAIGN_SEED,
        "frozen_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }


def _write_campaign_freeze(run_dir: Path, record: dict, *, resuming: bool) -> Path:
    """Persist the campaign freeze record to run_dir/CAMPAIGN-FREEZE.json.

    The freeze is captured at FIRST launch and never re-stamped: on resume an
    existing freeze is left untouched (the campaign's launch commit is the one
    that matters, not the resume commit). A resume of a pre-freeze run dir
    writes the record now so later resumes are anchored."""
    run_dir.mkdir(parents=True, exist_ok=True)
    out = run_dir / "CAMPAIGN-FREEZE.json"
    if resuming and out.exists():
        return out
    tmp = out.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(record, indent=2, sort_keys=True))
    tmp.replace(out)
    return out


def _write_run_manifest(run_dir: Path, *, benchmark: str, argv: list[str],
                        arms: list[str], trials: int, max_iterations: int,
                        intake_mode: str, error_matrix: bool, resuming: bool,
                        freeze_head: str) -> Path:
    """Persist the realized-run manifest to run_dir/run-manifest.json.

    Records exactly how this run was invoked (argv, arms, trials, intake mode,
    error-matrix flag) plus the frozen launch HEAD, so a reader can reconstruct
    the command without re-deriving it from the journal. Re-written on every
    (re)launch — the latest invocation is the authoritative manifest."""
    run_dir.mkdir(parents=True, exist_ok=True)
    out = run_dir / "run-manifest.json"
    manifest = {
        "benchmark": benchmark,
        "argv": list(argv),
        "arms": list(arms),
        "trials": trials,
        "max_iterations": max_iterations,
        "intake_mode": intake_mode,
        "error_matrix": bool(error_matrix),
        "resuming": bool(resuming),
        "freeze_head": freeze_head,
        "written_at": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
    }
    tmp = out.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(manifest, indent=2, sort_keys=True))
    tmp.replace(out)
    return out


def _assert_head_unchanged(freeze_head: str) -> None:
    """Fail the campaign if git HEAD drifted from the frozen launch commit.

    A run that recompiled mid-campaign (HEAD moved) is not reproducible from its
    freeze record. Skipped when the freeze captured 'unknown' (non-git /
    detached launch) — nothing to compare against."""
    if not freeze_head or freeze_head == "unknown":
        return
    now = _git_head()
    if now == "unknown":
        return
    if now != freeze_head:
        raise RuntimeError(
            f"HEAD moved during the campaign: frozen at {freeze_head[:12]}, "
            f"now {now[:12]}. The run is not reproducible from its freeze; "
            f"re-run from a clean checkout.")


def render_prereg_freeze_stanza(freeze_path: Path) -> str:
    """Render the prereg's 'Frozen-at commit SHA' stanza from a
    CAMPAIGN-FREEZE.json. This is the concrete fill-in for the placeholder in
    docs/eval-prereg/campaign-template.md — an operator copies this block into
    the pre-registration BEFORE results are read."""
    rec = json.loads(Path(freeze_path).read_text())
    arms = ", ".join(rec.get("arms", []))
    return (
        "## Campaign freeze (provenance)\n\n"
        f"- **Frozen-at commit SHA:** `{rec.get('git_head', 'unknown')}`\n"
        f"- **Benchmark:** {rec.get('benchmark', '?')}\n"
        f"- **Arms:** {arms}\n"
        f"- **Trials:** {rec.get('trials', '?')}\n"
        f"- **Seed:** {rec.get('seed', '?')}\n"
        f"- **Datasets lock:** {rec.get('datasets_lock', '?')}\n"
        f"- **Frozen at:** {rec.get('frozen_at', '?')}\n"
    )


def _reset_workflow_task_states(pkg: Path) -> None:
    """Flip every task in WORKFLOW.json back to {"status": "pending"} so a copied
    package re-runs from scratch instead of presenting a fully-terminal DAG the
    harness skips. Best-effort: a missing/malformed WORKFLOW.json is left alone."""
    wf = pkg / "WORKFLOW.json"
    try:
        data = json.loads(wf.read_text())
    except (OSError, ValueError):
        return
    tasks = data.get("tasks")
    if isinstance(tasks, dict):
        for t in tasks.values():
            if isinstance(t, dict):
                t["state"] = {"status": "pending"}
    elif isinstance(tasks, list):
        for t in tasks:
            if isinstance(t, dict):
                t["state"] = {"status": "pending"}
    else:
        return
    tmp = wf.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(wf)


def _isolated_pkg_copy(src_pkg: Path, dest: Path) -> Path:
    """Copy an emitted package tree to a fresh dir so each error-matrix cell runs
    on a clean, genuinely re-runnable package. Beyond the raw copy we (a) reset
    every WORKFLOW.json task to pending and (b) delete runtime/outputs/* (the base
    run's per-task VCFs / result.json), while PRESERVING inputs/ and any staged
    reference — otherwise the harness sees a completed DAG, never re-runs, and the
    fault-injection cell is scored against the base run's untouched outputs."""
    if dest.exists():
        shutil.rmtree(dest)
    shutil.copytree(src_pkg, dest)
    _reset_workflow_task_states(dest)
    shutil.rmtree(dest / "runtime" / "outputs", ignore_errors=True)
    return dest


def _stage_inputs(pkg_dir: Path, inputs: dict[str, Path]) -> None:
    """Hardlink each task input file into pkg_dir/inputs/ (copy across devices).

    Missing source files are silently skipped so a partially-staged task still
    runs (the agent surfaces the missing file as an error, not a harness crash).
    """
    dest = pkg_dir / "inputs"
    dest.mkdir(parents=True, exist_ok=True)
    missing = []
    for _name, src in inputs.items():
        if src.exists():
            stage_file(src, dest / src.name)
        else:
            missing.append(str(src))
    if missing:
        print(f"WARNING: {len(missing)} task input(s) missing, staged without them: "
              f"{missing}", file=sys.stderr)


def _read_workflow_task_ids(pkg: Path) -> list[str]:
    """Best-effort list of task ids from WORKFLOW.json. Returns [] when the file
    is absent or malformed so callers degrade to no-op rather than crashing."""
    try:
        data = json.loads((pkg / "WORKFLOW.json").read_text())
    except (OSError, ValueError):
        return []
    return list(data.get("tasks", {}).keys())


def _ready_or_pending_task_count(pkg: Path) -> int:
    """Number of tasks in WORKFLOW.json in a runnable state (pending or ready).
    A package the harness can actually advance must have >=1. Returns 0 when the
    file is absent/malformed so callers fail-closed (treat it as not runnable)."""
    try:
        data = json.loads((Path(pkg) / "WORKFLOW.json").read_text())
    except (OSError, ValueError):
        return 0
    tasks = data.get("tasks")
    items = (tasks.values() if isinstance(tasks, dict)
             else tasks if isinstance(tasks, list) else [])
    count = 0
    for t in items:
        st = t.get("state") if isinstance(t, dict) else None
        status = st.get("status") if isinstance(st, dict) else st
        if status in ("pending", "ready"):
            count += 1
    return count


def _write_auto_approve_discoveries(pkg: Path) -> None:
    """Unattended eval: pre-approve every discover_* method selection so it
    auto-advances to its best-practice top pick instead of blocking
    AwaitingSmeApproval (a benchmark has no SME to confirm, which otherwise
    severs the critical path and strands the workflow). Mirrors the server's
    /auto-approve-discoveries marker. deny=[] so even high-stakes axes advance —
    the benchmark measures execution + analysis quality, not the SME gate."""
    wf = pkg / "WORKFLOW.json"
    axes: set[str] = set()
    try:
        data = json.loads(wf.read_text())
        for tid, t in data.get("tasks", {}).items():
            if tid.startswith("discover_"):
                spec = t.get("spec") or {}
                axes.add(spec.get("stage_class") or tid[len("discover_"):])
    except (OSError, ValueError):
        pass
    marker = pkg / "runtime" / ".sme-auto-approve-discoveries"
    marker.parent.mkdir(parents=True, exist_ok=True)
    marker.write_text(json.dumps(
        {"allow": sorted(axes) if axes else ["*"], "deny": []}, indent=2))


def _write_auto_approve_discovery_gate(pkg: Path) -> None:
    """Unattended eval: auto-advance ONLY the discovery SME-review/selection
    gate — the human-in-the-loop step that has no claude-direct analog.

    Policy (eval-02): a benchmark has no SME to click the discovery-selection
    BlockerCard, and a `discover_*` review gate that never clears strands every
    downstream compute task at Ready (the harness idle-loops to max-iterations).
    That gate is the ONE step the bare arm has no counterpart for, so clearing
    it keeps the comparison fair without softening ECAA.

    What this DOES NOT touch — and why that is deliberate:

    * The silent-completion / empty-result-sentinel guard,
    * the required-artifact guard, and
    * the validation-contract / claim-verification guard
      (all in `crates/harness/src/main.rs`, bypassed only by a per-task
      `runtime/outputs/<task_id>/sme-decisions.json` skip option or by
      `ECAA_SME_AUTO_APPROVE_ALL=1`).

    The previous `_write_auto_approve_all` neutered all three by pre-writing a
    `skip_with_deviation` `sme-decisions.json` for EVERY task and by setting
    `ECAA_SME_AUTO_APPROVE_ALL=1`. That hid exactly the error-catching this eval
    is supposed to measure: ECAA's guards catching a task that completed empty,
    skipped a required artifact, or made an unsupported claim. We keep those
    guards ACTIVE; their outcomes are now a scored dimension (see
    `scorecard.guard_dimension` / `guard_outcomes`).

    The discovery gate (`scheduler::filter_picks_respecting_sme_gate` ->
    `read_confirmed_review_stages`) is on a SEPARATE on-disk signal from the
    post-completion guards, so scoping the bypass is achievable with the shipped
    harness — no rebuild, no env flag. Cleared by ANY of:
    `runtime/.sme-auto-approve-discoveries`,
    `runtime/sme-review-confirmed-<task_id>.json`, or a per-task `decision.json`
    with `auto_picked: true`. We write the marker (via
    `_write_auto_approve_discoveries`) AND a `sme-review-confirmed-*` sidecar per
    `discover_*` task as belt-and-suspenders.
    """
    # (1) discovery axis marker: clears filter_picks_respecting_sme_gate.
    _write_auto_approve_discoveries(pkg)

    task_ids = _read_workflow_task_ids(pkg)

    runtime = pkg / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)

    for tid in task_ids:
        # (2) per-discover review-confirmed sidecar — mirrors the server's
        # /sme-selection write so filter_picks_respecting_sme_gate clears the
        # gate even if the agent never writes decision.auto_picked. ONLY
        # discover_* tasks get this; non-discovery tasks keep every guard active.
        if tid.startswith("discover_"):
            sidecar = runtime / f"sme-review-confirmed-{tid}.json"
            sidecar.write_text(json.dumps({
                "stage": tid,
                "via": "unattended_eval_auto_approve_discovery_gate",
                "auto_approved": True,
            }, indent=2))


# Back-compat alias: callers/tests that imported the old name still work, but
# the behaviour is now the discovery-gate-only scope.
_write_auto_approve_all = _write_auto_approve_discovery_gate


def _strip_method_discovery(pkg) -> None:
    """Opt-in (ECAA_EVAL_SKIP_DISCOVERY=1): remove the literature-grounded
    method-discovery layer from the emitted DAG — `survey_method_landscape`
    plus every `discover_*` task — and drop them from every remaining task's
    `depends_on`.

    Those tasks run heavy live literature retrieval (PubMed MCP, ~15 min for
    the survey alone), the single largest source of agent token / subscription
    usage in a run (and what trips the Max/Pro session limit). The analysis
    atoms each carry their own `attributes.candidate_tools`, and method
    selection is delegated to the execution agent at runtime (the production
    default per CLAUDE.md), so removing the discovery layer leaves a valid,
    leaner DAG: the analysis + reporting half runs unchanged, the agent just
    picks a method from candidate_tools instead of from a survey-ranked table.

    A post-emit WORKFLOW.json rewrite, mirroring the existing eval post-emit
    augmentations (`_write_auto_approve_discovery_gate`,
    `_auto_resolve_guard_block`). No-op unless the env flag is set. Best-effort:
    a malformed/missing WORKFLOW.json is left untouched."""
    if os.environ.get("ECAA_EVAL_SKIP_DISCOVERY") != "1":
        return
    wf = Path(pkg) / "WORKFLOW.json"
    try:
        data = json.loads(wf.read_text())
    except (OSError, ValueError):
        return
    tasks = data.get("tasks")
    if not isinstance(tasks, dict):
        return
    drop = {tid for tid in tasks
            if tid == "survey_method_landscape" or tid.startswith("discover_")}
    if not drop:
        return
    for tid in drop:
        tasks.pop(tid, None)
    for t in tasks.values():
        deps = t.get("depends_on") if isinstance(t, dict) else None
        if isinstance(deps, list) and deps:
            t["depends_on"] = [d for d in deps if d not in drop]
    tmp = wf.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(wf)
    print(f"[eval] ECAA_EVAL_SKIP_DISCOVERY: stripped {len(drop)} "
          f"method-discovery task(s) from {Path(pkg).name}: {sorted(drop)}",
          file=sys.stderr)


def _append_agent_directive(pkg_dir, directive: str | None) -> None:
    """Append a package-wide agent directive to the emitted PROMPT.md so EVERY
    task's agent invocation sees it — agent-claude.sh re-reads PROMPT.md per task
    (`cat $PACKAGE/PROMPT.md`, agent-claude.sh:158). This is a POST-EMIT
    augmentation (like the SME auto-approve markers) and does NOT affect emit-time
    byte-reproducibility (verify-reproducibility.sh compares fresh emits). No-op on
    a None directive; idempotent (won't double-append on a resumed/copied package)."""
    if not directive:
        return
    prompt_md = Path(pkg_dir) / "PROMPT.md"
    try:
        existing = prompt_md.read_text() if prompt_md.exists() else ""
        if directive.strip() in existing:
            return
        prompt_md.write_text(existing.rstrip() + "\n\n" + directive.strip() + "\n")
    except OSError:
        pass


def _intake_mode() -> str:
    """`chat` (default) drives the full server chat-intake path; `cli` keeps the
    legacy no-LLM `ecaa-workflow intake` compile path (offline/CI smoke)."""
    return os.environ.get("ECAA_EVAL_INTAKE", "chat")


def _cli_intake(spec: RunSpec, task, workdir: Path) -> RunSpec:
    """Legacy no-LLM compile path: `ecaa-workflow intake` into workdir/pkg.
    Sets spec.package_dir; leaves spec.session_id None."""
    intake = workdir / "intake.txt"
    intake.write_text(spec.instruction)
    pkg = workdir / "pkg"
    subprocess.run(["ecaa-workflow", "intake", "-i", str(intake),
                    "-o", str(pkg), "--config", "config"],
                   cwd=str(REPO_ROOT), check=True)
    spec.package_dir = pkg
    return spec


def _chat_intake_or_cli(plugin, task, arm: Arm, workdir: Path,
                        server: ChatServer | None) -> RunSpec:
    """Emit the ECAA package for one (task, arm). Default: drive the server
    chat-intake path (sets spec.session_id + spec.package_dir to the
    server-emitted dir). `ECAA_EVAL_INTAKE=cli` falls back to the no-LLM CLI
    compile. Bare-arm specs pass through unchanged. Always stages inputs +
    writes the SME auto-approve markers into the emitted dir."""
    spec = plugin.build_run(task, arm, workdir)
    if spec.kind != "ecaa_package":
        return spec
    if _intake_mode() == "chat":
        if server is None:
            raise RuntimeError("chat intake requested but no ChatServer started")
        # proposal_policy is a Benchmark hook (default "reject"); resolve it
        # defensively so duck-typed/minimal plugins without the method are fine.
        _policy_fn = getattr(plugin, "proposal_policy", lambda _t, _a: "reject")
        sid, pkg, session_metrics = drive_chat_intake_with_metrics(
            server.base_url, spec.instruction,
            locked_methods=plugin.locked_methods(task, arm),
            proposal_policy=_policy_fn(task, arm))
        spec.session_id = sid
        spec.package_dir = pkg
        # RunSpec has no session_metrics slot; stash it dynamically (the
        # dataclass has no slots=True so attribute assignment is permitted).
        # Read defensively via getattr elsewhere so the CLI-intake path (no
        # metrics) stays fine.
        spec.session_metrics = session_metrics
    else:
        _cli_intake(spec, task, workdir)
    _stage_inputs(spec.package_dir, task.inputs)
    # Opt-in: strip the heavy literature-grounded method-discovery layer
    # (survey_method_landscape + discover_*) BEFORE the auto-approve markers so
    # they operate on the leaner DAG. No-op unless ECAA_EVAL_SKIP_DISCOVERY=1.
    _strip_method_discovery(spec.package_dir)
    # NOTE: the agent learns the inputs/ data location from the SME's intake
    # prose (surfaced into PROMPT.md via EmitConfig.objective), NOT from a
    # post-emit directive — all prompting goes through chat intake so the eval
    # tests realistic SME use.
    _write_auto_approve_discovery_gate(spec.package_dir)
    # contamination_directive is an OPT-IN Benchmark hook (default None); resolve
    # it defensively so duck-typed/minimal plugins without the method are fine.
    _directive_fn = getattr(plugin, "contamination_directive", lambda: None)
    _append_agent_directive(spec.package_dir, _directive_fn())
    return spec


def _ensure_package_for_cells(plugin, task, arm: Arm, base_rec: dict | None,
                              workdir: Path, server: ChatServer | None) -> RunSpec:
    """Return a spec with a usable package_dir for error-matrix cells.

    Resume fast-path: if the journaled `package_dir` still exists on disk, reuse
    it (no re-emit, no LLM tokens). Only when the dir is gone do we re-drive
    intake. Cells copy this tree per-cell and run offline (no session)."""
    spec = plugin.build_run(task, arm, workdir)
    if spec.kind != "ecaa_package":
        return spec
    if base_rec:
        recorded = base_rec.get("package_dir")
        if recorded and Path(recorded).exists():
            spec.package_dir = Path(recorded)
            spec.session_id = base_rec.get("session_id")
            if _ready_or_pending_task_count(spec.package_dir) < 1:
                raise UnrunnablePackageError(
                    f"reused package {spec.package_dir} has 0 runnable "
                    f"(pending/ready) tasks; refusing to score a cell against "
                    f"stale outputs")
            return spec
    # Package gone (or never journaled) — re-emit.
    return _chat_intake_or_cli(plugin, task, arm, workdir, server)


def run_base(plugin, task, arm: Arm, trial: int, workdir: Path, max_iter: int,
             server: ChatServer | None):
    """Run one (task, arm, trial) base run; return (output, spec)."""
    spec = _chat_intake_or_cli(plugin, task, arm, workdir, server)
    if spec.kind == "ecaa_package":
        res = agent_runner.run_ecaa_package(
            spec.package_dir, max_iterations=max_iter,
            session_id=spec.session_id,
            server_url=(server.base_url if server is not None else None))
        out = plugin.collect(spec, spec.package_dir)
        sm = getattr(spec, "session_metrics", None)
        if sm:
            out.artifacts = dict(out.artifacts or {})
            out.artifacts["session_metrics"] = sm
    else:
        res = agent_runner.run_bare(workdir, spec.instruction)
        (workdir / "agent-stdout.json").write_text(res.stdout or "")
        out = plugin.collect(spec, workdir)
    out.exit_ok, out.wall_secs = res.exit_ok, res.wall_secs
    return out, spec


def _cell_run_fn(spec, max_iter):
    """Closure that re-runs the arm under a PATH-shim env for one cell.

    Cells copy the emitted package to a fresh, unregistered dir and run the
    harness OFFLINE (session_id/server_url=None): cells measure fault-injection
    robustness, not session round-trip, and a copied package has no live session
    binding. Keeping them sessionless keeps cells fully parallel + decoupled.

    The per-cell copy resets task states to pending and clears stale outputs (see
    _isolated_pkg_copy). We then assert >=1 runnable task before launch: if the
    copy somehow has nothing to run we raise UnrunnablePackageError so the cell is
    recorded errored rather than silently scored against an empty/stale package."""
    def _fn(cell_workdir, env):
        if spec.kind == "ecaa_package":
            pkg_copy = _isolated_pkg_copy(spec.package_dir, cell_workdir / "pkg")
            if _ready_or_pending_task_count(pkg_copy) < 1:
                raise UnrunnablePackageError(
                    f"cell package copy {pkg_copy} has 0 runnable tasks after "
                    f"reset; refusing to score against a non-runnable package")
            # capture=True: the harness stdout/stderr is the reference exec.log,
            # scanned for diagnose signals (offline cells don't stream anyway).
            return agent_runner.run_ecaa_package(
                pkg_copy, max_iterations=max_iter, env=env,
                session_id=None, server_url=None, capture=True)
        return agent_runner.run_bare(cell_workdir, spec.instruction, env=env)
    return _fn


def _base_key(task_id, arm, trial):
    return f"{task_id}:{arm}:{trial}"


def _cell_key(task_id, arm, trial, pattern, tool, seed):
    return f"{task_id}:{arm}:{trial}:cell:{pattern}:{tool}:{seed}"


def _score_to_dict(s: Score) -> dict:
    return {"task_id": s.task_id, "arm": s.arm, "trial": s.trial,
            "overall": s.overall, "dimensions": s.dimensions, "jaccard": s.jaccard,
            "error_cells": s.error_cells, "judge_id": s.judge_id, "extra": s.extra}


def _score_from_dict(d: dict) -> Score:
    return Score(d["task_id"], d["arm"], d["trial"], d["overall"], d["dimensions"],
                 d["jaccard"], d.get("error_cells"), d["judge_id"], d.get("extra", {}))


def _package_dir_for_score(s: Score, spec_by_key: dict, base_recs: dict) -> Path | None:
    """Resolve the executed ECAA package dir for one score row. Prefers the live
    spec; falls back to the journaled `package_dir`. Returns None for the bare
    arm or when no package dir is known/extant."""
    if s.arm != Arm.ECAA_WORKFLOW.value:
        return None
    bk = _base_key(s.task_id, s.arm, s.trial)
    spec = spec_by_key.get(bk)
    pkg = getattr(spec, "package_dir", None) if spec is not None else None
    if pkg is None:
        recorded = (base_recs.get(bk) or {}).get("package_dir")
        pkg = Path(recorded) if recorded else None
    if pkg is None:
        return None
    pkg = Path(pkg)
    return pkg if pkg.exists() else None


def _attach_guard_outcomes(scores: list[Score], spec_by_key: dict,
                           base_recs: dict) -> None:
    """Stash per-row ECAA guard-outcome counts into Score.extra["guard_outcomes"]
    (scorecard aggregates them into the per-arm guard dimension). In-place;
    best-effort (a missing package dir is skipped silently)."""
    for s in scores:
        pkg = _package_dir_for_score(s, spec_by_key, base_recs)
        if pkg is None:
            continue
        s.extra = dict(s.extra or {})
        s.extra["guard_outcomes"] = collect_guard_outcomes(pkg)


# E1: the paper's named conversational / parameter-completion friction metrics.
# Copied positionally via .get(key) (never index) so a missing key is None, not
# a crash, and an older server's smaller snapshot still works.
_HARVESTED_METRIC_KEYS = (
    "followup_count", "time_to_emit_ms", "task_success_rate",
    "method_recommendation_requests", "is_ambiguous", "blockers_encountered",
    "affordance_fallbacks", "coverage_gap_events",
)


def _attach_session_metrics(scores: list[Score], metrics_by_key: dict) -> None:
    """Copy the harvested per-session metrics snapshot (keyed by base_key) onto
    each ECAA Score.extra["session_metrics"], filtered to the named allowlist.
    Bare-arm rows have no session and are skipped. In-place; best-effort."""
    for s in scores:
        if s.arm != Arm.ECAA_WORKFLOW.value:
            continue
        snap = metrics_by_key.get(_base_key(s.task_id, s.arm, s.trial))
        if not isinstance(snap, dict) or not snap:
            continue
        s.extra = dict(s.extra or {})
        s.extra["session_metrics"] = {k: snap.get(k) for k in _HARVESTED_METRIC_KEYS}


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("benchmark", choices=list(PLUGINS))
    ap.add_argument("--smoke", action="store_true")
    ap.add_argument("--arms", default="ecaa,claude-direct")
    ap.add_argument("--trials", type=int, default=3)
    # Default well above a compiled workflow's task count: the ECAA arm's DAG
    # (e.g. nekrutenko variant_calling = 27 tasks) plus retries needs more than
    # the old 20, which stranded the harness mid-run (2/27 completed).
    ap.add_argument("--max-iterations", type=int, default=60)
    ap.add_argument("--error-matrix", action="store_true",
                    help="Run the 36-cell PATH-shim matrix (Nekrutenko only).")
    ap.add_argument("--max-parallel", type=int, default=1,
                    help="Max concurrent agent runs (pool size = global cap).")
    ap.add_argument("--resume", default=None,
                    help="Resume into an existing run dir; skip journaled work.")
    args = ap.parse_args(argv)

    if os.environ.get("ECAA_EVAL_LIVE") != "1":
        print("SKIP: set ECAA_EVAL_LIVE=1 (+ GEMINI_API_KEY/ECAA_ANTHROPIC_API_KEY) "
              "to run live benchmarks. This harness is operator-run and never in CI.")
        return 0

    # Reproducibility gate: refuse to launch against an unpinned dataset (a
    # branch alias like main/HEAD or a truncated/non-hex SHA), before any fetch
    # or agent invocation.
    try:
        _validate_datasets_lock()
    except (ValueError, OSError) as e:
        print(f"ABORT: datasets.lock is unpinned/unreadable: {e}", file=sys.stderr)
        return 2

    plugin = PLUGINS[args.benchmark]()
    arms = [Arm(a) for a in args.arms.split(",")]
    trials = 1 if args.smoke else args.trials
    handle = plugin.fetch(cache_root())
    tasks = plugin.tasks(handle, smoke=args.smoke)

    if args.resume:
        run_dir = Path(args.resume)
    else:
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        run_dir = eval_runs_dir() / f"{args.benchmark}-{stamp}"
    journal = Journal(run_dir)
    resuming = bool(args.resume)

    # Provenance freeze: stamp the launch commit + pins before any agent runs.
    from scripts.eval.services.datasets import datasets_lock_revisions
    _freeze = _campaign_freeze_record(
        benchmark=args.benchmark, arms=[a.value for a in arms],
        trials=trials, datasets_lock=datasets_lock_revisions())
    _write_campaign_freeze(run_dir, _freeze, resuming=resuming)
    _freeze_head = json.loads(
        (run_dir / "CAMPAIGN-FREEZE.json").read_text()).get("git_head", "unknown")
    _write_run_manifest(
        run_dir, benchmark=args.benchmark, argv=list(argv),
        arms=[a.value for a in arms], trials=trials,
        max_iterations=args.max_iterations, intake_mode=_intake_mode(),
        error_matrix=args.error_matrix, resuming=resuming,
        freeze_head=_freeze_head)

    # Only consult the journal when explicitly resuming. A fresh run never skips
    # work or reconstructs from a pre-existing journal (guards against a stamp
    # collision or a stale dir silently poisoning a new run).
    done = journal.completed_keys() if resuming else set()
    base_recs = ({r["key"]: r for r in journal.records() if r.get("kind") == "base"}
                 if resuming else {})
    cell_recs: dict[str, list[dict]] = {}
    if resuming:
        for r in journal.records():
            if r.get("kind") == "cell":
                cell_recs.setdefault(r["parent_key"], []).append(r["cell"])

    # A plugin is "deterministic" (no LLM judge) iff it emits no judge requests.
    is_deterministic = not plugin.judge_requests(
        tasks[0], arms[0], Output("", "", {}, True, 0.0)) if tasks else True

    base_items = [(task, arm, trial)
                  for task in tasks for arm in arms for trial in range(trials)]
    spec_by_key: dict[str, object] = {}
    out_by_key: dict[str, Output] = {}
    score_by_key: dict[str, Score] = {}

    # ECAA_EVAL_KEEP_SCRATCH=1 keeps the per-run package tree (for post-mortem
    # inspection of agent outputs / state.patch / VCFs); default cleans it.
    _keep_scratch = os.environ.get("ECAA_EVAL_KEEP_SCRATCH") == "1"
    base_dir = Path(tempfile.mkdtemp(dir=scratch_root()))

    # The chat-intake path emits packages under ECAA_PACKAGE_ROOT (NOT base_dir),
    # so they survive base_dir cleanup; track them to remove at run end.
    emitted_pkg_dirs: list[Path] = []

    # ---- Chat server: ONE shared instance for the whole run, started only when
    # an ECAA arm is present and chat-intake is enabled (default). The CLI
    # fallback (ECAA_EVAL_INTAKE=cli) and bare-only runs need no server.
    server: ChatServer | None = None
    if (any(a == Arm.ECAA_WORKFLOW for a in arms)
            and _intake_mode() == "chat"):
        server = ChatServer(run_dir).start()

    # ---- PHASE 1a: base runs (parallel) ----
    def _run_base_item(item):
        task, arm, trial = item
        wd = base_dir / f"{task.task_id}-{arm.value}-{trial}"
        out, spec = run_base(plugin, task, arm, trial, wd, args.max_iterations, server)
        if getattr(spec, "package_dir", None) and spec.kind == "ecaa_package":
            emitted_pkg_dirs.append(Path(spec.package_dir))
        rec = {"kind": "base", "key": _base_key(task.task_id, arm.value, trial),
               "task_id": task.task_id, "arm": arm.value, "trial": trial,
               "exit_ok": out.exit_ok, "wall_secs": out.wall_secs,
               "session_id": getattr(spec, "session_id", None),
               "session_metrics": (out.artifacts.get("session_metrics")
                                   if isinstance(out.artifacts, dict) else None),
               "package_dir": (str(spec.package_dir)
                               if getattr(spec, "package_dir", None) else None)}
        if is_deterministic:
            # Score NOW while the run dir is alive (VCFs etc. still exist).
            rec["score"] = _score_to_dict(plugin.score(task, arm, out, trial))
        else:
            rec["trace_md"] = out.trace_md
            rec["answer_txt"] = out.answer_txt
        return item, spec, out, rec

    def _phases() -> int:
        pending_base = [it for it in base_items
                        if _base_key(it[0].task_id, it[1].value, it[2]) not in done]
        # Journal each base run AS IT COMPLETES (via run_phase's on_done), not
        # batched after the whole phase finishes — so a usage-window exhaustion
        # mid-phase leaves already-finished runs durable and --resume skips them.
        # Without this, a phase that spans more than one usage window never
        # journals and re-runs from scratch forever. Journal.append is thread+
        # process-safe (threading.Lock + flock), so on_done can call it directly.
        def _journal_base_done(_it, result):
            if isinstance(result, Exception):
                task, arm, trial = _it
                bk = _base_key(task.task_id, arm.value, trial)
                journal.append({"kind": "base_failed", "fail_of": bk,
                                "error": f"{type(result).__name__}: {result}"})
                print(f"[run] base run {bk} FAILED ({type(result).__name__}: {result}) "
                      f"— left unscored; --resume retries", file=sys.stderr)
            else:
                journal.append(result[3])  # the base rec

        for _it, result in run_phase(pending_base, max_parallel=args.max_parallel,
                                     run_fn=_run_base_item,
                                     on_done=_journal_base_done):
            if isinstance(result, Exception):
                continue
            item, spec, out, rec = result
            k = rec["key"]
            spec_by_key[k] = spec
            out_by_key[k] = out
            if "score" in rec:
                score_by_key[k] = _score_from_dict(rec["score"])
            # journaling happens in _journal_base_done as each run completes

        # Reconstruct already-journaled base runs (resume) from the journal.
        for it in base_items:
            k = _base_key(it[0].task_id, it[1].value, it[2])
            if k in spec_by_key or k not in base_recs:
                continue
            r = base_recs[k]
            if "score" in r:
                score_by_key[k] = _score_from_dict(r["score"])
            else:
                out_by_key[k] = Output(r.get("trace_md", ""), r.get("answer_txt", ""),
                                       {}, r["exit_ok"], r["wall_secs"])

        # ---- PHASE 1b: Nekrutenko error-matrix cells (parallel, flat) ----
        if args.error_matrix and hasattr(plugin, "error_matrix_specs"):
            cell_items = []
            for it in base_items:
                task, arm, trial = it
                bk = _base_key(task.task_id, arm.value, trial)
                spec = spec_by_key.get(bk)
                if spec is None:
                    # Resumed base run with no live spec — reuse the journaled
                    # package dir if it survives on disk (no re-emit, no LLM
                    # tokens); only re-drive intake when the dir is gone.
                    base_rec = base_recs.get(bk)
                    recorded = (base_rec or {}).get("package_dir")
                    wd = base_dir / f"{task.task_id}-{arm.value}-{trial}-reemit"
                    spec = _ensure_package_for_cells(
                        plugin, task, arm, base_rec, wd, server)
                    spec_by_key[bk] = spec
                    # Only schedule cleanup for a FRESHLY re-emitted package; a
                    # journal-reused dir was emitted by a prior run and must
                    # survive for a later resume.
                    pkg_dir = getattr(spec, "package_dir", None)
                    if (pkg_dir and spec.kind == "ecaa_package"
                            and str(pkg_dir) != recorded
                            and Path(pkg_dir) not in emitted_pkg_dirs):
                        emitted_pkg_dirs.append(Path(pkg_dir))
                for cs in plugin.error_matrix_specs():
                    ck = _cell_key(task.task_id, arm.value, trial, *cs)
                    if ck not in done:
                        cell_items.append((task, arm, trial, cs, spec))

            def _run_cell_item(item):
                task, arm, trial, cs, spec = item
                cell = plugin.run_error_cell(task, cs, _cell_run_fn(spec, args.max_iterations))
                bk = _base_key(task.task_id, arm.value, trial)
                return {"kind": "cell",
                        "key": _cell_key(task.task_id, arm.value, trial, *cs),
                        "parent_key": bk, "cell": cell}

            # Journal each cell AS IT COMPLETES (on_done) for the same cross-
            # window durability reason as the base phase above.
            def _journal_cell_done(_it, rec):
                if not isinstance(rec, Exception):
                    journal.append(rec)

            for _it, rec in run_phase(cell_items, max_parallel=args.max_parallel,
                                      run_fn=_run_cell_item,
                                      on_done=_journal_cell_done):
                if isinstance(rec, Exception):
                    continue
                cell_recs.setdefault(rec["parent_key"], []).append(rec["cell"])
                # journaling happens in _journal_cell_done as each cell completes

            for bk, cells in cell_recs.items():
                if bk in score_by_key:
                    score_by_key[bk].error_cells = cells

        # ---- PHASE 2: scores ----
        ordered = [(t, a, tr) for t in tasks for a in arms for tr in range(trials)]
        scores: list[Score] = []
        if is_deterministic:
            scores = [score_by_key[_base_key(t.task_id, a.value, tr)]
                      for (t, a, tr) in ordered
                      if _base_key(t.task_id, a.value, tr) in score_by_key]
        else:
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
                if vd:
                    scores.append(plugin.assemble_score(t, a, out, tr, vd))
                else:
                    # Every judge failed for this row (e.g. all providers out of credits).
                    # Do NOT re-invoke judges live via score() — leave it unscored so
                    # --resume re-judges from the journaled output once credits return.
                    print(f"[judge] no verdict for {bk} — left unscored; --resume re-judges",
                          file=sys.stderr)

        if _keep_scratch:
            print(f"[eval] kept scratch for inspection: {base_dir}", file=sys.stderr)
        else:
            shutil.rmtree(base_dir, ignore_errors=True)

        for arm in arms:
            if not [s for s in scores if s.arm == arm.value]:
                print(f"ERROR: arm '{arm.value}' produced zero score rows", file=sys.stderr)
                return 1

        # eval-02: attach ECAA guard-outcome evidence to each score row. The
        # silent-completion / missing-artifact / validation / claim guards stay
        # active (only the discovery review gate is auto-advanced), so their
        # catches are measured here rather than hidden. Bare-arm rows have no
        # package and carry nothing. Best-effort: a missing/cleaned package dir
        # yields no guard_outcomes (the package survives unless KEEP_SCRATCH).
        _attach_guard_outcomes(scores, spec_by_key, base_recs)

        # E1: harvest the per-session friction metrics onto each ECAA row.
        # Reconstruct from live specs first, falling back to the journal so
        # --resume runs still attach what the base run captured.
        metrics_by_key: dict[str, dict] = {}
        for k, sp in spec_by_key.items():
            sm = getattr(sp, "session_metrics", None)
            if isinstance(sm, dict) and sm:
                metrics_by_key[k] = sm
        for k, r in base_recs.items():
            if k not in metrics_by_key and isinstance(r.get("session_metrics"), dict):
                metrics_by_key[k] = r["session_metrics"]
        _attach_session_metrics(scores, metrics_by_key)

        card = plugin.report(scores)
        # Cost breakdown (per provider + totals), tolerant of partial/deterministic rows.
        card.meta["cost"] = {
            "judge_usd": round(sum(s.extra.get("judge_cost_usd", 0.0) for s in scores), 4),
            "gemini_usd": round(sum(s.extra.get("gemini_cost_usd", 0.0) for s in scores), 4),
            "anthropic_usd": round(sum(s.extra.get("anthropic_cost_usd", 0.0) for s in scores), 4),
            "partial_judging_rows": sum(1 for s in scores if s.extra.get("partial_judging")),
        }
        # Surface stalled/incomplete runs so depressed scores aren't mistaken for poor work.
        incomplete = {bk: o.artifacts.get("incomplete_reason")
                      for bk, o in out_by_key.items() if o.artifacts.get("incomplete_reason")}
        if incomplete:
            card.meta["incomplete_runs"] = incomplete
        # Row-count integrity: flag any (task,arm,trial) that produced no score row.
        expected = len(ordered)
        if len(scores) < expected:
            scored = {(s.task_id, s.arm, s.trial) for s in scores}
            missing = [_base_key(t.task_id, a.value, tr) for (t, a, tr) in ordered
                       if (t.task_id, a.value, tr) not in scored]
            card.meta["incomplete_scorecard"] = {"expected": expected,
                                                 "scored": len(scores), "missing": missing}
            print(f"WARNING: scorecard has {len(scores)}/{expected} rows; missing: {missing}",
                  file=sys.stderr)
        # F12: hand the scorecard a representative executed ECAA package so it
        # disk-probes the real de-vacuifying artifacts (proofs.jsonl / signed
        # sink) for the published benchmarkable set, instead of hardcoding the
        # all-false pre-Phase state. Resolved exactly like the guard-outcomes
        # path (first ECAA row whose package dir is extant); None for a bare-arm-
        # only run leaves the honest pre-Phase fallback.
        ref_pkg = next(
            (pkg for s in scores
             if (pkg := _package_dir_for_score(s, spec_by_key, base_recs)) is not None),
            None,
        )
        # Reproducibility gate: a campaign that recompiled mid-run (HEAD moved
        # off the frozen launch commit) is not reproducible from its freeze.
        _assert_head_unchanged(_freeze_head)
        write_scorecard(card, run_dir, plugin=plugin, package_dir=ref_pkg)
        print(f"wrote {run_dir}/scorecard.md")
        # E2: also emit the cost-redacted, provenance-stamped public copy that
        # `make eval-publish` commits under docs/eval-results/.
        from scripts.eval.services.scorecard import write_public_scorecard
        from scripts.eval.services.datasets import datasets_lock_revisions
        write_public_scorecard(
            card, run_dir,
            git_head=_git_head(),
            datasets_lock=datasets_lock_revisions(),
            seed=1729,  # mirrors scorecard._BOOTSTRAP_SEED
            arms=[a.value for a in arms],
            trials=trials)
        print(f"wrote {run_dir}/scorecard.public.md")
        return 0

    try:
        return _phases()
    finally:
        if server is not None:
            server.stop()
        if not _keep_scratch:
            for pkg in emitted_pkg_dirs:
                shutil.rmtree(pkg, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
