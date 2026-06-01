"""Offline tests for the eval fault-injection shim wrappers.

These exercise scripts/eval/_eval_shim/{bwa,lofreq} directly via subprocess
with a STUB real tool placed on PATH AFTER the shim dir, so the shim's dynamic
real-bin resolution delegates to the stub. No live docker/containers/network.
"""
from __future__ import annotations
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

SHIM_DIR = Path(__file__).resolve().parents[1] / "_eval_shim"


def _write_stub(bin_dir: Path, tool: str, body: str) -> Path:
    """Write an executable stub `tool` into bin_dir."""
    bin_dir.mkdir(parents=True, exist_ok=True)
    p = bin_dir / tool
    p.write_text("#!/usr/bin/env bash\n" + body)
    p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
    return p


def _run_wrapper(tool, args, *, pattern, target, state_dir, stub_dir):
    """Invoke the shim WRAPPER (scripts/eval/_eval_shim/<tool>) with the stub
    real bin on PATH AFTER the shim dir, mirroring the in-container layout."""
    env = os.environ.copy()
    # shim dir FIRST (so the wrapper resolves), stub dir AFTER (so the shim's
    # _resolve_real skips its own dir and finds the stub).
    env["PATH"] = f"{SHIM_DIR}{os.pathsep}{stub_dir}{os.pathsep}" + env.get("PATH", "")
    env["EVAL_INJECT_PATTERN"] = pattern
    env["EVAL_INJECT_TARGET"] = target
    env["EVAL_INJECT_STATE"] = str(state_dir)
    return subprocess.run(
        [str(SHIM_DIR / tool), *args],
        env=env, capture_output=True, text=True,
    )


def test_wrappers_exist_and_executable():
    for tool in ("bwa", "lofreq"):
        w = SHIM_DIR / tool
        assert w.is_file(), f"missing wrapper {w}"
        assert os.access(w, os.X_OK), f"wrapper {w} not executable"


def test_invoked_marker_written_and_delegates_clean(tmp_path):
    """(a) the invoked.<tool> marker is written; (b) clean pattern delegates to
    the stub real bin (dynamic resolution)."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    sentinel = tmp_path / "stub_ran"
    _write_stub(stub, "bwa", f'echo ran > "{sentinel}"\nexit 0\n')

    r = _run_wrapper("bwa", ["mem", "ref.fa"], pattern="none", target="bwa",
                     state_dir=state, stub_dir=stub)
    assert r.returncode == 0, r.stderr
    assert (state / "invoked.bwa").exists()          # (a) bypass-detection marker
    assert sentinel.exists()                          # (b) stub really ran


def test_flake_first_call_fails_then_succeeds(tmp_path):
    """(c) flake_first_call exits 1 on the first call then succeeds."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    _write_stub(stub, "bwa", "exit 0\n")

    r1 = _run_wrapper("bwa", ["mem"], pattern="flake_first_call", target="bwa",
                      state_dir=state, stub_dir=stub)
    assert r1.returncode == 1

    r2 = _run_wrapper("bwa", ["mem"], pattern="flake_first_call", target="bwa",
                      state_dir=state, stub_dir=stub)
    assert r2.returncode == 0


def test_missing_lib_error_exits_127(tmp_path):
    """(d) missing_lib_error exits 127."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    _write_stub(stub, "lofreq", "exit 0\n")

    r = _run_wrapper("lofreq", ["call"], pattern="missing_lib_error",
                     target="lofreq", state_dir=state, stub_dir=stub)
    assert r.returncode == 127
    assert "shared libraries" in r.stderr


def test_silent_truncation_zeroes_output(tmp_path):
    """(e) silent_truncation zeroes the -o output file after delegation."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    out = tmp_path / "out.vcf"
    # Stub real lofreq writes a real VCF to -o; the shim then truncates it.
    _write_stub(stub, "lofreq",
                'while [ "$#" -gt 0 ]; do\n'
                '  if [ "$1" = "-o" ]; then shift; '
                'printf "##fileformat=VCFv4.2\\nchrM\\t152\\t.\\tT\\tC\\t.\\tPASS\\tAF=0.9\\n" > "$1"; fi\n'
                '  shift\n'
                'done\nexit 0\n')

    r = _run_wrapper("lofreq", ["call", "-o", str(out), "in.bam"],
                     pattern="silent_truncation", target="lofreq",
                     state_dir=state, stub_dir=stub)
    assert r.returncode == 0, r.stderr
    assert out.exists()
    assert out.read_text() == ""          # truncated to 0 bytes


def test_wrong_format_output_strips_to_header(tmp_path):
    """(f) wrong_format_output strips variant lines, leaving header-only."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    out = tmp_path / "out.vcf"
    _write_stub(stub, "lofreq",
                'while [ "$#" -gt 0 ]; do\n'
                '  if [ "$1" = "-o" ]; then shift; '
                'printf "##fileformat=VCFv4.2\\n#CHROM\\tPOS\\nchrM\\t152\\t.\\tT\\tC\\t.\\tPASS\\tAF=0.9\\n" > "$1"; fi\n'
                '  shift\n'
                'done\nexit 0\n')

    r = _run_wrapper("lofreq", ["call", "-o", str(out), "in.bam"],
                     pattern="wrong_format_output", target="lofreq",
                     state_dir=state, stub_dir=stub)
    assert r.returncode == 0, r.stderr
    content = out.read_text()
    assert content.strip() != ""
    # only header (#) lines survive
    for ln in content.splitlines():
        assert ln.startswith("#"), f"unexpected variant line survived: {ln!r}"


def test_non_target_tool_delegates_cleanly(tmp_path):
    """(g) a non-target tool delegates cleanly even under an active pattern
    (the pattern targets bwa; invoking lofreq must pass through untouched)."""
    state = tmp_path / "state"
    stub = tmp_path / "stub_bin"
    sentinel = tmp_path / "lofreq_ran"
    _write_stub(stub, "lofreq", f'echo ok > "{sentinel}"\nexit 0\n')

    r = _run_wrapper("lofreq", ["call"], pattern="missing_lib_error",
                     target="bwa", state_dir=state, stub_dir=stub)
    assert r.returncode == 0, r.stderr        # NOT 127: pattern targets bwa
    assert sentinel.exists()                   # lofreq delegated to its stub
    assert (state / "invoked.lofreq").exists() # marker still written


# --- env-forward / wrapper-plumbing static assertions (offline, no docker) ---

REPO = Path(__file__).resolve().parents[3]
AGENT_CLAUDE = REPO / "scripts" / "agent-claude.sh"
BARE_AGENT = REPO / "scripts" / "eval" / "_bare_agent.sh"


def _text(p: Path) -> str:
    return p.read_text()


def test_bash_syntax_ok_both_scripts():
    for script in (AGENT_CLAUDE, BARE_AGENT):
        r = subprocess.run(["bash", "-n", str(script)],
                           capture_output=True, text=True)
        assert r.returncode == 0, f"bash -n failed on {script}: {r.stderr}"


def test_bare_agent_has_gated_shim_plumbing():
    t = _text(BARE_AGENT)
    assert "ECAA_EVAL_SHIM_DIR" in t
    # gated on the env var being set
    assert 'if [ -n "${ECAA_EVAL_SHIM_DIR:-}" ]' in t
    # ro shim mount + rw state mount
    assert '"$ECAA_EVAL_SHIM_DIR":"$ECAA_EVAL_SHIM_DIR":ro' in t
    assert '"$EVAL_INJECT_STATE":"$EVAL_INJECT_STATE":rw' in t
    # EVAL_INJECT_* pass-through
    assert "EVAL_INJECT_PATTERN" in t
    assert "EVAL_INJECT_TARGET" in t
    assert "EVAL_INJECT_STATE" in t


def test_agent_claude_has_gated_shim_plumbing():
    t = _text(AGENT_CLAUDE)
    assert "ECAA_EVAL_SHIM_DIR" in t
    assert 'if [ -n "${ECAA_EVAL_SHIM_DIR:-}" ]' in t
    # a dedicated conditional DOCKER_SHIM_ARGS array
    assert "DOCKER_SHIM_ARGS" in t
    # ro shim mount + rw state mount somewhere in the shim block
    assert '"$ECAA_EVAL_SHIM_DIR":"$ECAA_EVAL_SHIM_DIR":ro' in t
    assert '"$EVAL_INJECT_STATE":"$EVAL_INJECT_STATE":rw' in t
    # EVAL_INJECT_* forwarded
    assert "EVAL_INJECT_PATTERN" in t
    assert "EVAL_INJECT_TARGET" in t


if __name__ == "__main__":
    sys.exit(pytest.main([__file__, "-q"]))
