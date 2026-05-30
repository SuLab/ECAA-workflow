#!/usr/bin/env python3
"""
Shared evaluation + corpus-validation primitives for the blinded DAG-correctness corpus harness.

Both the Phase 1 driver (run_corpus.py) and the Phase 2 driver
import from this module. Keep everything
here free of driver-specific state such as MANIFEST paths or CLI parsers.
"""

import glob
import json
import os
import re
import time
import urllib.request
import urllib.error
from typing import Any

try:
    import yaml  # noqa: F401 — imported for callers; not used in this module directly
except ImportError:
    import sys
    sys.exit("pip install pyyaml")

__all__ = [
    "POLL_INTERVAL_SECS",
    "PER_TURN_TIMEOUT_SECS",
    "FOLLOWUP_MAX_TURNS",
    "EMIT_WAIT_SECS",
    "NAIVE_REPLIES",
    "SERVER",
    "PACKAGE_ROOT",
    "http",
    "create_session",
    "send_turn",
    "get_state_kind",
    "poll_until",
    "confirm",
    "list_proposals",
    "extract_proposal_capabilities",
    "collected_proposal_capabilities",
    "approve_pending_proposals",
    "get_quick_replies",
    "select_quick_reply",
    "find_emitted_workflow",
    "normalize_capability_slug",
    "corpus_load_validator",
    "evaluate_dag",
    "run_one",
]

# ---------------------------------------------------------------------------
# Module-level constants
# ---------------------------------------------------------------------------

SERVER = os.environ.get("SWFC_SERVER_URL", "http://127.0.0.1:3000")
PACKAGE_ROOT = os.environ.get(
    "SWFC_PACKAGE_ROOT",
    "/home/a/mounts/wadmin/home/a/swfc-packages/ivd-comprehensive-20260429",
)

POLL_INTERVAL_SECS = 4
PER_TURN_TIMEOUT_SECS = 600
FOLLOWUP_MAX_TURNS = 8  # max conversational rounds before giving up
EMIT_WAIT_SECS = 120

# Naive-but-decisive followup replies. The LLM tends to ask 4-7 clarifying
# questions for niche modalities; canned generic answers either work or
# unstick the chain so we can converge to a confirmation card.
NAIVE_REPLIES = [
    "Yes, those defaults are fine. Please proceed.",
    "Single processing batch, unrelated donors, local files. Please proceed.",
    "Standard reference genome, no special covariates beyond what's already mentioned. Proceed.",
    "Whatever the typical method is for this analysis is fine. Proceed.",
    "Just keep it to what I described — please put up the summary so I can confirm.",
    "I don't have additional metadata beyond the group labels. Please proceed.",
    "Please call propose_summary_confirmation now so I can confirm and emit.",
    "Lock in whatever you have so far and put up the summary card.",
]

# Private: used by normalize_capability_slug and corpus_load_validator.
_SLUG_RE = re.compile(r"[^a-z0-9_]+")

# Placeholder pair used by Tier-B flex scenarios whose modality
# isn't in the archetype catalog. When `required_atoms` equals
# exactly this set, the real test signal lives in
# `required_proposal_capabilities` — Invariant 2 below refuses to
# accept a Tier-B scenario that pairs these placeholders with no
# capabilities, since that combination tests nothing.
_VACUOUS_REQUIRED = {"raw_qc", "generic_summary"}


# ---------------------------------------------------------------------------
# HTTP primitives
# ---------------------------------------------------------------------------

def http(method: str, path: str, body: Any = None, timeout: int = 120) -> tuple[int, Any]:
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
        body_text = e.read().decode(errors="replace")
        return e.code, body_text


def create_session() -> str:
    code, body = http("POST", "/api/chat/session", {})
    if code != 200:
        raise RuntimeError(f"session create failed: {code} {body}")
    return body["session_id"]


def send_turn(session: str, text: str) -> None:
    code, body = http(
        "POST",
        f"/api/chat/session/{session}/turn",
        {"message": text},
        timeout=PER_TURN_TIMEOUT_SECS,
    )
    if code not in (200, 202):
        raise RuntimeError(f"turn failed: {code} {body}")


def get_state_kind(session: str) -> str:
    code, body = http("GET", f"/api/chat/session/{session}/state")
    if code != 200 or not isinstance(body, dict):
        return "unknown"
    st = body.get("state")
    if isinstance(st, dict):
        return st.get("kind", "unknown")
    return str(st)


def poll_until(session: str, target_kinds: set[str], timeout: int) -> str:
    """Poll until state is in `target_kinds`, or until state hits a
    SME-action state (intake_followup / intake) where the caller is
    expected to send a reply rather than block waiting. Without this
    short-circuit, an LLM turn that ends with a clarifying question
    (state -> intake_followup) would hang the poll for the full
    `timeout`, since the state will never transition to the target
    set on its own."""
    sme_action_kinds = {"intake_followup", "intake"}
    deadline = time.time() + timeout
    started_at = time.time()
    while time.time() < deadline:
        kind = get_state_kind(session)
        if kind in target_kinds:
            return kind
        if kind in sme_action_kinds and time.time() - started_at > POLL_INTERVAL_SECS * 2:
            return kind
        time.sleep(POLL_INTERVAL_SECS)
    return get_state_kind(session)


def confirm(session: str) -> None:
    code, body = http("POST", f"/api/chat/session/{session}/confirm", {})
    if code not in (200, 204):
        raise RuntimeError(f"confirm failed: {code} {body}")


# ---------------------------------------------------------------------------
# Proposal helpers
# ---------------------------------------------------------------------------

def list_proposals(session: str) -> list[dict]:
    """Return every proposal on the session (empty list on error/404)."""
    code, body = http("GET", f"/api/chat/session/{session}/proposals")
    if code != 200 or not isinstance(body, list):
        return []
    return body


def extract_proposal_capabilities(proposals: list[dict]) -> set[str]:
    """Pure function: pull `node_id` out of each proposal and normalize.

    Drops malformed entries (missing/non-string `node_id`) silently —
    they can't satisfy a capability assertion anyway and the harness
    is downstream of the server's own schema enforcement. `intent`
    is intentionally NOT read; it's free-text English, not a slug.
    """
    out: set[str] = set()
    for p in proposals:
        nid = p.get("node_id") if isinstance(p, dict) else None
        if not isinstance(nid, str) or not nid.strip():
            continue
        slug = normalize_capability_slug(nid)
        if slug:
            out.add(slug)
    return out


def collected_proposal_capabilities(session: str) -> set[str]:
    """Convenience wrapper: list_proposals + extract_proposal_capabilities.

    Captures proposals in any lifecycle state (PendingValidation /
    AwaitingSignoff / Promoted / Rejected). The classifier test cares
    that the LLM PROPOSED the capability, not whether the harness
    auto-approved it.
    """
    return extract_proposal_capabilities(list_proposals(session))


def approve_pending_proposals(session: str) -> int:
    """Auto-approve every AwaitingSignoff proposal. Returns number approved.

    Mirrors the SME clicking "Approve & promote" on each hypothesized-node
    card. Without this, emit_package refuses because proposals.lifecycle
    is still AwaitingSignoff — which was the root cause of the
    post_confirm_blocked bucket in the corpus run.
    """
    n = 0
    for p in list_proposals(session):
        lc = p.get("lifecycle", {})
        # Lifecycle is tagged: {"kind": "awaiting_signoff"} when ready.
        if (isinstance(lc, dict) and lc.get("kind") == "awaiting_signoff") or lc == "awaiting_signoff":
            pid = p.get("id") or p.get("proposal_id")
            if not pid:
                continue
            # If id is wrapped in a struct like {"0": "proposal-xxx"}, extract.
            if isinstance(pid, dict):
                pid = next(iter(pid.values()), None)
            if not pid:
                continue
            code, _ = http(
                "POST",
                f"/api/chat/session/{session}/proposal/{pid}/signoff",
                {"sme_initials": "harness"},
            )
            if code in (200, 204):
                n += 1
    return n


# ---------------------------------------------------------------------------
# Quick-reply selector
# ---------------------------------------------------------------------------

def get_quick_replies(session: str) -> list[str]:
    """If the latest assistant turn surfaced quick-reply chips, return them.

    Pulled from /transcript: the last turn's `quick_replies` field. The
    UI shows these as click-to-send chips; we mirror that by sending
    the first option's text as a new turn.
    """
    code, body = http("GET", f"/api/chat/session/{session}/transcript")
    if code != 200 or not isinstance(body, list) or not body:
        return []
    last = body[-1]
    qr = last.get("quick_replies") or []
    if not isinstance(qr, list):
        return []
    out: list[str] = []
    for entry in qr:
        if isinstance(entry, str):
            out.append(entry)
        elif isinstance(entry, dict):
            # Quick-reply shape: {"label": "...", "send_text": "..."}
            txt = entry.get("send_text") or entry.get("label")
            if isinstance(txt, str):
                out.append(txt)
    return out


def select_quick_reply(replies: list[str], scenario: dict) -> str:
    """Choose the quick-reply chip that best matches scenario expectations.

    The UI offers concrete SME choices such as "raw FASTQs" vs "Cell Ranger
    output". The batch harness should click the option consistent with the
    manifest instead of blindly choosing the first chip.
    """
    if not replies:
        return ""
    expected = scenario.get("expected_dag") or {}
    required = set(expected.get("required_atoms") or [])
    prompt = str(scenario.get("blinded_prompt") or "").lower()
    needs_raw_read_processing = (
        "alignment" in required
        or "quantification" in required
        or any(atom.endswith("_alignment") for atom in required)
        or any(atom.endswith("_quantification") for atom in required)
        or "raw fastq" in prompt
        or "raw fastqs" in prompt
        or "per-sample fastq" in prompt
        or "per-sample fastqs" in prompt
    )
    if not needs_raw_read_processing:
        return replies[0]

    raw_markers = ("raw", "fastq", "fastqs", "bcl")
    preprocessed_markers = (
        "cell ranger output",
        "filtered feature-barcode",
        "matrix",
        "matrices",
        "already have",
        "preprocessed",
    )

    def score(reply: str) -> int:
        text = reply.lower()
        raw_score = sum(1 for marker in raw_markers if marker in text)
        preprocessed_penalty = sum(
            1 for marker in preprocessed_markers if marker in text
        )
        return raw_score * 2 - preprocessed_penalty

    return max(replies, key=score)


# ---------------------------------------------------------------------------
# Workflow finder
# ---------------------------------------------------------------------------

def find_emitted_workflow(session: str, timeout: int) -> dict | None:
    deadline = time.time() + timeout
    while time.time() < deadline:
        matches = sorted(
            glob.glob(f"{PACKAGE_ROOT}/{session}-*/WORKFLOW.json"),
            key=os.path.getmtime,
            reverse=True,
        )
        if matches:
            with open(matches[0]) as f:
                d = json.load(f)
                d["__path"] = matches[0]
                return d
        time.sleep(2)
    return None


# ---------------------------------------------------------------------------
# Slug normalization + corpus validator
# ---------------------------------------------------------------------------

def normalize_capability_slug(s: str) -> str:
    """Canonical form: lowercase, non-[a-z0-9_] → underscore, collapse + strip.

    Used for both `required_proposal_capabilities` entries and proposal
    `node_id` lookups. Empty input returns empty string; the validator
    rejects empty slugs explicitly.
    """
    if not isinstance(s, str):
        return ""
    slug = _SLUG_RE.sub("_", s.strip().lower())
    while "__" in slug:
        slug = slug.replace("__", "_")
    return slug.strip("_")


def corpus_load_validator(
    scenarios: list[dict],
) -> list[str]:
    """Pure function: returns list of human-readable error strings.

    Invariants enforced on every scenario:
      1. min_task_count >= len(required_atoms) on every scenario that
         declares both (catches the tri-omics 14-required / min=4 defect).
      2. Tier-B scenarios whose required_atoms is exactly the vacuous
         pair {raw_qc, generic_summary} MUST declare
         required_proposal_capabilities (catches the flex-* defect).
      3. Every entry in required_proposal_capabilities must
         normalize-round-trip to itself (catches typos / casing /
         whitespace that would silently never match).
    """
    errors: list[str] = []
    for s in scenarios:
        sid = s.get("id", "<no-id>")
        tier = s.get("tier")
        ed = s.get("expected_dag") or {}
        req = ed.get("required_atoms") or []
        mn = ed.get("min_task_count")
        caps = ed.get("required_proposal_capabilities") or []

        if isinstance(mn, int) and isinstance(req, list) and mn < len(req):
            errors.append(
                f"{sid}: min_task_count ({mn}) below len(required_atoms) ({len(req)}); "
                f"task-count check would be dead"
            )

        if tier == "B" and set(req) == _VACUOUS_REQUIRED and not caps:
            errors.append(
                f"{sid}: Tier-B scenario with vacuous required_atoms "
                f"{sorted(_VACUOUS_REQUIRED)!r} must declare "
                f"required_proposal_capabilities"
            )

        for cap in caps:
            normalized = normalize_capability_slug(cap)
            if not normalized:
                errors.append(f"{sid}: empty capability slug")
            elif normalized != cap:
                errors.append(
                    f"{sid}: capability slug {cap!r} is not canonical "
                    f"(should be {normalized!r})"
                )

    return errors


# ---------------------------------------------------------------------------
# DAG evaluator (monolithic — inner helpers are closures over task_map)
# ---------------------------------------------------------------------------

def evaluate_dag(
    workflow: dict,
    expected: dict,
    strict_structure: bool = False,
    collected_caps: set[str] | None = None,
) -> tuple[bool, list[str], list[str]]:
    """Return (pass, list_of_failure_reasons, list_of_warnings).

    Pass is judged purely by ATOM TYPE coverage:
      - all required atoms present in the emitted DAG
      - no forbidden atoms present

    By default, task-count bounds and structural constraints are surfaced
    as warnings for historical compatibility. With strict_structure=True
    they become hard failures and the emitted graph is also checked for
    missing dependency targets, cycles, and isolated multi-task nodes.
    """
    tasks = set(workflow.get("tasks", {}).keys())
    n = len(tasks)
    failures: list[str] = []
    warnings: list[str] = []

    def record_structural(message: str) -> None:
        if strict_structure:
            failures.append(message)
        else:
            warnings.append(message)

    task_map = workflow.get("tasks", {}) or {}
    deps_by_child: dict[str, set[str]] = {
        tid: set((t or {}).get("depends_on") or [])
        for tid, t in task_map.items()
    }
    children_by_parent: dict[str, set[str]] = {tid: set() for tid in task_map}
    missing_dep_refs: list[str] = []
    for child, deps in deps_by_child.items():
        for parent in deps:
            if parent not in task_map:
                missing_dep_refs.append(f"{child!r} depends on missing {parent!r}")
            else:
                children_by_parent[parent].add(child)

    if missing_dep_refs:
        failures.extend(missing_dep_refs)

    isolated = sorted(
        tid
        for tid in task_map
        if len(task_map) > 1
        and not deps_by_child.get(tid)
        and not children_by_parent.get(tid)
    )
    if isolated:
        failures.append(f"isolated node(s) with in_deg=out_deg=0: {isolated}")

    def candidates_for(name: str) -> list[str]:
        """Resolve manifest shorthand onto concrete emitted task ids."""
        name = name.strip().strip("`'\". ")
        normalized = re.sub(r"\s+", " ", name.lower())
        name = {
            "alignment check": "alignment_check",
            "the alignment check": "alignment_check",
            "sample alignment check": "alignment_check",
            "the sample alignment check": "alignment_check",
        }.get(normalized, name)
        explicit_aliases = {
            "alignment_check": [
                "cross_omics_alignment_check",
                "validate_sample_alignment_n_way",
            ],
            "endpoint_analysis": ["clinical_endpoint_analysis"],
            "subgroup": ["clinical_subgroup_analysis"],
            "subgroup_analysis": ["clinical_subgroup_analysis"],
            "sensitivity": ["clinical_sensitivity_analysis"],
            "sensitivity_analysis": ["clinical_sensitivity_analysis"],
            "harmonization": ["gwas_summary_harmonization"],
            "harmonisation": ["gwas_summary_harmonization"],
            "integrator": [
                "cross_omics_diablo_integration",
                "integrate_multi_omics_mofa",
                "integrate_multi_omics_snf",
                "joint_wnn_integration",
            ],
            "integration": [
                "cross_omics_diablo_integration",
                "integrate_multi_omics_mofa",
                "integrate_multi_omics_snf",
                "joint_wnn_integration",
            ],
        }
        if name == "differential_expression" and "sgrna_assignment" in task_map:
            explicit_aliases[name] = [
                "per_perturbation_pseudobulk_de",
                "differential_expression",
            ]
        candidates = []
        if name in task_map:
            candidates.append(name)
        candidates.extend(t for t in explicit_aliases.get(name, []) if t in task_map)
        # Prefix-aware shorthand for branch constraints such as
        # "rnaseq and methylation branches".
        if name.endswith("_branch"):
            prefix = name.removesuffix("_branch") + "_"
            candidates.extend(t for t in task_map if t.startswith(prefix))
        return list(dict.fromkeys(candidates))

    def task_present(name: str) -> bool:
        return bool(candidates_for(name))

    def _has_path(parent: str, child: str) -> bool:
        if parent == child:
            return True
        if parent not in task_map or child not in task_map:
            return False
        stack = list(children_by_parent.get(parent, set()))
        seen: set[str] = set()
        while stack:
            node = stack.pop()
            if node == child:
                return True
            if node in seen:
                continue
            seen.add(node)
            stack.extend(children_by_parent.get(node, set()))
        return False

    def has_path(parent: str, child: str) -> bool:
        return any(
            _has_path(p, c)
            for p in candidates_for(parent)
            for c in candidates_for(child)
        )

    def _has_direct_dep(child: str, parent: str) -> bool:
        return parent in deps_by_child.get(child, set())

    def has_direct_dep(child: str, parent: str) -> bool:
        return any(
            _has_direct_dep(c, p)
            for p in candidates_for(parent)
            for c in candidates_for(child)
        )

    def check_parent_relation(child: str, parents: list[str], relation: str, any_parent: bool) -> bool:
        if not task_present(child):
            return False
        checks = [
            has_path(parent, child) if relation == "transitive" else has_direct_dep(child, parent)
            for parent in parents
            if task_present(parent)
        ]
        if not checks:
            return False
        return any(checks) if any_parent else all(checks)

    def parse_atoms(raw: str) -> list[str]:
        pieces = re.split(r"\s*(?:\+|,|\bAND\b|\band\b)\s*", raw)
        return [
            p.strip().strip("`'\". ")
            for p in pieces
            if p.strip().strip("`'\". ")
        ]

    # Cycle detection, independent of strict mode because cycles invalidate
    # every emitted package.
    if task_map:
        indeg = {tid: len(deps_by_child.get(tid, set())) for tid in task_map}
        queue = [tid for tid, degree in indeg.items() if degree == 0]
        visited = 0
        while queue:
            node = queue.pop()
            visited += 1
            for child in children_by_parent.get(node, set()):
                indeg[child] -= 1
                if indeg[child] == 0:
                    queue.append(child)
        if visited != len(task_map):
            cyclic = sorted(tid for tid, degree in indeg.items() if degree > 0)
            failures.append(f"cycle detected involving task(s): {cyclic[:10]}")

    req = set(expected.get("required_atoms") or [])
    forb = set(expected.get("forbidden_atoms") or [])
    missing = req - tasks
    if missing:
        failures.append(f"missing required atom types: {sorted(missing)}")
    illegal = forb & tasks
    if illegal:
        failures.append(f"contains forbidden atom types: {sorted(illegal)}")

    # Proposal-capability assertion (Tier-B + a few Tier-A scenarios).
    # When the scenario declares required_proposal_capabilities, every
    # capability slug must appear in the set of proposal node_ids
    # observed on the session (after normalize_capability_slug). When
    # the scenario does NOT declare any, this check is skipped.
    req_caps_raw = expected.get("required_proposal_capabilities") or []
    if req_caps_raw:
        observed = collected_caps if collected_caps is not None else set()
        req_caps = {normalize_capability_slug(c) for c in req_caps_raw if c}
        missing_caps = req_caps - observed
        if missing_caps:
            failures.append(
                f"missing required proposal capabilities: {sorted(missing_caps)}"
            )

    mn = expected.get("min_task_count")
    mx = expected.get("max_task_count")
    if mn is not None and n < mn:
        record_structural(f"task count {n} below min {mn}")
    exp_mod = expected.get("expected_modality")
    max_count_advisory = isinstance(exp_mod, str) and (
        exp_mod.startswith("cross_omics_")
        or exp_mod in {"crispr_screen_scrnaseq"}
    )
    if mx is not None and n > mx:
        msg = f"task count {n} above max {mx}"
        if max_count_advisory:
            warnings.append(msg)
        else:
            record_structural(msg)

    # Prefer the fine-grained modality_id (added to meta to match the
    # manifest's modality_id strings). Fall back to modality_stratum
    # for back-compat with packages emitted before the modality_id
    # field landed.
    meta = workflow.get("meta", {})
    actual_mod = meta.get("modality_id") or meta.get("modality_stratum")

    def inferred_expected_modality() -> str | None:
        if not isinstance(exp_mod, str):
            return None
        has = task_present
        if exp_mod == "crispr_screen_scrnaseq" and has("sgrna_assignment"):
            return exp_mod
        if exp_mod == "cross_omics_multiome_arc" and has("multiome_arc_demultiplex") and has("joint_wnn_integration"):
            return exp_mod
        if exp_mod == "cross_omics_share_seq" and has("share_seq_barcode_match") and has("joint_wnn_integration"):
            return exp_mod
        if exp_mod == "cross_omics_rnaseq_methylation" and has("expression_methylation_correlation"):
            if any(t.startswith("rnaseq_") for t in task_map) and any(t.startswith("methylation_") for t in task_map):
                return exp_mod
        if exp_mod == "cross_omics_rnaseq_atac_chip":
            if all(any(t.startswith(prefix) for t in task_map) for prefix in ("rnaseq_", "atac_", "chip_")):
                return exp_mod
        if exp_mod == "cross_omics_variant_rnaseq_chipseq":
            if has("cis_regulatory_variant_scoring") and all(any(t.startswith(prefix) for t in task_map) for prefix in ("variant_", "rnaseq_", "chipseq_")):
                return exp_mod
        if exp_mod == "cross_omics_rnaseq_proteomics":
            if has("cross_omics_thematic_comparison") and all(any(t.startswith(prefix) for t in task_map) for prefix in ("rnaseq_", "proteomics_")):
                return exp_mod
        if exp_mod == "cross_omics_rnaseq_proteomics_diablo" and has("cross_omics_diablo_integration"):
            return exp_mod
        if exp_mod == "cross_omics_rnaseq_proteomics_mofa" and has("integrate_multi_omics_mofa"):
            return exp_mod
        if exp_mod == "cross_omics_rnaseq_proteomics_snf" and has("integrate_multi_omics_snf"):
            return exp_mod
        return None

    inferred_mod = inferred_expected_modality()
    if exp_mod is not None and actual_mod != exp_mod and inferred_mod != exp_mod:
        record_structural(f"modality {actual_mod!r} differs from expected {exp_mod!r}")

    # Structural constraints — substring eval against
    # depends_on edges so authors can write plain-English invariants
    # like "differential_expression depends on normalisation".
    constraints = expected.get("structural_constraints") or []
    if constraints:
        for c in constraints:
            lower = c.lower()

            if "no " in lower and " emitted" in lower:
                continue
            if "expected project_class" in lower or "slot is filled" in lower or "surfaced" in lower:
                continue

            m_present_dep = re.match(
                r"^([a-z0-9_]+) present and depends transitively on ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_present_dep:
                child, parent = m_present_dep.group(1), m_present_dep.group(2)
                if not task_present(child) or not has_path(parent, child):
                    record_structural(f"structural: {child!r} missing transitive dependency on {parent!r}")
                continue

            m_present_dep_direct = re.match(
                r"^([a-z0-9_]+) present and depends on ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_present_dep_direct:
                child, parent = m_present_dep_direct.group(1), m_present_dep_direct.group(2)
                if not task_present(child) or not has_direct_dep(child, parent):
                    record_structural(f"structural: {child!r} missing direct dependency on {parent!r}")
                continue

            m_present = re.match(
                r"^([a-z0-9_]+) present$",
                c,
                flags=re.IGNORECASE,
            )
            if m_present:
                child = m_present.group(1)
                if not task_present(child):
                    record_structural(f"structural: {child!r} missing")
                continue

            m_both_present = re.match(
                r"^([a-z0-9_]+) and ([a-z0-9_]+) both present$",
                c,
                flags=re.IGNORECASE,
            )
            if m_both_present:
                missing = [p for p in m_both_present.groups() if not task_present(p)]
                if missing:
                    record_structural(f"structural: missing present task(s) {missing!r}")
                continue

            if lower == "clustering and dimensionality_reduction both present":
                missing = [p for p in ("clustering", "dimensionality_reduction") if not task_present(p)]
                if missing:
                    record_structural(f"structural: missing present task(s) {missing!r}")
                continue

            if lower == "broad and narrow peak calling both feed peak_annotation":
                if not has_path("peak_calling", "peak_annotation"):
                    record_structural("structural: 'peak_calling' does not feed 'peak_annotation'")
                continue

            if lower == "subgroup and sensitivity analyses both depend on endpoint_analysis":
                missing = [
                    child
                    for child in ("clinical_subgroup_analysis", "clinical_sensitivity_analysis")
                    if not has_path("clinical_endpoint_analysis", child)
                ]
                if missing:
                    record_structural(f"structural: endpoint_analysis does not feed {missing!r}")
                continue

            if lower == "endpoint_analysis precedes subgroup and sensitivity analyses":
                missing = [
                    child
                    for child in ("clinical_subgroup_analysis", "clinical_sensitivity_analysis")
                    if not has_path("clinical_endpoint_analysis", child)
                ]
                if missing:
                    record_structural(f"structural: endpoint_analysis does not precede {missing!r}")
                continue

            if lower == "expression_methylation_correlation depends on both de and dmr atoms":
                if not (
                    has_direct_dep("expression_methylation_correlation", "rnaseq_differential_expression")
                    and has_direct_dep("expression_methylation_correlation", "methylation_dmr_calling")
                ):
                    record_structural(
                        "structural: 'expression_methylation_correlation' missing direct dependency on RNA-seq DE or methylation DMR"
                    )
                continue

            if lower == "cis_regulatory_variant_scoring depends on at least one task from each branch":
                deps = deps_by_child.get("cis_regulatory_variant_scoring", set())
                missing_prefixes = [
                    prefix
                    for prefix in ("variant_", "rnaseq_", "chipseq_")
                    if not any(dep.startswith(prefix) for dep in deps)
                ]
                if missing_prefixes:
                    record_structural(
                        f"structural: 'cis_regulatory_variant_scoring' missing direct dependency from branch prefix(es) {missing_prefixes!r}"
                    )
                continue

            m_branch_direct = re.match(
                r"^([a-z0-9_]+) depends on at least one task in each branch$",
                c,
                flags=re.IGNORECASE,
            )
            if m_branch_direct:
                child = m_branch_direct.group(1)
                deps = deps_by_child.get(child, set())
                expected_prefixes = [
                    prefix
                    for prefix in ("rnaseq_", "atac_", "chip_", "chipseq_")
                    if any(t.startswith(prefix) for t in task_map)
                ]
                missing_prefixes = [
                    prefix
                    for prefix in expected_prefixes
                    if not any(dep.startswith(prefix) for dep in deps)
                ]
                if missing_prefixes:
                    record_structural(
                        f"structural: {child!r} missing direct dependency from branch prefix(es) {missing_prefixes!r}"
                    )
                continue

            m_transitive = re.match(
                r"^([a-z0-9_]+) depends transitively on ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_transitive:
                child, parent = m_transitive.group(1), m_transitive.group(2)
                if not has_path(parent, child):
                    record_structural(f"structural: {child!r} missing transitive dependency on {parent!r}")
                continue

            m_dep_if_present = re.match(
                r"^([a-z0-9_]+), if present, depends on ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_dep_if_present:
                child, parent = m_dep_if_present.group(1), m_dep_if_present.group(2)
                if child in tasks and not has_path(parent, child):
                    record_structural(f"structural: {child!r} missing dependency path from {parent!r}")
                continue

            m_dep = re.match(
                r"^([a-z0-9_]+) depends on (.+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_dep:
                child = m_dep.group(1)
                parent_raw = m_dep.group(2).strip().rstrip(".")
                if re.search(r"\s+OR\s+", parent_raw, flags=re.IGNORECASE):
                    parents = [
                        p.strip().strip("`'\"")
                        for p in re.split(r"\s+OR\s+", parent_raw, flags=re.IGNORECASE)
                        if p.strip()
                    ]
                    if not check_parent_relation(child, parents, "direct", any_parent=True):
                        record_structural(f"structural: {child!r} missing direct dependency on one of {parents!r}")
                else:
                    parents = parse_atoms(parent_raw)
                    if not check_parent_relation(child, parents, "direct", any_parent=False):
                        record_structural(f"structural: {child!r} missing direct dependency on {parents!r}")
                continue

            m_precedes = re.match(
                r"^([a-z0-9_]+) precedes ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_precedes:
                parent, child = m_precedes.group(1), m_precedes.group(2)
                if not has_path(parent, child):
                    record_structural(f"structural: {parent!r} does not precede {child!r}")
                continue

            m_feeds = re.match(
                r"^([a-z0-9_]+) (?:present and )?(?:feeds|reads from) ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_feeds:
                parent, child = m_feeds.group(1), m_feeds.group(2)
                if "reads from" in lower:
                    parent, child = child, parent
                if not has_path(parent, child):
                    record_structural(f"structural: {parent!r} does not feed {child!r}")
                continue

            m_both_feed = re.match(
                r"^([a-z0-9_]+) and ([a-z0-9_]+) both feed ([a-z0-9_]+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_both_feed:
                p1, p2, child = m_both_feed.group(1), m_both_feed.group(2), m_both_feed.group(3)
                missing = [p for p in (p1, p2) if not has_path(p, child)]
                if missing:
                    record_structural(f"structural: {missing!r} do not feed {child!r}")
                continue

            m_consumes = re.match(
                r"^([a-z0-9_]+) consumes (.+)$",
                c,
                flags=re.IGNORECASE,
            )
            if m_consumes:
                child = m_consumes.group(1)
                parents = parse_atoms(m_consumes.group(2))
                missing = [p for p in parents if not has_path(p, child)]
                if missing:
                    record_structural(f"structural: {child!r} does not consume {missing!r}")
                continue

            warnings.append(f"structural constraint not machine-checked: {c}")

    return (not failures, failures, warnings)


# ---------------------------------------------------------------------------
# Session orchestrator
# ---------------------------------------------------------------------------

def run_one(scenario: dict, strict_structure: bool = False, log_prefix: str = "") -> dict:
    sid = scenario["id"]
    start = time.time()
    result = {
        "id": sid,
        "tier": scenario.get("tier"),
        "archetype": scenario.get("archetype"),
        "ok": False,
        "stage": "init",
        "duration_secs": 0,
        "failures": [],
        "warnings": [],
        "task_count": None,
        "task_ids": None,
        "modality_stratum": None,
        "workflow_path": None,
        "session": None,
        "proposal_capabilities": None,
    }
    try:
        session = create_session()
        result["session"] = session
        result["stage"] = "session_created"

        # Turn 1 — the blinded prompt.
        send_turn(session, scenario["blinded_prompt"])
        result["stage"] = "first_turn_sent"

        # Loop conversational followups (up to FOLLOWUP_MAX_TURNS).
        for round_idx in range(FOLLOWUP_MAX_TURNS):
            kind = poll_until(
                session,
                {"pending_confirmation", "ready_to_emit", "emitted", "blocked"},
                PER_TURN_TIMEOUT_SECS,
            )
            # Auto-approve any pending proposals at every iteration so
            # the LLM can move on. Without this, propose_hypothesized_node
            # calls leave proposals in AwaitingSignoff and emit_package
            # refuses to fire.
            n_approved = approve_pending_proposals(session)
            if n_approved > 0:
                result["stage"] = f"approved_{n_approved}_proposals"
                # After approval the LLM typically wants another turn
                # to acknowledge. Be explicit that the harness already
                # clicked the UI cards, otherwise niche fallback
                # sessions can duplicate proposals until they hit the
                # model spend ceiling instead of raising the summary.
                send_turn(
                    session,
                    "I clicked Approve & promote on every proposal card in the UI. "
                    "Do not create duplicate proposals. Please call propose_summary_confirmation now "
                    "so I can confirm and emit.",
                )
                continue

            if kind in ("pending_confirmation", "ready_to_emit", "emitted"):
                break
            if kind == "blocked":
                result["stage"] = "blocked"
                result["failures"].append("session entered Blocked state")
                return result

            # Quick-reply chips beat free-text in two ways: (a) they
            # carry concrete option strings the SME would click, (b)
            # the LLM short-circuits on them. Use them when available
            # before falling back to the canned naive replies.
            qrs = get_quick_replies(session)
            if qrs:
                send_turn(session, select_quick_reply(qrs, scenario))
            else:
                send_turn(session, NAIVE_REPLIES[round_idx % len(NAIVE_REPLIES)])

        kind = get_state_kind(session)
        if kind not in ("pending_confirmation", "ready_to_emit", "emitted"):
            result["stage"] = f"stuck_in_{kind}"
            result["failures"].append(f"never reached confirmation (state={kind})")
            return result

        if kind != "emitted":
            # One last sweep for any in-flight proposals before confirm —
            # the LLM sometimes proposes a hypothesized node late in
            # intake, right before raising the summary card.
            approve_pending_proposals(session)
            confirm(session)
            result["stage"] = "confirm_sent"
            # Server design: /confirm advances PendingConfirmation →
            # ReadyToEmit. The actual emit_package tool call fires
            # from the NEXT /turn (the UI auto-sends "(confirmed —
            # please continue)" after the Accept click). Mirror that.
            kind = poll_until(
                session, {"ready_to_emit", "emitted"}, EMIT_WAIT_SECS
            )
            if kind == "ready_to_emit":
                # Approve any proposals that surfaced after confirm.
                approve_pending_proposals(session)
                send_turn(session, "(confirmed — please continue)")
                # Some sessions need a second prod (LLM emits via
                # the next turn but stays in ready_to_emit if it asks
                # a follow-up). Re-check + nudge once.
                kind = poll_until(session, {"emitted"}, EMIT_WAIT_SECS)
                if kind != "emitted":
                    approve_pending_proposals(session)
                    send_turn(session, "Please call emit_package now.")
                    kind = poll_until(session, {"emitted"}, EMIT_WAIT_SECS)
            if kind != "emitted":
                result["stage"] = f"post_confirm_{kind}"
                result["failures"].append(
                    f"post-confirm state never reached emitted (state={kind})"
                )
                return result

        wf = find_emitted_workflow(session, 30)
        if wf is None:
            result["stage"] = "no_workflow_found"
            result["failures"].append("WORKFLOW.json not found on disk")
            return result
        result["workflow_path"] = wf.get("__path")
        result["stage"] = "workflow_loaded"
        result["task_count"] = len(wf.get("tasks", {}))
        result["task_ids"] = sorted(wf.get("tasks", {}).keys())
        result["modality_stratum"] = wf.get("meta", {}).get("modality_stratum")

        collected_caps = collected_proposal_capabilities(session)
        ok, failures, warnings = evaluate_dag(
            wf,
            scenario["expected_dag"],
            strict_structure=strict_structure,
            collected_caps=collected_caps,
        )
        result["proposal_capabilities"] = sorted(collected_caps)
        result["ok"] = ok
        result["stage"] = "evaluated"
        result["failures"].extend(failures)
        result["warnings"].extend(warnings)
    except Exception as e:
        result["failures"].append(f"exception: {type(e).__name__}: {e}")
    finally:
        result["duration_secs"] = round(time.time() - start, 1)
    return result
