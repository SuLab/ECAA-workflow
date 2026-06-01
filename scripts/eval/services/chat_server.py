"""One shared chat server per eval run.

The ECAA arm drives the production SME chat-intake path (compiler-as-service)
instead of the no-LLM `ecaa-workflow intake` CLI. That path lives behind the
Axum server (`ecaa-workflow-server`), so an eval run that uses chat-intake
needs exactly one server process hosting every concurrent session.

A single shared server is the production topology (the web UI is one server,
many sessions); sessions are independent disk-persisted objects keyed by UUID,
and `/turn` is rate-limited per-session (not globally), so parallel workers on
distinct sessions don't contend. Per-worker servers would multiply port picks,
readiness waits, and `ECAA_PACKAGE_ROOT` ownership, and would fragment the
single `--server-url` every harness posts progress back to.
"""
from __future__ import annotations
import fcntl
import json
import os
import signal
import socket
import subprocess
import time
from pathlib import Path

import requests

REPO_ROOT = Path(__file__).resolve().parents[3]

# Readiness + teardown budgets (seconds).
_READY_DEADLINE = 60.0
_READY_POLL = 0.25
_STOP_GRACE = 10.0


def _pick_free_port() -> int:
    """Bind a throwaway socket to 127.0.0.1:0, read the kernel-assigned port,
    close it, and return the port. Avoids a race with a hardcoded 3000/3737.

    There is a small TOCTOU window between close and the server re-binding the
    port, but the eval starts exactly one server per run and immediately binds,
    so the window is negligible in practice."""
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]
    finally:
        s.close()


class ChatServerError(RuntimeError):
    """Raised when the server fails to start or become ready."""


class ChatServer:
    """Owns the single `ecaa-workflow-server` subprocess for an eval run.

    Acquire a per-run-dir flock before starting so a `--resume` invocation in
    the same run dir doesn't spawn a second server. Always (re)start a fresh
    server — sessions persist on disk via ECAA_CHAT_SESSIONS_DIR, so old session
    ids remain loadable across restarts; the eval never reattaches to a prior
    process."""

    def __init__(self, run_dir: Path, *, env: dict | None = None):
        self.run_dir = Path(run_dir)
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self._base_env = dict(env) if env is not None else os.environ.copy()
        self.port: int | None = None
        self.proc: subprocess.Popen | None = None
        self._lock_fh = None
        self._lock_path = self.run_dir / ".chat-server.lock"
        self._meta_path = self.run_dir / "chat-server.json"

    @property
    def base_url(self) -> str:
        if self.port is None:
            raise ChatServerError("ChatServer not started")
        return f"http://127.0.0.1:{self.port}"

    def _server_env(self) -> dict:
        """Server subprocess env. Forces git off (no provenance commits during
        eval), pins config to the repo, and passes through the package/session
        dirs + API key so the harness, agent, and eval resolve identical paths.
        ECAA_BIND_ADDR is left UNSET so the server defaults to loopback and
        needs no auth token."""
        env = self._base_env.copy()
        env["ECAA_GIT_ENABLED"] = "0"
        env.setdefault("ECAA_CONFIG_DIR", str(REPO_ROOT / "config"))
        # ECAA_CHAT_SESSIONS_DIR / ECAA_PACKAGE_ROOT / ECAA_ANTHROPIC_API_KEY
        # pass through from _base_env unchanged when already set.
        env.pop("ECAA_BIND_ADDR", None)
        return env

    def start(self) -> "ChatServer":
        # Single-instance-per-run-dir guard. Non-blocking exclusive flock; if a
        # sibling process already holds it (e.g. a concurrent resume), refuse
        # rather than spawn a second server.
        self._lock_fh = self._lock_path.open("w")
        try:
            fcntl.flock(self._lock_fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError as e:
            self._lock_fh.close()
            self._lock_fh = None
            raise ChatServerError(
                f"another chat server holds {self._lock_path}: {e}") from e

        self.port = _pick_free_port()
        cmd = ["ecaa-workflow-server", "--port", str(self.port)]
        self.proc = subprocess.Popen(  # noqa: S603 — fixed argv, no shell
            cmd, cwd=str(REPO_ROOT), env=self._server_env(),
            start_new_session=True,
        )
        try:
            self._wait_ready()
        except ChatServerError:
            self.stop()
            raise

        self._meta_path.write_text(json.dumps(
            {"pid": self.proc.pid, "port": self.port, "base_url": self.base_url},
            indent=2))
        return self

    def _wait_ready(self) -> None:
        """Poll GET /healthz (always-200, unauthenticated) until ready or the
        deadline elapses. Fail loudly if the process died early."""
        deadline = time.time() + _READY_DEADLINE
        url = f"{self.base_url}/healthz"
        while time.time() < deadline:
            if self.proc is not None and self.proc.poll() is not None:
                raise ChatServerError(
                    f"server exited early (code {self.proc.returncode}) "
                    f"before /healthz became ready")
            try:
                r = requests.get(url, timeout=2)
                if r.status_code == 200:
                    return
            except requests.RequestException:
                pass
            time.sleep(_READY_POLL)
        raise ChatServerError(
            f"server not ready at {url} within {_READY_DEADLINE:.0f}s")

    def stop(self) -> None:
        """SIGTERM the server's process group, wait up to 10s, then SIGKILL.
        Idempotent; safe to call from a finally block even if start() failed."""
        proc = self.proc
        if proc is not None and proc.poll() is None:
            try:
                os.killpg(os.getpgid(proc.pid), signal.SIGTERM)
            except (ProcessLookupError, PermissionError):
                try:
                    proc.terminate()
                except ProcessLookupError:
                    pass
            try:
                proc.wait(timeout=_STOP_GRACE)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(os.getpgid(proc.pid), signal.SIGKILL)
                except (ProcessLookupError, PermissionError):
                    try:
                        proc.kill()
                    except ProcessLookupError:
                        pass
                try:
                    proc.wait(timeout=_STOP_GRACE)
                except subprocess.TimeoutExpired:
                    pass
        self.proc = None
        if self._lock_fh is not None:
            try:
                fcntl.flock(self._lock_fh.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
            self._lock_fh.close()
            self._lock_fh = None

    def __enter__(self) -> "ChatServer":
        return self.start()

    def __exit__(self, *exc) -> None:
        self.stop()
