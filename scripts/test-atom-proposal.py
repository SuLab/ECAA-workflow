#!/usr/bin/env python3
"""
Atom-proposal edge case: send a prompt that triggers
`propose_hypothesized_node` (a niche modality that doesn't match any
archetype well). Auto-approve the proposal. Verify the emitted DAG
contains the promoted node.

Acceptance:
  - At least one proposal was created during the session.
  - At least one proposal reached `promoted` lifecycle.
  - The promoted node_id (or a sanitized variant) appears in the
  emitted WORKFLOW.json tasks set.
"""

import glob
import json
import os
import sys
import time
import urllib.error
import urllib.request

SERVER = os.environ.get("SWFC_SERVER_URL", "http://127.0.0.1:3000")
PACKAGE_ROOT = os.environ.get("SWFC_PACKAGE_ROOT", "/tmp/swfc-packages-test")
PREFIX = "/api/v1/chat"
POLL = 4
PER_TURN = 240


def http(method, path, body=None, timeout=PER_TURN):
    url = SERVER + path
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method)
    if body is not None:
        req.add_header("Content-Type", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=timeout) as r:
            raw = r.read()
            try:
                return r.status, json.loads(raw) if raw else None
            except json.JSONDecodeError:
                return r.status, raw.decode(errors="replace")
    except urllib.error.HTTPError as e:
        return e.code, e.read().decode(errors="replace")


def get_state(sid):
    code, body = http("GET", f"{PREFIX}/session/{sid}/state", timeout=10)
    if code == 200 and isinstance(body, dict):
        st = body.get("state")
        return st.get("kind") if isinstance(st, dict) else str(st)
    return "unknown"


def poll_until(sid, targets, timeout):
    deadline = time.time() + timeout
    while time.time() < deadline:
        k = get_state(sid)
        if k in targets:
            return k
        time.sleep(POLL)
    return get_state(sid)


def proposals(sid):
    code, body = http("GET", f"{PREFIX}/session/{sid}/proposals", timeout=10)
    return body if code == 200 and isinstance(body, list) else []


def approve_all(sid):
    n = 0
    for p in proposals(sid):
        lc = p.get("lifecycle", {})
        ready = (
            (isinstance(lc, dict) and lc.get("kind") == "awaiting_signoff")
            or lc == "awaiting_signoff"
        )
        if not ready:
            continue
        pid = p.get("id") or p.get("proposal_id")
        if isinstance(pid, dict):
            pid = next(iter(pid.values()), None)
        if not pid:
            continue
        code, _ = http(
            "POST", f"{PREFIX}/session/{sid}/proposal/{pid}/signoff",
            body={"sme_initials": "harness"}, timeout=20,
        )
        if code in (200, 204):
            n += 1
    return n


# Prompt designed to trigger propose_hypothesized_node — describes an
# unusual analysis (single-cell CyTOF immunophenotyping) which doesn't
# match a fitted archetype. The model should propose new node types.
PROMPT = """We have mass cytometry (CyTOF) data from a panel of ~42 surface
+ intracellular markers profiled on PBMC samples from 24 healthy donors
and 30 patients with chronic lymphocytic leukemia. FCS files are already
normalized and bead-cleaned. I want to identify immune cell subsets via
clustering on the marker space (FlowSOM or similar), compute per-cluster
abundance per donor, and test for differential abundance between
healthy vs CLL. I also want a UMAP of the manifold and a per-cluster
mean-intensity heatmap. The clustering algorithm + UMAP step are not
in your standard catalog — please propose appropriate node types for
them.
"""


def main():
    code, body = http("POST", f"{PREFIX}/session", body={})
    if code != 200:
        print(f"FAIL session: {code} {body}")
        return 1
    sid = body["session_id"]
    print(f"session: {sid}")

    http("POST", f"{PREFIX}/session/{sid}/turn", body={"message": PROMPT})

    naive = [
        "Yes — please propose those node types as hypothesized atoms. Auto-approve assumed.",
        "OK, sensible defaults. Please put up the summary so I can confirm.",
        "Approved — please continue.",
        "Lock in the proposed nodes and emit.",
        "Yes proceed.",
    ]
    promoted_node_ids = []
    for i in range(10):
        kind = poll_until(
            sid, {"pending_confirmation", "ready_to_emit", "emitted", "blocked"}, PER_TURN
        )
        n_approved = approve_all(sid)
        if n_approved:
            print(f"approved {n_approved} proposal(s) at round {i}")
            # Record promoted ids for later DAG check
            for p in proposals(sid):
                lc = p.get("lifecycle")
                if isinstance(lc, dict) and lc.get("kind") == "promoted":
                    promoted_node_ids.append(p.get("node_id"))
        if kind in ("pending_confirmation", "ready_to_emit", "emitted"):
            break
        if kind == "blocked":
            print(f"FAIL blocked at round {i}")
            return 1
        http("POST", f"{PREFIX}/session/{sid}/turn", body={"message": naive[i % len(naive)]})

    # Approval sweep + emit
    approve_all(sid)
    kind = get_state(sid)
    if kind != "emitted":
        http("POST", f"{PREFIX}/session/{sid}/confirm", body={})
        kind = poll_until(sid, {"ready_to_emit", "emitted"}, 60)
        if kind == "ready_to_emit":
            approve_all(sid)
            http("POST", f"{PREFIX}/session/{sid}/turn",
                 body={"message": "(confirmed — please continue)"})
            kind = poll_until(sid, {"emitted"}, 60)

    final_proposals = proposals(sid)
    print(f"final proposals: {len(final_proposals)}")
    promoted = [
        p for p in final_proposals
        if isinstance(p.get("lifecycle"), dict) and p["lifecycle"].get("kind") == "promoted"
    ]
    print(f"promoted: {len(promoted)} — node_ids: {[p.get('node_id') for p in promoted]}")

    if not promoted:
        print("FAIL: zero proposals reached promoted lifecycle")
        return 1

    if kind != "emitted":
        print(f"FAIL: never emitted, state={kind}")
        return 1

    # Look for emitted WORKFLOW.json
    deadline = time.time() + 30
    wf = None
    while time.time() < deadline:
        matches = sorted(
            glob.glob(f"{PACKAGE_ROOT}/{sid}-*/WORKFLOW.json"),
            key=os.path.getmtime, reverse=True,
        )
        if matches:
            with open(matches[0]) as f:
                wf = json.load(f)
                wf["__path"] = matches[0]
                break
        time.sleep(2)
    if wf is None:
        print("FAIL: WORKFLOW.json not found")
        return 1

    tasks = set(wf.get("tasks", {}).keys())
    print(f"emitted: {wf['__path']}")
    print(f"task_count: {len(tasks)}")

    found_promoted = []
    for nid in [p.get("node_id") for p in promoted]:
        if not nid:
            continue
        # Promoted node_id should appear as-is OR as a task_id prefix
        if nid in tasks or any(t.startswith(nid) or nid in t for t in tasks):
            found_promoted.append(nid)

    if not found_promoted:
        node_ids = [p.get('node_id') for p in promoted]
        print(f"FAIL: promoted nodes {node_ids} not present in DAG tasks")
        print(f"  tasks sample: {sorted(tasks)[:10]}")
        return 1

    print(f"PASS atom proposal — promoted nodes present in DAG: {found_promoted}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
