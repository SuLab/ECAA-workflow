"""Unit tests for the ChatServer lifecycle wrapper.

The free-port picker and chat-server.json metadata write are tested without a
real server. A real-start test runs only when `ecaa-workflow-server` is on PATH;
otherwise it is skipped (CI / dev boxes without the built binary).
"""
from __future__ import annotations
import json
import shutil
import socket

import pytest

from scripts.eval.services import chat_server
from scripts.eval.services.chat_server import ChatServer, _pick_free_port


def test_pick_free_port_returns_bindable_port():
    port = _pick_free_port()
    assert 1024 < port < 65536
    # The port is actually bindable right after being picked.
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        s.bind(("127.0.0.1", port))
    finally:
        s.close()


def test_base_url_before_start_raises(tmp_path):
    srv = ChatServer(tmp_path)
    with pytest.raises(chat_server.ChatServerError):
        _ = srv.base_url


def test_server_env_forces_git_off_and_drops_bind_addr(tmp_path):
    srv = ChatServer(tmp_path, env={
        "ECAA_GIT_ENABLED": "1",
        "ECAA_BIND_ADDR": "0.0.0.0",
        "ECAA_PACKAGE_ROOT": "/pkgs",
        "PATH": "/usr/bin",
    })
    env = srv._server_env()
    assert env["ECAA_GIT_ENABLED"] == "0"          # forced off
    assert "ECAA_BIND_ADDR" not in env             # dropped -> loopback default
    assert env["ECAA_PACKAGE_ROOT"] == "/pkgs"     # passed through
    assert env["ECAA_CONFIG_DIR"].endswith("/config")  # defaulted to repo config


def test_metadata_written_on_real_start(tmp_path):
    if shutil.which("ecaa-workflow-server") is None:
        pytest.skip("ecaa-workflow-server not on PATH")
    srv = ChatServer(tmp_path)
    try:
        srv.start()
        meta = json.loads((tmp_path / "chat-server.json").read_text())
        assert meta["port"] == srv.port
        assert meta["base_url"] == srv.base_url
        assert meta["pid"] == srv.proc.pid
    finally:
        srv.stop()
    # After stop the process is reaped.
    assert srv.proc is None


def test_stop_is_idempotent_without_start(tmp_path):
    srv = ChatServer(tmp_path)
    srv.stop()   # never started — must not raise
    srv.stop()
