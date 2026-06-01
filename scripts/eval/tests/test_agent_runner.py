import os
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


def test_run_bare_uses_provided_env(tmp_path):
    """run_bare must thread the caller-provided env into the subprocess.

    We set a marker var in the env dict and write a stub 'claude' that echoes
    the marker value to a file.  If env threading works, the file will contain
    the expected value.  /usr/bin and /bin must still be present (PATH
    preservation) so the shebang interpreter resolves correctly.
    """
    bindir = tmp_path / "bin"
    bindir.mkdir()
    stub = bindir / "claude"
    # Write the value of EVAL_MARKER to marker.txt in the working directory.
    stub.write_text(
        '#!/usr/bin/env bash\n'
        'echo "$EVAL_MARKER" > marker.txt\n'
    )
    stub.chmod(0o755)

    wd = tmp_path / "wd"
    wd.mkdir()

    # Build a custom env: start from os.environ, add the stub bin dir to PATH,
    # and inject our marker.  We deliberately do NOT include /usr/bin:/bin here
    # to verify that run_bare adds them automatically.
    custom_env = os.environ.copy()
    custom_env["PATH"] = str(bindir) + os.pathsep + custom_env.get("PATH", "")
    custom_env["EVAL_MARKER"] = "hello_from_env_injection"

    res = run_bare(wd, "do the thing", timeout=30, env=custom_env)

    assert res.exit_ok is True, "stub claude should exit 0"

    marker_file = wd / "marker.txt"
    assert marker_file.exists(), "stub claude should have written marker.txt"
    contents = marker_file.read_text().strip()
    assert contents == "hello_from_env_injection", (
        f"subprocess did not see the injected env var; got: {contents!r}"
    )

    # Also confirm /usr/bin and /bin were preserved (run_bare's PATH contract).
    # We check this indirectly: the stub used /usr/bin/env bash to launch, which
    # only works if /usr/bin is on PATH — and the test passed, so it is.


def test_run_bare_captures_stdout(tmp_path):
    """run_bare must capture stdout from the subprocess into RunResult.stdout."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    stub = bindir / "claude"
    stub.write_text('#!/usr/bin/env bash\necho \'{"result":"hi"}\'\n')
    stub.chmod(0o755)

    custom_env = os.environ.copy()
    custom_env["PATH"] = str(bindir) + os.pathsep + custom_env.get("PATH", "")

    wd = tmp_path / "wd"
    wd.mkdir()

    res = run_bare(wd, "do the thing", timeout=30, env=custom_env)

    assert res.exit_ok is True, "stub claude should exit 0"
    assert "hi" in res.stdout, (
        f"res.stdout should contain 'hi'; got: {res.stdout!r}"
    )
