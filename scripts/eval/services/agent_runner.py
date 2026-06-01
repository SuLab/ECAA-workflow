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


def run_ecaa_package(package_dir: Path, *, max_iterations: int = 20,
                     timeout: int = 3600,
                     env: dict | None = None) -> RunResult:
    agent = str(REPO_ROOT / "scripts" / "agent-claude.sh")
    cmd = ["ecaa-workflow-harness", "--package", str(package_dir),
           "--agent", agent, "--max-iterations", str(max_iterations),
           "--no-interactive"]
    t0 = time.time()
    proc = subprocess.run(cmd, cwd=str(REPO_ROOT), timeout=timeout,
                          env=env if env is not None else None)
    return RunResult(proc.returncode == 0, time.time() - t0, package_dir)


def run_bare(workdir: Path, instruction: str, *, timeout: int = 3600,
             env: dict | None = None) -> RunResult:
    """Run the bare benchmark arm inside the bio-min:local container.

    The bare arm calls ``agent-claude.sh <workdir>`` without ``ECAA_TASK_ID``
    set, which triggers the standalone path: the script reads ``workdir/PROMPT.md``
    and runs Claude Code via ``docker run`` against the image named by
    ``ECAA_DEFAULT_CONTAINER_IMAGE`` (defaulting to ``bio-min:local``).

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
        REPO_ROOT / "scripts" / "agent-claude.sh"
    )

    # Build the subprocess environment: start from the caller's env (or the
    # current process env) so credentials, ECAA_* tunables, and PATH additions
    # flow through.  Then guarantee the container image default and that
    # standard system dirs are on PATH so shebang interpreters resolve.
    # ECAA_TASK_ID must NOT be set — its absence is what selects the standalone
    # path inside agent-claude.sh (reads PROMPT.md, no per-task output dir).
    effective_env = env.copy() if env is not None else os.environ.copy()
    effective_env.setdefault("ECAA_DEFAULT_CONTAINER_IMAGE", "bio-min:local")
    effective_env.pop("ECAA_TASK_ID", None)
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
    return RunResult(
        exit_ok=(proc.returncode == 0),
        wall_secs=time.time() - t0,
        run_dir=workdir,
        stdout=proc.stdout or "",
    )
