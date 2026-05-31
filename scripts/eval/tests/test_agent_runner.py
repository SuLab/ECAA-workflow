import shutil, subprocess
from pathlib import Path
import pytest
from scripts.eval.services.agent_runner import run_bare

def test_run_bare_executes_command(tmp_path, monkeypatch):
    # Point the runner at a stub "claude" on PATH that just writes files.
    bindir = tmp_path / "bin"; bindir.mkdir()
    stub = bindir / "claude"
    stub.write_text('#!/usr/bin/env bash\necho "ran in $(pwd)" > trace.md\n')
    stub.chmod(0o755)
    monkeypatch.setenv("PATH", f"{bindir}:{tmp_path}")
    wd = tmp_path / "wd"; wd.mkdir()
    res = run_bare(wd, "do the thing", timeout=30)
    assert res.exit_ok is True
    assert (wd / "trace.md").exists()
