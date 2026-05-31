"""Subprocess wrappers around the real ecaa-workflow execution path.

run_ecaa_package -> ecaa-workflow-harness loops scripts/agent-claude.sh.
run_bare        -> raw Claude Code agentic loop in a plain workdir.
Both return where outputs landed; the plugin's collect() reads them.
"""
from __future__ import annotations
import os
import subprocess
import time
from dataclasses import dataclass
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]
# Flags extracted from scripts/agent-claude.sh (lines 1121, 1280):
# claude --dangerously-skip-permissions --output-format=json -p "$PROMPT"
CLAUDE_FLAGS = ["--dangerously-skip-permissions", "--output-format=json", "-p"]


@dataclass
class RunResult:
    exit_ok: bool
    wall_secs: float
    run_dir: Path


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
    """Run a headless-to-completion agentic Claude Code session for a single
    benchmark prompt.  `claude -p` drives the full agentic loop to completion
    without interactive input — this is the intended bare-arm semantics."""
    workdir.mkdir(parents=True, exist_ok=True)
    cmd = ["claude", *CLAUDE_FLAGS, instruction]
    # Ensure standard system paths are available so shebang interpreters
    # (e.g. /usr/bin/env bash) can be resolved inside subprocess scripts.
    # When a caller provides an env, preserve their PATH additions but still
    # guarantee the standard system dirs are present.
    effective_env = env.copy() if env is not None else os.environ.copy()
    for sysdir in ("/usr/bin", "/bin"):
        if sysdir not in effective_env.get("PATH", "").split(os.pathsep):
            effective_env["PATH"] = effective_env.get("PATH", "") + os.pathsep + sysdir
    t0 = time.time()
    proc = subprocess.run(cmd, cwd=str(workdir), timeout=timeout, env=effective_env)
    return RunResult(proc.returncode == 0, time.time() - t0, workdir)
