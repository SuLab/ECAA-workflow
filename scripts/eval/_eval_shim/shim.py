#!/usr/bin/env python3
"""Eval-owned fault-injection shim — container-safe (mounted into the agent
container and placed FIRST on PATH by the agent wrappers when ECAA_EVAL_SHIM_DIR
is set).

Unlike the paper's host shim, this:
  * records every invocation to EVAL_INJECT_STATE/invoked.<tool> so the harness
    can DETECT A BYPASS (the agent used an absolute path / conda-activated a bin
    ahead of the shim / used a different tool) and mark the cell inconclusive
    instead of scoring an un-injected run as a recovery;
  * resolves the REAL tool DYNAMICALLY from PATH (skipping this shim's own dir),
    so it delegates to whatever the agent installed (conda/pip/apt/source) —
    no fixed EVAL_REAL_BIN_DIR needed.

Invoked as:  shim.py <tool> <real-args...>
Contract env: EVAL_INJECT_PATTERN, EVAL_INJECT_TARGET, EVAL_INJECT_STATE.
"""
from __future__ import annotations
import os
import subprocess
import sys
import time
from pathlib import Path

SLOW_SECONDS = int(os.environ.get("EVAL_SLOW_SECONDS", "30"))
NOISE_LINES = 200
INJECT_TARGET_SAMPLE = "M117C1-ch"


def _resolve_real(tool: str):
    """Real tool on PATH, skipping this shim's dir so we delegate to whatever the
    agent actually installed. Falls back to EVAL_REAL_BIN_DIR if set."""
    self_dir = os.path.realpath(str(Path(__file__).resolve().parent))
    for d in os.environ.get("PATH", "").split(os.pathsep):
        if not d or os.path.realpath(d) == self_dir:
            continue
        cand = Path(d) / tool
        if cand.is_file() and os.access(cand, os.X_OK):
            return str(cand)
    rb = os.environ.get("EVAL_REAL_BIN_DIR", "")
    if rb and (Path(rb) / tool).exists():
        return str(Path(rb) / tool)
    return None


def _output_vcf(args):
    for i, a in enumerate(args):
        if a in ("-o", "--out") and i + 1 < len(args):
            return Path(args[i + 1])
        if a.startswith("--out="):
            return Path(a.split("=", 1)[1])
    return None


def _delegate(real, tool, args) -> int:
    if real is None:
        sys.stderr.write(f"shim: real {tool} not found on PATH\n")
        return 127
    return subprocess.call([real, *args])


def main() -> int:
    if len(sys.argv) < 2:
        return 2
    tool, args = sys.argv[1], sys.argv[2:]
    pattern = os.environ.get("EVAL_INJECT_PATTERN", "none")
    target = os.environ.get("EVAL_INJECT_TARGET", "")
    state = Path(os.environ.get("EVAL_INJECT_STATE", "/tmp/eval-shim-state"))
    try:
        state.mkdir(parents=True, exist_ok=True)
        (state / f"invoked.{tool}").write_text("1")  # bypass-detection marker
    except OSError:
        pass

    real = _resolve_real(tool)
    if pattern in ("", "none") or target != tool:
        return _delegate(real, tool, args)

    if pattern == "missing_lib_error":
        sys.stderr.write(f"{tool}: error while loading shared libraries: libfoo.so.1\n")
        return 127
    if pattern == "flake_first_call":
        c = state / f"calls.{tool}"
        n = int(c.read_text()) if c.exists() else 0
        c.write_text(str(n + 1))
        if n == 0:
            sys.stderr.write(f"{tool}: transient failure (flake_first_call)\n")
            return 1
        return _delegate(real, tool, args)
    if pattern == "one_sample_fails":
        if any(INJECT_TARGET_SAMPLE in a for a in args):
            sys.stderr.write(f"{tool}: failing sample {INJECT_TARGET_SAMPLE}\n")
            return 1
        return _delegate(real, tool, args)
    if pattern == "slow_tool":
        time.sleep(SLOW_SECONDS)
        return _delegate(real, tool, args)
    if pattern == "stderr_warning_storm":
        for _ in range(NOISE_LINES):
            sys.stderr.write("WARNING: noisy but harmless\n")
        return _delegate(real, tool, args)
    if pattern == "silent_truncation":
        rc = _delegate(real, tool, args)
        out = _output_vcf(args)
        if out and out.exists():
            out.write_text("")  # truncate to 0 bytes
        return rc
    if pattern == "wrong_format_output":
        rc = _delegate(real, tool, args)
        out = _output_vcf(args)
        if out and out.exists():
            kept = [ln for ln in out.read_text().splitlines() if ln.startswith("#")]
            out.write_text(("\n".join(kept) + "\n") if kept else "")  # header-only
        return rc
    return _delegate(real, tool, args)


if __name__ == "__main__":
    sys.exit(main())
