"""Subprocess wrappers around the real ecaa-workflow execution path.

run_ecaa_package -> ecaa-workflow-harness loops scripts/agent-claude.sh.
run_bare        -> Claude Code inside bio-min:local via agent-claude.sh's
                   standalone (no ECAA_TASK_ID) path.
Both return where outputs landed; the plugin's collect() reads them.
"""
from __future__ import annotations
import json
import os
import subprocess
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]

# Skip option the harness's sme_skip::detect_intent honors — accept a documented
# deviation instead of re-blocking (crates/harness/src/sme_skip.rs SKIP_OPTION_IDS).
_EVAL_SKIP_OPTION = "emit_skip_sentinel_row"


@dataclass
class RunResult:
    exit_ok: bool
    wall_secs: float
    run_dir: Path
    stdout: str = ""
    resolved_blocks: list = field(default_factory=list)


def _eval_max_relaunch() -> int:
    """Bounded harness relaunches to auto-resolve guard-blocked tasks in an
    unattended run. Default 0 = single-shot (preserve current behavior AND the
    guard-outcome scoring); opt in with ECAA_EVAL_MAX_RELAUNCH=N."""
    try:
        return max(0, int(os.environ.get("ECAA_EVAL_MAX_RELAUNCH", "0")))
    except ValueError:
        return 0


def _blocked_guard_tasks(package_dir: Path) -> list[tuple[str, str]]:
    """Tasks the harness left Blocked on a guard (WORKFLOW.json state.status ==
    'blocked'), with their reason. Mirrors the block parse in
    scorecard.collect_guard_outcomes."""
    try:
        data = json.loads((package_dir / "WORKFLOW.json").read_text())
    except (OSError, ValueError):
        return []
    tasks = data.get("tasks", {})
    items = tasks.items() if isinstance(tasks, dict) else [
        (t.get("id"), t) for t in tasks]
    out: list[tuple[str, str]] = []
    for tid, t in items:
        st = t.get("state") if isinstance(t, dict) else None
        status = st.get("status") if isinstance(st, dict) else st
        if status == "blocked":
            rec = st.get("record") or {} if isinstance(st, dict) else {}
            out.append((str(tid), str(rec.get("reason") or "")))
    return out


def _task_status_counts(package_dir: Path) -> dict[str, int]:
    """Histogram of task statuses in WORKFLOW.json. Empty on parse failure."""
    try:
        data = json.loads((package_dir / "WORKFLOW.json").read_text())
    except (OSError, ValueError):
        return {}
    tasks = data.get("tasks", {})
    items = tasks.values() if isinstance(tasks, dict) else tasks
    counts: dict[str, int] = {}
    for t in items:
        st = t.get("state") if isinstance(t, dict) else None
        status = st.get("status") if isinstance(st, dict) else st
        counts[str(status)] = counts.get(str(status), 0) + 1
    return counts


def _auto_resolve_guard_block(package_dir: Path, task_id: str, reason: str) -> None:
    """Unattended eval: accept a guard-blocked task as a documented deviation so
    execution can continue. The guard already RAN and recorded its catch (e.g.
    runtime/validation-reports.jsonl) in the run that produced the block — this
    clears the block AFTER that catch, so it does not suppress the measurement
    (pre-writing the skip WOULD, since detect_intent short-circuits the
    validators; we never do that). Writes the sme-decisions.json the harness
    honors + flips the task blocked->ready in WORKFLOW.json (mirrors the server's
    resume_blocked_tasks_in_workflow) + appends a transparency record."""
    dec_dir = package_dir / "runtime" / "outputs" / task_id
    dec_dir.mkdir(parents=True, exist_ok=True)
    (dec_dir / "sme-decisions.json").write_text(json.dumps({
        "task_id": task_id,
        "decisions": [{"id": "eval_auto", "chosen": _EVAL_SKIP_OPTION}],
        "rationale": (
            "eval unattended auto-resolve: the guard already recorded its catch "
            "in the run that blocked this task; accepting a documented deviation "
            f"so the workflow can complete (reason: {reason[:160]})"),
    }, indent=2))
    # Flip blocked->ready so the relaunched harness re-dispatches it.
    wf = package_dir / "WORKFLOW.json"
    data = json.loads(wf.read_text())
    tasks = data.get("tasks", {})
    t = tasks.get(task_id) if isinstance(tasks, dict) else next(
        (x for x in tasks if x.get("id") == task_id), None)
    if t is not None:
        t["state"] = {"status": "ready"}
    tmp = wf.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2))
    tmp.replace(wf)
    # On-disk transparency record (for the scorecard meta / post-hoc audit).
    rec_path = package_dir / "runtime" / ".eval-auto-resolved-blocks.json"
    try:
        existing = json.loads(rec_path.read_text()) if rec_path.exists() else []
    except (OSError, ValueError):
        existing = []
    existing.append({"task_id": task_id, "reason": reason})
    rec_path.parent.mkdir(parents=True, exist_ok=True)
    rec_path.write_text(json.dumps(existing, indent=2))


def eval_model() -> str:
    """The single model BOTH arms run, so the ecaa-vs-direct delta isolates the
    scaffolding rather than model capability (the bare arm runs one model, so the
    ECAA arm must too). Override with ECAA_EVAL_MODEL."""
    return os.environ.get("ECAA_EVAL_MODEL", "claude-sonnet-4-6")


def run_ecaa_package(package_dir: Path, *, max_iterations: int = 60,
                     timeout: int | None = None,
                     env: dict | None = None,
                     session_id: str | None = None,
                     server_url: str | None = None,
                     capture: bool = False) -> RunResult:
    agent = str(REPO_ROOT / "scripts" / "agent-claude.sh")
    # Per-task wall deadline (default 600s, env-tunable): variant calling + an
    # in-container tool install exceed the harness's 300s default. The harness
    # exposes --task-timeout (main.rs); the eval never passed it before.
    task_timeout = int(os.environ.get("ECAA_EVAL_TASK_TIMEOUT", "600"))
    cmd = ["ecaa-workflow-harness", "--package", str(package_dir),
           "--agent", agent, "--max-iterations", str(max_iterations),
           "--task-timeout", str(task_timeout), "--no-interactive"]
    # Route harness lifecycle/progress back to the emitting chat session (and
    # enable the per-session agent install cache). Only when BOTH are set: a
    # session-id with no server-url has nowhere to post. Error-matrix cells run
    # against unregistered package copies and pass neither (offline cells).
    if session_id and server_url:
        cmd += ["--session-id", session_id, "--server-url", server_url]
    # Pin the ECAA arm to the same model as the bare arm (fairness): the override
    # makes agent-claude.sh bypass per-task model tiering. setdefault so an
    # explicit caller/operator value wins.
    effective_env = (env.copy() if env is not None else os.environ.copy())
    effective_env.setdefault("ECAA_AGENT_MODEL_OVERRIDE", eval_model())
    # eval-02: do NOT set ECAA_SME_AUTO_APPROVE_ALL. That env flag is an
    # all-or-nothing bypass — `sme_skip::detect_intent` returns EmitSentinel for
    # EVERY task and `scheduler::filter_picks_respecting_sme_gate` short-circuits,
    # which disables the silent-completion, missing-artifact, and
    # validation/claim guards we now KEEP active to measure ECAA's
    # error-catching. The discovery review gate (the only SME step with no
    # claude-direct analog) is cleared narrowly by the marker files
    # `_write_auto_approve_discovery_gate` writes into the package, not by this
    # blanket flag. If a caller pre-set it (e.g. a debugging operator run), we
    # leave their value untouched and surface it in the guard-outcome dimension.
    effective_env.setdefault("MAX_TURNS_PER_TASK", "60")  # was 40 — too tight w/ installs
    # Execution-environment fairness: BOTH arms run UNCAPPED in the same bio-min
    # container (the canonical eval), so the ecaa-vs-bare delta isolates the
    # scaffolding, not a resource asymmetry. (A Harbor-comparable per-step cap was
    # intentionally dropped: it cannot equalize a decomposed DAG against Harbor's
    # single-task budget, and clamping ECAA suppresses the orchestration/sizing
    # capability the eval exists to measure.)
    # Whole-harness subprocess ceiling: a multi-task DAG with per-task deadlines
    # needs more than 1h. Env-tunable; default 2h.
    harness_timeout = timeout if timeout is not None else int(
        os.environ.get("ECAA_EVAL_HARNESS_TIMEOUT", "7200"))
    t0 = time.time()
    # Error-matrix cells run with capture=True so the harness's stdout/stderr is
    # available as the reference's exec.log for diagnose scoring (offline cells
    # don't stream to a session anyway). Base runs leave capture=False to keep
    # live console/session streaming intact.
    # Single subprocess by default. With ECAA_EVAL_MAX_RELAUNCH>=1, after the
    # harness exits with guard-blocked tasks (e.g. an upstream survey_method_
    # landscape failing Phase-13 obligations strands the whole DAG), resolve each
    # block (skip sme-decision + blocked->ready) and relaunch, bounded — so an
    # unattended run completes instead of stranding while the guard-catch stays
    # measured (the validators already ran + reported before the block).
    max_relaunch = _eval_max_relaunch()
    relaunches = 0
    resolved: list[str] = []
    captured = ""
    prev_completed = -1
    while True:
        proc = subprocess.run(cmd, cwd=str(REPO_ROOT), timeout=harness_timeout,
                              env=effective_env,
                              capture_output=capture, text=True if capture else None)
        if capture:
            captured += (proc.stdout or "") + (proc.stderr or "")
        if relaunches >= max_relaunch:
            break
        counts = _task_status_counts(package_dir)
        completed = counts.get("completed", 0)
        remaining = sum(counts.get(s, 0) for s in ("pending", "ready", "running"))
        blocked = _blocked_guard_tasks(package_dir)
        if blocked:
            # Guard-blocked tasks: accept the documented deviation + flip ready,
            # relaunch. Checked BEFORE the terminal-DAG test because a blocked
            # task resolves to ready and is then runnable on the next launch.
            for tid, reason in blocked:
                _auto_resolve_guard_block(package_dir, tid, reason)
                resolved.append(tid)
        elif remaining == 0:
            break  # DAG fully terminal (all completed / failed) — nothing to continue.
        elif completed > prev_completed:
            # No blocks, but the DAG is incomplete and the last launch made
            # progress: the harness exited early (hit --max-iterations or its
            # wall-clock timeout) with unblocked work still pending. Completed
            # task state persists on disk, so relaunching simply CONTINUES the
            # DAG from where it stopped. Gate on forward progress so a genuinely
            # wedged DAG (no completions this launch) stops instead of spinning.
            pass
        else:
            break  # no blocks and no progress since last launch — stop (stuck).
        prev_completed = completed
        relaunches += 1
    return RunResult(proc.returncode == 0, time.time() - t0, package_dir,
                     captured, resolved_blocks=resolved)


def run_bare(workdir: Path, instruction: str, *, timeout: int = 3600,
             env: dict | None = None) -> RunResult:
    """Run the bare benchmark arm inside the bio-min:local container.

    The bare arm calls ``scripts/eval/_bare_agent.sh <workdir>`` — a clean
    container runner that reads ``workdir/PROMPT.md`` and runs Claude Code via
    ``docker run`` against ``ECAA_DEFAULT_CONTAINER_IMAGE`` (default
    ``bio-min:local``) with NO ecaa task scaffolding (no DAG, no appended task
    contract, no state.patch.json/retry machinery — agent-claude.sh's retry is
    coupled to that contract and is wrong for a bare prompt). claude's JSON
    result envelope is printed to stdout and captured here.

    This ensures the bare arm has the same toolchain, credential mounts, and
    container environment as the ECAA arm — the comparison isolates the
    compiler/typed-spec/claim-verifier scaffolding while holding the execution
    environment constant.  Inputs are staged into ``workdir`` by the plugin's
    ``build_run`` before this function is called; the agent writes ``trace.md``
    and ``answer.txt`` into ``workdir``, which ``collect()`` reads unchanged.

    Override the agent script path with ``ECAA_EVAL_BARE_AGENT_SCRIPT`` to
    inject a stub in tests without touching the real container.
    """
    workdir.mkdir(parents=True, exist_ok=True)
    (workdir / "PROMPT.md").write_text(instruction)

    agent_script = os.environ.get("ECAA_EVAL_BARE_AGENT_SCRIPT") or str(
        REPO_ROOT / "scripts" / "eval" / "_bare_agent.sh"
    )

    # Build the subprocess environment from the caller's env (or the current
    # process env) so credentials, ECAA_* tunables, and PATH additions flow
    # through; guarantee the container image default and that standard system
    # dirs are on PATH so shebang interpreters resolve. No ECAA_TASK_ID — the
    # bare runner carries no ecaa task scaffolding.
    effective_env = env.copy() if env is not None else os.environ.copy()
    effective_env.setdefault("ECAA_DEFAULT_CONTAINER_IMAGE", "bio-min:local")
    effective_env.setdefault("ECAA_EVAL_BARE_MODEL", eval_model())
    for sysdir in ("/usr/bin", "/bin"):
        if sysdir not in effective_env.get("PATH", "").split(os.pathsep):
            effective_env["PATH"] = effective_env.get("PATH", "") + os.pathsep + sysdir

    t0 = time.time()
    proc = subprocess.run(
        [agent_script, str(workdir)],
        cwd=str(REPO_ROOT),
        timeout=timeout,
        env=effective_env,
        capture_output=True,
        text=True,
    )
    # _bare_agent.sh prints claude's --output-format=json result to stdout.
    captured = proc.stdout or ""
    return RunResult(
        exit_ok=(proc.returncode == 0),
        wall_secs=time.time() - t0,
        run_dir=workdir,
        stdout=captured,
    )
