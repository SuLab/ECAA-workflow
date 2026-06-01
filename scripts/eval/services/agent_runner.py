"""Subprocess wrappers around the real ecaa-workflow execution path.

run_ecaa_package -> ecaa-workflow-harness loops scripts/agent-claude.sh.
run_bare        -> Claude Code inside bio-min:local via agent-claude.sh's
                   standalone (no ECAA_TASK_ID) path.
Both return where outputs landed; the plugin's collect() reads them.
"""
from __future__ import annotations
import os
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]


@dataclass
class RunResult:
    exit_ok: bool
    wall_secs: float
    run_dir: Path
    stdout: str = ""


def eval_model() -> str:
    """The single model BOTH arms run, so the ecaa-vs-direct delta isolates the
    scaffolding rather than model capability (the bare arm runs one model, so the
    ECAA arm must too). Override with ECAA_EVAL_MODEL."""
    return os.environ.get("ECAA_EVAL_MODEL", "claude-sonnet-4-6")


def run_ecaa_package(package_dir: Path, *, max_iterations: int = 60,
                     timeout: int | None = None,
                     env: dict | None = None,
                     session_id: str | None = None,
                     server_url: str | None = None) -> RunResult:
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
    # Unattended eval: uniformly bypass every SME gate so the harness never
    # parks on `waiting_for_sme`. Harmless when the harness build predates the
    # env-read — the marker-file path (_write_auto_approve_all) still applies.
    effective_env.setdefault("ECAA_SME_AUTO_APPROVE_ALL", "1")
    effective_env.setdefault("MAX_TURNS_PER_TASK", "60")  # was 40 — too tight w/ installs
    # Whole-harness subprocess ceiling: a multi-task DAG with per-task deadlines
    # needs more than 1h. Env-tunable; default 2h.
    harness_timeout = timeout if timeout is not None else int(
        os.environ.get("ECAA_EVAL_HARNESS_TIMEOUT", "7200"))
    t0 = time.time()
    proc = subprocess.run(cmd, cwd=str(REPO_ROOT), timeout=harness_timeout,
                          env=effective_env)
    return RunResult(proc.returncode == 0, time.time() - t0, package_dir)


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
