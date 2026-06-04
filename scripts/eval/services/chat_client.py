"""Drive the production SME chat-intake REST contract for the ECAA arm.

This is the ONLY module that talks chat HTTP. It encapsulates the exact
sequence a human SME's browser would drive: create a session, send the project
description, loop intake turns until the LLM proposes the confirmation summary,
click confirm (which runs auto-emit synchronously), then read the
server-assigned `emitted_package_path`.

Method-neutrality: the deterministic nudge sent after the first turn never
names a method, so the LLM won't lock an aligner / caller / test unless the
eval pre-sets the SME-named flag for that stage (the `locked_methods`
argument). For recipe benchmarks (Nekrutenko) we DO lock; for open benchmarks
(BiomniBench) we leave methods free so the execution agent chooses at runtime.
"""
from __future__ import annotations
import time
import uuid
from pathlib import Path

import requests

# Turn / confirm / poll budgets (seconds).
_CREATE_TIMEOUT = 30
_TURN_TIMEOUT = 180
_STATE_TIMEOUT = 30
# /confirm runs try_auto_emit_after_confirm SYNCHRONOUSLY and only returns 204
# AFTER emit completes. With ECAA_VALIDATE_ON_EMIT=full that emit runs the
# external runcrate + OWL-DL validators (ECAA_VALIDATION_EXTERNAL_TIMEOUT_SECS,
# default 180s, possibly several validators), so a real confirm can take 3+ min.
# A short timeout here makes _post time out and RETRY the confirm; the retry then
# lands on the now-Emitting session and 400s ("illegal transition ... from
# Emitting"), failing the whole ECAA base run before the harness ever runs. Size
# the budget above worst-case synchronous emit.
_CONFIRM_TIMEOUT = 600
_SME_NAMED_TIMEOUT = 30
_METRICS_TIMEOUT = 30

# Post-confirm poll must also cover a full synchronous emit: if confirm was a
# retry that fell through (session already Emitting), emit may still be running
# when we start polling for `emitted`.
_EMIT_POLL_DEADLINE = 600.0
_EMIT_POLL_INTERVAL = 0.5

# Connection/5xx retry backoff schedule (seconds).
_RETRY_BACKOFF = [0.5, 2, 5]
# 429 (per-session rate-limit) cooldown before retrying the SAME turn.
_RATE_LIMIT_SLEEP = 2.0

# Deterministic nudge sent on every turn after the first. Drives the LLM to
# call propose_summary_confirmation without naming any method.
_NUDGE = (
    "That's the complete description. Please summarize the plan and ask me to "
    "confirm so we can proceed."
)

# Terminal kinds that mean intake converged enough to stop looping.
_READY_KINDS = {"pending_confirmation", "ready_to_emit", "emitting", "emitted"}

# Per-benchmark policy for hypothesized-node proposals the LLM raises during
# intake (the v4 composer's gap-fill). "signoff" approves (promotes into the
# DAG); "reject" declines (keeps the composed plan as-is).
_PROPOSAL_POLICIES = {"signoff", "reject"}
# Proposal lifecycle kinds that need no SME action (already decided).
_TERMINAL_PROPOSAL_KINDS = {"promoted", "rejected"}


class ChatIntakeError(Exception):
    """Any unrecoverable failure driving chat intake. The eval treats this
    exactly like a failed base run: journal it as a failure so --resume retries."""


def _post(base_url: str, path: str, *, json_body=None, headers=None,
          timeout: int = _STATE_TIMEOUT) -> requests.Response:
    """POST with timeout + bounded retry on connection errors / 5xx. 429 is NOT
    retried here — the caller handles it (it must re-send the same turn after a
    cooldown, not consume a budget iteration)."""
    url = base_url.rstrip("/") + path
    last_exc: Exception | None = None
    for attempt in range(len(_RETRY_BACKOFF) + 1):
        try:
            r = requests.post(url, json=json_body, headers=headers, timeout=timeout)
        except requests.RequestException as e:
            last_exc = e
        else:
            if r.status_code < 500:
                return r
            last_exc = ChatIntakeError(f"POST {path} -> {r.status_code}: {r.text[:300]}")
        if attempt < len(_RETRY_BACKOFF):
            time.sleep(_RETRY_BACKOFF[attempt])
    raise ChatIntakeError(f"POST {path} failed after retries: {last_exc}")


def _get(base_url: str, path: str, *, timeout: int = _STATE_TIMEOUT) -> requests.Response:
    """GET with timeout + bounded retry on connection errors / 5xx."""
    url = base_url.rstrip("/") + path
    last_exc: Exception | None = None
    for attempt in range(len(_RETRY_BACKOFF) + 1):
        try:
            r = requests.get(url, timeout=timeout)
        except requests.RequestException as e:
            last_exc = e
        else:
            if r.status_code < 500:
                return r
            last_exc = ChatIntakeError(f"GET {path} -> {r.status_code}: {r.text[:300]}")
        if attempt < len(_RETRY_BACKOFF):
            time.sleep(_RETRY_BACKOFF[attempt])
    raise ChatIntakeError(f"GET {path} failed after retries: {last_exc}")


def _state_kind(base_url: str, sid: str) -> tuple[str, dict]:
    """Return (kind, full_state_dict) for the session."""
    r = _get(base_url, f"/api/chat/session/{sid}/state", timeout=_STATE_TIMEOUT)
    if r.status_code != 200:
        raise ChatIntakeError(f"GET /state -> {r.status_code}: {r.text[:300]}")
    state = r.json()
    kind = (state.get("state") or {}).get("kind", "")
    return kind, state


def _harvest_metrics(base_url: str, sid: str) -> dict:
    """GET the per-session metrics snapshot. Best-effort: a 404 (older server),
    a 200-null (fresh session), or any non-dict body yields {}. NEVER raises —
    a missing snapshot must not fail an otherwise-successful intake. Reads keys
    positionally only via .get on the CALLER side; this just returns the raw dict."""
    try:
        r = _get(base_url, f"/api/chat/session/{sid}/metrics", timeout=_METRICS_TIMEOUT)
    except ChatIntakeError:
        return {}
    if r.status_code != 200:
        return {}
    try:
        body = r.json()
    except ValueError:
        return {}
    return body if isinstance(body, dict) else {}


def _send_turn(base_url: str, sid: str, message: str) -> None:
    """Send one intake turn. A fresh user_turn_id per call makes the turn
    idempotent server-side. On HTTP 429 (per-session rate-limit) sleep and
    re-send the SAME message until it is accepted — this does NOT consume an
    intake-budget iteration (the caller's loop only advances on a 2xx turn)."""
    while True:
        r = _post(base_url, f"/api/chat/session/{sid}/turn",
                  json_body={"message": message, "user_turn_id": str(uuid.uuid4())},
                  timeout=_TURN_TIMEOUT)
        if r.status_code == 429:
            time.sleep(_RATE_LIMIT_SLEEP)
            continue
        if r.status_code != 200:
            raise ChatIntakeError(f"POST /turn -> {r.status_code}: {r.text[:300]}")
        return


def _resolve_pending_proposals(base_url: str, sid: str, policy: str) -> int:
    """Approve (signoff) or reject every proposal awaiting SME action.

    The v4 composer's gap-detection prompts the LLM to call
    `propose_hypothesized_node` during intake; the resulting proposal sits in
    `awaiting_signoff` and the server REFUSES `propose_summary_confirmation`
    (and `emit_package`) until every proposal is `promoted` or `rejected`. A
    headless eval has no UI to click the proposal card, so without this drain
    the session is pinned in `intake_followup` until the turn budget runs out.
    Returns the number of proposals acted on this call."""
    r = _get(base_url, f"/api/chat/session/{sid}/proposals", timeout=_STATE_TIMEOUT)
    if r.status_code != 200:
        raise ChatIntakeError(f"GET /proposals -> {r.status_code}: {r.text[:300]}")
    acted = 0
    for p in r.json():
        kind = (p.get("lifecycle") or {}).get("kind", "")
        pid = p.get("id")
        if not pid or kind in _TERMINAL_PROPOSAL_KINDS:
            continue
        # "signoff" promotes an `awaiting_signoff` proposal. A `blocked`
        # proposal is unpromotable (it failed a sandbox/policy gate, so signoff
        # 409s) yet still holds the confirmation gate closed (is_pending_sme is
        # true for Blocked), so it MUST be rejected to let intake converge.
        # Other pre-signoff kinds (pending validation/sandbox) advance
        # synchronously and are re-checked next turn. "reject" declines every
        # non-terminal proposal outright.
        if policy == "signoff" and kind == "awaiting_signoff":
            resp = _post(base_url,
                         f"/api/chat/session/{sid}/proposal/{pid}/signoff",
                         json_body={"sme_initials": "eval"},
                         timeout=_SME_NAMED_TIMEOUT)
        elif policy == "signoff" and kind != "blocked":
            continue
        else:
            resp = _post(base_url,
                         f"/api/chat/session/{sid}/proposal/{pid}/reject",
                         json_body={"rationale": "eval: declining hypothesized "
                                    "node to let intake converge"},
                         timeout=_SME_NAMED_TIMEOUT)
        if resp.status_code != 204:
            raise ChatIntakeError(
                f"proposal action({pid}, policy={policy}, kind={kind}) -> "
                f"{resp.status_code}: {resp.text[:200]}")
        acted += 1
    return acted


def drive_chat_intake_with_metrics(base_url: str, instruction: str, *,
                                   careful_mode: bool = False,
                                   locked_methods: list[tuple[str, str]] | None = None,
                                   proposal_policy: str = "reject",
                                   intake_turn_budget: int = 8) -> tuple[str, Path, dict]:
    """Drive a session from create → confirm → emitted; return
    (session_id, pkg, metrics_snapshot).

    `locked_methods` is a list of (stage_id, method) pairs. For each, the eval
    POSTs the SME-named-method flag BEFORE the first turn, then names the method
    in the opening instruction context so the LLM is permitted to lock it.
    Empty / None leaves all methods free (the production default — the execution
    agent picks methods at runtime).

    `proposal_policy` decides hypothesized-node proposals the LLM raises during
    intake: "signoff" approves (promotes the node into the DAG), "reject"
    declines (keeps the composed plan). Either way the server-side gate that
    blocks confirmation while a proposal is undecided is cleared so intake can
    converge.

    Known residual (bounded, by design): reaching PendingConfirmation is
    LLM-mediated — only the assistant's propose_summary_confirmation advances the
    session there; no deterministic server lever exists to raise the summary
    card, and `discover_*` axes stay unresolved at intake (method choice is
    delegated to the runtime agent, never pinned here). Draining proposals above
    clears the only hard convergence blocker; the remaining dependence on the LLM
    emitting the summary within `intake_turn_budget` is covered by the server's
    intake-followup streak nudge. If a future run exhausts the budget without a
    blocking proposal, raise `intake_turn_budget` rather than trying to resolve
    discovery here (there is no method-neutral way to do so pre-emission).
    """
    if proposal_policy not in _PROPOSAL_POLICIES:
        raise ChatIntakeError(
            f"unknown proposal_policy {proposal_policy!r}; "
            f"expected one of {sorted(_PROPOSAL_POLICIES)}")
    locked = locked_methods or []

    # 1. Create the session.
    r = _post(base_url, "/api/chat/session",
              json_body={"careful_mode": careful_mode}, timeout=_CREATE_TIMEOUT)
    if r.status_code != 200:
        raise ChatIntakeError(f"POST /session -> {r.status_code}: {r.text[:300]}")
    sid = r.json()["session_id"]

    # 1b. Pre-set SME-named-method flags so the LLM is PERMITTED to call
    # set_intake_method for the locked stage/method pairs. Must happen BEFORE
    # the first turn (the flag gates the subsequent set_intake_method call).
    first_message = instruction
    if locked:
        for stage_id, method in locked:
            sr = _post(base_url,
                       f"/api/chat/session/{sid}/intake-method/{stage_id}/sme-named",
                       json_body={}, timeout=_SME_NAMED_TIMEOUT)
            if sr.status_code != 204:
                raise ChatIntakeError(
                    f"POST sme-named({stage_id}) -> {sr.status_code}: {sr.text[:200]}")
        method_clause = "; ".join(f"{m} for {s}" for s, m in locked)
        first_message = (
            f"{instruction}\n\nUse these specific methods: {method_clause}.")

    # 2. Intake loop. First turn carries the (method-augmented) instruction;
    # subsequent turns carry the deterministic, method-free nudge.
    kind, _ = "", {}
    for i in range(intake_turn_budget):
        message = first_message if i == 0 else _NUDGE
        _send_turn(base_url, sid, message)
        # Decide any proposal the LLM raised this turn before checking state —
        # an undecided proposal pins the session in `intake_followup` (the
        # server refuses propose_summary_confirmation until it's cleared).
        _resolve_pending_proposals(base_url, sid, proposal_policy)
        kind, _ = _state_kind(base_url, sid)
        if kind == "blocked":
            raise ChatIntakeError(
                f"session {sid} blocked during intake (no SME to resolve)")
        if kind in _READY_KINDS:
            break
    else:
        raise ChatIntakeError(
            f"intake did not converge to confirmation in {intake_turn_budget} "
            f"turns (last kind={kind!r})")

    # 3. Confirm (only valid from pending_confirmation). The handler runs
    # try_auto_emit_after_confirm synchronously, so on 204 the package is
    # already (almost always) emitted. An Idempotency-Key makes a retried
    # confirm replay the cached 204 rather than double-acting.
    if kind == "pending_confirmation":
        cr = _post(base_url, f"/api/chat/session/{sid}/confirm",
                   json_body={},
                   headers={"Idempotency-Key": str(uuid.uuid4())},
                   timeout=_CONFIRM_TIMEOUT)
        if cr.status_code != 204:
            # A confirm that 400s with an "illegal transition ... from
            # Emitting/Emitted" means the session has ALREADY advanced past
            # PendingConfirmation — i.e. a prior confirm (e.g. one that _post
            # timed out on mid-emit and retried) already succeeded and auto-emit
            # is underway or done. That is the success path, not a failure: fall
            # through to the emit poll below rather than aborting the base run.
            body = cr.text or ""
            already_confirmed = cr.status_code == 400 and (
                "from Emitting" in body or "from Emitted" in body)
            if not already_confirmed:
                raise ChatIntakeError(
                    f"POST /confirm -> {cr.status_code}: {body[:300]}")

    # 4. Poll for the emitted package path (auto-emit is synchronous, but be
    # defensive against an emitting->emitted lag).
    deadline = time.time() + _EMIT_POLL_DEADLINE
    while time.time() < deadline:
        kind, state = _state_kind(base_url, sid)
        pkg = state.get("emitted_package_path")
        if pkg and kind == "emitted":
            metrics = _harvest_metrics(base_url, sid)
            return sid, Path(pkg), metrics
        if kind == "blocked":
            raise ChatIntakeError(
                f"session {sid} blocked after confirm (no SME to resolve)")
        time.sleep(_EMIT_POLL_INTERVAL)
    raise ChatIntakeError(
        f"session {sid} package not emitted after confirm "
        f"(last kind={kind!r})")


def drive_chat_intake(base_url: str, instruction: str, **kwargs) -> tuple[str, Path]:
    """Back-compat 2-tuple wrapper around `drive_chat_intake_with_metrics`.
    Existing callers that don't want the metrics snapshot stay unchanged."""
    sid, pkg, _metrics = drive_chat_intake_with_metrics(base_url, instruction, **kwargs)
    return sid, pkg
