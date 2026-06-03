"""Unit tests for chat_client.drive_chat_intake against a scripted HTTP stub.

A real threaded http.server serves the chat REST contract with a programmable
state script, so the client's turn-loop, nudge-after-turn-1, sme-named POST,
/confirm, 429-retry, blocked, and budget-exhaustion paths are all exercised
without a live ecaa-workflow-server.
"""
from __future__ import annotations
import json
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest

from scripts.eval.services import chat_client
from scripts.eval.services.chat_client import ChatIntakeError, drive_chat_intake


class _Script:
    """Programmable server behavior shared with the request handler."""
    def __init__(self, *, state_kinds, pkg_path="/emitted/pkg",
                 turn_429_first=False, proposals=None):
        # state_kinds is the sequence returned by successive GET /state calls.
        self.state_kinds = list(state_kinds)
        self.pkg_path = pkg_path
        self.turn_429_first = turn_429_first
        # Proposals served by GET /proposals; signoff/reject flip lifecycle.
        self.proposals = [dict(p) for p in (proposals or [])]
        # /metrics body: {} (default) serves 200 + empty dict; None serves 404.
        self.metrics_body: dict | None = {}
        # Captured for assertions.
        self.turn_messages: list[str] = []
        self.sme_named_calls: list[str] = []
        self.signoff_calls: list[str] = []
        self.reject_calls: list[str] = []
        self.confirm_calls = 0
        self.idempotency_keys: list[str] = []
        self._state_idx = 0
        self._turn_count = 0

    def _set_lifecycle(self, pid, kind):
        for p in self.proposals:
            if p.get("id") == pid:
                p["lifecycle"] = {"kind": kind}

    def next_state(self):
        idx = min(self._state_idx, len(self.state_kinds) - 1)
        kind = self.state_kinds[idx]
        self._state_idx += 1
        body = {"session_id": "sid-123", "state": {"kind": kind}}
        if kind == "emitted":
            body["emitted_package_path"] = self.pkg_path
        return body


def _make_handler_capturing(script: _Script):
    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *a):
            pass

        def _send_json(self, code, obj):
            payload = json.dumps(obj).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

        def _send_empty(self, code):
            self.send_response(code)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def _read_body(self):
            n = int(self.headers.get("Content-Length", 0) or 0)
            raw = self.rfile.read(n) if n else b""
            try:
                return json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                return {}

        def do_POST(self):
            path = self.path
            body = self._read_body()
            if path == "/api/chat/session":
                self._send_json(200, {"session_id": "sid-123", "greeting": {}})
            elif path.endswith("/turn"):
                script._turn_count += 1
                if script.turn_429_first and script._turn_count == 1:
                    self._send_empty(429)
                    return
                script.turn_messages.append(body.get("message", ""))
                self._send_json(200, {})
            elif "/intake-method/" in path and path.endswith("/sme-named"):
                parts = path.split("/")
                stage = parts[parts.index("intake-method") + 1]
                script.sme_named_calls.append(stage)
                self._send_empty(204)
            elif path.endswith("/confirm"):
                script.confirm_calls += 1
                key = self.headers.get("Idempotency-Key")
                if key:
                    script.idempotency_keys.append(key)
                self._send_empty(204)
            elif "/proposal/" in path and path.endswith("/signoff"):
                pid = path.split("/proposal/")[1].split("/")[0]
                script.signoff_calls.append(pid)
                script._set_lifecycle(pid, "promoted")
                self._send_empty(204)
            elif "/proposal/" in path and path.endswith("/reject"):
                pid = path.split("/proposal/")[1].split("/")[0]
                script.reject_calls.append(pid)
                script._set_lifecycle(pid, "rejected")
                self._send_empty(204)
            else:
                self._send_empty(404)

        def do_GET(self):
            if self.path.endswith("/metrics"):
                body = getattr(script, "metrics_body", {})
                if body is None:
                    self._send_empty(404)
                else:
                    self._send_json(200, body)
            elif self.path.endswith("/state"):
                self._send_json(200, script.next_state())
            elif self.path.endswith("/proposals"):
                self._send_json(200, script.proposals)
            else:
                self._send_empty(404)

    return Handler


def _serve(script: _Script):
    httpd = ThreadingHTTPServer(("127.0.0.1", 0), _make_handler_capturing(script))
    thread = threading.Thread(target=httpd.serve_forever, daemon=True)
    thread.start()
    base = f"http://127.0.0.1:{httpd.server_address[1]}"
    return httpd, base


@pytest.fixture(autouse=True)
def _fast_retries(monkeypatch):
    """Collapse sleeps so retry/429/poll paths don't slow the suite."""
    monkeypatch.setattr(chat_client, "_RETRY_BACKOFF", [0, 0, 0])
    monkeypatch.setattr(chat_client, "_RATE_LIMIT_SLEEP", 0)
    monkeypatch.setattr(chat_client, "_EMIT_POLL_INTERVAL", 0)
    monkeypatch.setattr(chat_client.time, "sleep", lambda *_: None)


def test_happy_path_two_turns_then_confirm():
    # Turn 1 -> still intake; turn 2 (nudge) -> pending_confirmation; then the
    # confirm-poll GET returns emitted with the package path.
    script = _Script(state_kinds=[
        "intake_followup",       # after turn 1
        "pending_confirmation",  # after turn 2 (nudge)
        "emitted",               # confirm poll
    ])
    httpd, base = _serve(script)
    try:
        sid, pkg = drive_chat_intake(base, "Do an analysis", intake_turn_budget=8)
    finally:
        httpd.shutdown()
    assert sid == "sid-123"
    assert str(pkg) == "/emitted/pkg"
    # Exactly 2 turns sent; the first is the full instruction, the second is the
    # method-free nudge.
    assert len(script.turn_messages) == 2
    assert script.turn_messages[0] == "Do an analysis"
    assert script.turn_messages[1] == chat_client._NUDGE
    assert script.confirm_calls == 1
    assert len(script.idempotency_keys) == 1  # confirm carried an Idempotency-Key
    assert script.sme_named_calls == []        # nothing locked


def test_locked_methods_post_sme_named_before_first_turn():
    script = _Script(state_kinds=["pending_confirmation", "emitted"])
    httpd, base = _serve(script)
    try:
        sid, pkg = drive_chat_intake(
            base, "Call variants",
            locked_methods=[("alignment", "bwa"), ("variant_calling", "lofreq")])
    finally:
        httpd.shutdown()
    assert sid == "sid-123"
    # Both stages flagged via sme-named, BEFORE the first turn was sent.
    assert script.sme_named_calls == ["alignment", "variant_calling"]
    # The first turn names the locked methods so the LLM is permitted to lock.
    assert "bwa for alignment" in script.turn_messages[0]
    assert "lofreq for variant_calling" in script.turn_messages[0]


def test_signoff_policy_resolves_pending_proposal():
    # The LLM proposes a hypothesized node on turn 1 (awaiting_signoff), which
    # blocks propose_summary_confirmation server-side. With proposal_policy=
    # "signoff" the driver must POST .../signoff to clear the gate so intake
    # can converge to pending_confirmation, then emit.
    script = _Script(
        state_kinds=["intake_followup", "pending_confirmation", "emitted"],
        proposals=[{"id": "proposal-1", "node_id": "cross_sample_variant_table",
                    "lifecycle": {"kind": "awaiting_signoff"}}],
    )
    httpd, base = _serve(script)
    try:
        sid, pkg = drive_chat_intake(base, "Call variants",
                                     proposal_policy="signoff",
                                     intake_turn_budget=8)
    finally:
        httpd.shutdown()
    assert str(pkg) == "/emitted/pkg"
    assert script.signoff_calls == ["proposal-1"]
    assert script.reject_calls == []


def test_reject_policy_resolves_pending_proposal():
    # proposal_policy="reject" declines the hypothesized node (recipe fidelity):
    # the gate clears via .../reject, NOT .../signoff.
    script = _Script(
        state_kinds=["intake_followup", "pending_confirmation", "emitted"],
        proposals=[{"id": "proposal-7", "node_id": "cross_sample_variant_table",
                    "lifecycle": {"kind": "awaiting_signoff"}}],
    )
    httpd, base = _serve(script)
    try:
        drive_chat_intake(base, "Call variants", proposal_policy="reject",
                          intake_turn_budget=8)
    finally:
        httpd.shutdown()
    assert script.reject_calls == ["proposal-7"]
    assert script.signoff_calls == []


def test_signoff_policy_rejects_gate_blocked_proposal():
    # Under a strict sandbox bundle (clinical_trial/phi_strict) a node proposal
    # can land in lifecycle "blocked" — unpromotable, so signoff would 409. The
    # gate (is_pending_sme covers Blocked) stays closed, so the signoff-policy
    # driver must REJECT a blocked proposal to clear it; skipping it deadlocks.
    script = _Script(
        state_kinds=["intake_followup", "pending_confirmation", "emitted"],
        proposals=[{"id": "proposal-b", "node_id": "x",
                    "lifecycle": {"kind": "blocked"}}],
    )
    httpd, base = _serve(script)
    try:
        drive_chat_intake(base, "instr", proposal_policy="signoff",
                          intake_turn_budget=8)
    finally:
        httpd.shutdown()
    assert script.reject_calls == ["proposal-b"]
    assert script.signoff_calls == []


def test_default_policy_is_reject():
    # No explicit policy → reject (a plugin without an override must not silently
    # expand its emitted DAG with unvetted hypothesized nodes).
    script = _Script(
        state_kinds=["intake_followup", "pending_confirmation", "emitted"],
        proposals=[{"id": "proposal-9",
                    "lifecycle": {"kind": "awaiting_signoff"}}],
    )
    httpd, base = _serve(script)
    try:
        drive_chat_intake(base, "instr", intake_turn_budget=8)
    finally:
        httpd.shutdown()
    assert script.reject_calls == ["proposal-9"]
    assert script.signoff_calls == []


def test_invalid_proposal_policy_raises():
    with pytest.raises(ChatIntakeError, match="unknown proposal_policy"):
        drive_chat_intake("http://127.0.0.1:1", "instr", proposal_policy="maybe")


def test_already_emitted_skips_confirm():
    # If intake auto-advanced straight to emitted, no confirm should be sent.
    script = _Script(state_kinds=["emitted"])
    httpd, base = _serve(script)
    try:
        sid, pkg = drive_chat_intake(base, "instr")
    finally:
        httpd.shutdown()
    assert str(pkg) == "/emitted/pkg"
    assert script.confirm_calls == 0


def test_blocked_state_raises():
    script = _Script(state_kinds=["blocked"])
    httpd, base = _serve(script)
    try:
        with pytest.raises(ChatIntakeError, match="blocked"):
            drive_chat_intake(base, "instr")
    finally:
        httpd.shutdown()


def test_budget_exhaustion_raises():
    # Never reaches a ready kind within the budget.
    script = _Script(state_kinds=["intake_followup"])
    httpd, base = _serve(script)
    try:
        with pytest.raises(ChatIntakeError, match="did not converge"):
            drive_chat_intake(base, "instr", intake_turn_budget=3)
    finally:
        httpd.shutdown()
    # Budget honored: exactly 3 turns attempted.
    assert len(script.turn_messages) == 3


def test_429_retry_does_not_consume_budget():
    # First turn POST returns 429 (retried), then 200; one GET -> pending_conf;
    # confirm poll -> emitted. The 429 must NOT count against the turn budget.
    script = _Script(state_kinds=["pending_confirmation", "emitted"],
                     turn_429_first=True)
    httpd, base = _serve(script)
    try:
        sid, pkg = drive_chat_intake(base, "instr", intake_turn_budget=2)
    finally:
        httpd.shutdown()
    assert str(pkg) == "/emitted/pkg"
    # Only ONE successful turn recorded despite the 429 retry.
    assert len(script.turn_messages) == 1


def test_drive_with_metrics_harvests_metrics_snapshot():
    # After emitted, the client GETs /metrics and returns it as a third element.
    script = _Script(state_kinds=["pending_confirmation", "emitted"])
    script.metrics_body = {
        "followup_count": 2,
        "time_to_emit_ms": 4200,
        "task_success_rate": None,
        "method_recommendation_requests": 1,
        "is_ambiguous": False,
        "blockers_encountered": [],
        "affordance_fallbacks": [{"semantic_type": "data:2603", "primitive": "scatter", "count": 3}],
        "coverage_gap_events": 1,
    }
    httpd, base = _serve(script)
    try:
        from scripts.eval.services.chat_client import drive_chat_intake_with_metrics
        sid, pkg, metrics = drive_chat_intake_with_metrics(base, "Do an analysis")
    finally:
        httpd.shutdown()
    assert sid == "sid-123"
    assert str(pkg) == "/emitted/pkg"
    assert metrics["followup_count"] == 2
    assert metrics["time_to_emit_ms"] == 4200
    assert metrics["coverage_gap_events"] == 1


def test_drive_with_metrics_tolerates_missing_metrics_endpoint():
    # A 404 / null /metrics (fresh session, or older server) yields {} not a raise.
    script = _Script(state_kinds=["emitted"])
    script.metrics_body = None  # signals: respond 404 to /metrics
    httpd, base = _serve(script)
    try:
        from scripts.eval.services.chat_client import drive_chat_intake_with_metrics
        sid, pkg, metrics = drive_chat_intake_with_metrics(base, "instr")
    finally:
        httpd.shutdown()
    assert metrics == {}


def test_drive_chat_intake_still_returns_two_tuple():
    # The legacy 2-tuple entry point keeps working (callers not yet migrated).
    script = _Script(state_kinds=["emitted"])
    httpd, base = _serve(script)
    try:
        result = drive_chat_intake(base, "instr")
    finally:
        httpd.shutdown()
    assert isinstance(result, tuple) and len(result) == 2
    sid, pkg = result
    assert str(pkg) == "/emitted/pkg"
