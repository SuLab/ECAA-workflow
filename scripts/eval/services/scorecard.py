"""Emit machine (JSON) + human (markdown) scorecards.

Nondeterministic fields (timestamps, cost, wall-clock) live under
meta/extra so reruns diff cleanly on substantive fields.
"""
from __future__ import annotations
import json
import math
import random
from dataclasses import asdict
from pathlib import Path
from statistics import mean, pstdev
from scripts.eval.benchmark import Scorecard

# Deterministic bootstrap so a re-rendered scorecard's CI is reproducible.
_BOOTSTRAP_SEED = 1729
_BOOTSTRAP_RESAMPLES = 2000

# Below this many paired (task,trial) observations the bootstrap CI is too
# under-powered to trust; the scorecard flags it loudly rather than letting a
# default `--trials 3` run (or a single-task Nekrutenko run) read as conclusive.
_MIN_POWER_PAIRS = 10


def _is_partial_judging(row) -> bool:
    """True for a BiomniBench row scored by the Opus cross-check because the
    Gemini headline judge was absent. Such rows must not enter the Gemini
    headline aggregates (they mix two judges that disagree ~0.23 linear kappa)."""
    return bool((row.extra or {}).get("partial_judging"))

# Reason-string markers the harness writes when a guard re-blocks a task
# (crates/harness/src/main.rs). Each maps a Blocked task's reason to the guard
# that caught it. Order matters only for labelling; counts are independent.
_GUARD_REASON_MARKERS: tuple[tuple[str, str], ...] = (
    ("[missing_artifact]", "missing_artifact"),
    ("[validation_failed]", "validation_failed"),
    ("overall_", "empty_result_sentinel"),       # overall_*_not_run sentinel
    ("empty-result sentinel", "empty_result_sentinel"),
    ("no-progress guard", "no_progress"),
)


def _by_arm(card: Scorecard) -> dict[str, list[float]]:
    """Per-arm overall scores for the HEADLINE. Excludes partial-judging rows so
    the Gemini-headline mean isn't blended with Opus-only fallback scores."""
    out: dict[str, list[float]] = {}
    for r in card.rows:
        if _is_partial_judging(r):
            continue
        out.setdefault(r.arm, []).append(r.overall)
    return out


def _partial_judging_count(card: Scorecard) -> int:
    return sum(1 for r in card.rows if _is_partial_judging(r))


def _render_error_matrix(em: dict) -> list[str]:
    """Render meta["error_matrix"] — one line per arm plus a by-pattern table."""
    lines = ["", "## Error matrix", ""]
    arms = sorted(em.keys())
    for arm in arms:
        entry = em[arm]
        line = (
            f"- {arm}: recover {entry.get('recover_rate', 0.0):.3f},"
            f" diagnose {entry.get('diagnose_rate', 0.0):.3f}"
            f" (n={entry.get('n_cells', 0)})"
        )
        # The paper's Table-7 handle-category signature (recover/partial/
        # propagate/crash), when the rollup carried it.
        hc = entry.get("handle_counts")
        if hc:
            line += (f" | handle recover/partial/propagate/crash = "
                     f"{hc.get('recover', 0)}/{hc.get('partial', 0)}/"
                     f"{hc.get('propagate', 0)}/{hc.get('crash', 0)}")
        lines.append(line)
    lines.append("")
    # Collect union of patterns across all arms.
    all_patterns: list[str] = []
    seen: set[str] = set()
    for arm in arms:
        for pat in em[arm].get("by_pattern", {}):
            if pat not in seen:
                all_patterns.append(pat)
                seen.add(pat)
    if all_patterns:
        # Header: pattern | <arm> recover | <arm> diagnose (repeated per arm)
        header_cols = ["pattern"]
        sep_cols = ["---"]
        for arm in arms:
            header_cols += [f"{arm} recover", f"{arm} diagnose"]
            sep_cols += ["---", "---"]
        lines.append("| " + " | ".join(header_cols) + " |")
        lines.append("| " + " | ".join(sep_cols) + " |")
        for pat in all_patterns:
            row_cols = [pat]
            for arm in arms:
                bp = em[arm].get("by_pattern", {}).get(pat)
                if bp:
                    row_cols += [
                        f"{bp.get('recover_rate', 0.0):.3f}",
                        f"{bp.get('diagnose_rate', 0.0):.3f}",
                    ]
                else:
                    row_cols += ["", ""]
            lines.append("| " + " | ".join(row_cols) + " |")
    return lines


def _dimension_caveat_text(meta: dict) -> str | None:
    """The explicit 'these are NOT paper-faithful dimension scores' caveat.

    Returns ``None`` only for a paper-defined dimension source; any heuristic
    source (the default for BiomniBench-DA, which defines no dimensions) yields
    a loud, citable warning. Falls back to a default sentence when the plugin
    set the heuristic marker but no explicit note."""
    source = meta.get("dimension_source")
    if source is None or source == "paper_defined":
        return None
    note = meta.get("dimension_note")
    base = (
        "Per-dimension scores below are a TITLE-KEYWORD HEURISTIC "
        f"(dimension_source = {source}), NOT paper-defined dimensions. "
        "BiomniBench-DA defines no per-dimension breakdown; only the overall "
        "0-100 score is benchmark-faithful. DO NOT cite these per-dimension "
        "numbers as paper-faithful dimension scores."
    )
    if note:
        return f"{base} {note}"
    return base


def _render_dimensions(meta: dict) -> list[str]:
    """Render meta["dimensions"] (BiomniBench) as a per-dimension table.

    eval-05: when the dimension source is a heuristic (the default — the dataset
    defines no dimensions), a loud, unmissable caveat is rendered immediately
    above the table so the numbers can't be lifted out as paper-faithful."""
    dims_meta: dict = meta["dimensions"]
    arms = sorted(dims_meta.keys())
    # Collect union of dimension names in insertion order.
    all_dims: list[str] = []
    seen: set[str] = set()
    for arm in arms:
        for dim in dims_meta[arm]:
            if dim not in seen:
                all_dims.append(dim)
                seen.add(dim)

    lines = ["", "## Per-dimension", ""]
    caveat = _dimension_caveat_text(meta)
    if caveat:
        lines.append(f"> **HEURISTIC — NOT PAPER-FAITHFUL.** {caveat}")
        lines.append("")
    ecaa_vals = dims_meta.get("ecaa", {})
    direct_vals = dims_meta.get("claude-direct", {})

    lines.append("| dimension | ecaa | claude-direct | delta |")
    lines.append("| --- | --- | --- | --- |")
    for dim in all_dims:
        e = ecaa_vals.get(dim)
        d = direct_vals.get(dim)
        e_str = f"{e:.1f}" if e is not None else ""
        d_str = f"{d:.1f}" if d is not None else ""
        if e is not None and d is not None:
            delta_str = f"{e - d:+.1f}"
        else:
            delta_str = ""
        lines.append(f"| {dim} | {e_str} | {d_str} | {delta_str} |")

    if "published_best" in meta:
        lines.append("")
        lines.append(f"Published best: {meta['published_best']}")

    return lines


def _render_judge_agreement(ja: dict) -> list[str]:
    exact = ja.get("exact", "")
    kappa = ja.get("kappa", "")
    return [f"Inter-judge agreement: exact {exact}, linear-weighted kappa {kappa}"]


# ── eval-02: guard-outcome dimension (ECAA error-catching, measured) ─────────
#
# The eval keeps ECAA's silent-completion / missing-artifact / validation /
# claim-verification guards ACTIVE (only the discovery review gate is
# auto-advanced). Those guards leave on-disk evidence in the executed package:
#
#   * WORKFLOW.json — a task flipped completed -> Blocked whose reason carries a
#     guard marker ([missing_artifact] / [validation_failed] / sentinel /
#     no-progress) is a blocked-by-guard event.
#   * runtime/validation-reports.jsonl — rows whose outcome is "failed:..." /
#     "errored:..." are validation failures the harness caught.
#   * runtime/claim-verification.json (top-level rollup) and any per-task
#     runtime/outputs/<tid>/claim-verification.json — n_mismatch is the count of
#     narrative claims that contradicted the result tables.
#
# Counting these turns ECAA's error-catching into measured evidence rather than
# something hidden by a blanket SME-bypass.

def collect_guard_outcomes(package_dir: Path) -> dict:
    """Scan one executed ECAA package for guard-catch evidence.

    Returns a dict with integer counts and the list of affected task ids:

        {
          "blocked_by_guard": int,        # tasks re-blocked by a guard
          "blocked_by_kind": {kind: int}, # breakdown by guard kind
          "blocked_tasks": [task_id, ...],
          "validation_failures": int,     # failed/errored validator rows
          "claim_mismatches": int,        # n_mismatch across claim reports
          "corrections": int,             # blocked_by_guard + validation_failures
        }

    Never raises: a missing/corrupt package yields all-zero counts.
    """
    pkg = Path(package_dir)
    out = {
        "blocked_by_guard": 0,
        "blocked_by_kind": {},
        "blocked_tasks": [],
        "validation_failures": 0,
        "claim_mismatches": 0,
        "corrections": 0,
    }

    # (a) guard re-blocks in WORKFLOW.json
    wf = pkg / "WORKFLOW.json"
    try:
        tasks = json.loads(wf.read_text()).get("tasks", {})
    except (OSError, ValueError, AttributeError):
        tasks = {}
    if isinstance(tasks, list):  # tolerate legacy list shape
        tasks = {t["id"]: t for t in tasks if isinstance(t, dict) and "id" in t}
    if isinstance(tasks, dict):
        for tid, task in tasks.items():
            if not isinstance(task, dict):
                continue
            state = task.get("state") or {}
            reason = ""
            if isinstance(state, dict):
                status = state.get("status")
                # Two serialized shapes: {"status":"blocked","record":{"reason"}}
                # and the flattened {"blocked":{"reason":...}} — handle both.
                if status == "blocked":
                    rec = state.get("record") or {}
                    reason = rec.get("reason", "") if isinstance(rec, dict) else ""
                elif "blocked" in state and isinstance(state["blocked"], dict):
                    reason = state["blocked"].get("reason", "")
            if not reason:
                continue
            kind = _guard_kind_for_reason(reason)
            if kind is None:
                continue
            out["blocked_by_guard"] += 1
            out["blocked_tasks"].append(str(tid))
            out["blocked_by_kind"][kind] = out["blocked_by_kind"].get(kind, 0) + 1

    # (b) validation-report failures/errors
    vr = pkg / "runtime" / "validation-reports.jsonl"
    if vr.exists():
        for line in vr.read_text().splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                row = json.loads(line)
            except ValueError:
                continue
            outcome = str(row.get("outcome", ""))
            if outcome.startswith("failed:") or outcome.startswith("errored:"):
                out["validation_failures"] += 1

    # (c) claim-verification mismatches (top-level rollup + per-task reports)
    claim_paths = [pkg / "runtime" / "claim-verification.json"]
    outputs = pkg / "runtime" / "outputs"
    if outputs.is_dir():
        claim_paths += sorted(outputs.glob("*/claim-verification.json"))
    for cp in claim_paths:
        if not cp.exists():
            continue
        try:
            rep = json.loads(cp.read_text())
        except (OSError, ValueError):
            continue
        if isinstance(rep, dict):
            out["claim_mismatches"] += int(rep.get("n_mismatch", 0) or 0)

    out["blocked_tasks"] = sorted(set(out["blocked_tasks"]))
    out["corrections"] = out["blocked_by_guard"] + out["validation_failures"]
    return out


def _guard_kind_for_reason(reason: str) -> str | None:
    for marker, kind in _GUARD_REASON_MARKERS:
        if marker in reason:
            return kind
    # A blocked task whose reason mentions the harness guard but matches no
    # specific marker is still a guard catch (label it generically).
    if "harness guard" in reason.lower() or "harness-guard" in reason.lower():
        return "other_guard"
    return None


def _aggregate_guard_outcomes(card: Scorecard) -> dict:
    """Roll per-row guard outcomes (stashed in Score.extra["guard_outcomes"])
    up to per-arm totals. Returns {} when no row carries guard evidence."""
    per_arm: dict[str, dict] = {}
    for r in card.rows:
        go = (r.extra or {}).get("guard_outcomes")
        if not isinstance(go, dict):
            continue
        agg = per_arm.setdefault(r.arm, {
            "blocked_by_guard": 0,
            "validation_failures": 0,
            "claim_mismatches": 0,
            "corrections": 0,
            "n_rows": 0,
        })
        agg["blocked_by_guard"] += int(go.get("blocked_by_guard", 0) or 0)
        agg["validation_failures"] += int(go.get("validation_failures", 0) or 0)
        agg["claim_mismatches"] += int(go.get("claim_mismatches", 0) or 0)
        agg["corrections"] += int(go.get("corrections", 0) or 0)
        agg["n_rows"] += 1
    return per_arm


def _render_guard_outcomes(per_arm: dict) -> list[str]:
    lines = ["", "## Guard outcomes (ECAA error-catching, measured)", ""]
    lines.append(
        "The discovery review gate is auto-advanced; the silent-completion, "
        "missing-artifact, validation-contract, and claim-verification guards "
        "stay ACTIVE. Counts below are how often each arm's run was caught by a "
        "guard — higher means ECAA stopped a bad completion the bare arm would "
        "have shipped silently."
    )
    lines.append("")
    lines.append(
        "| arm | runs | blocked-by-guard | validation failures | "
        "claim mismatches | corrections |"
    )
    lines.append("| --- | --- | --- | --- | --- | --- |")
    for arm in sorted(per_arm):
        a = per_arm[arm]
        lines.append(
            f"| {arm} | {a['n_rows']} | {a['blocked_by_guard']} | "
            f"{a['validation_failures']} | {a['claim_mismatches']} | "
            f"{a['corrections']} |"
        )
    return lines


# ── eval-03: method-lock asymmetry (recipe arms pin canonical tools) ─────────
#
# Recipe benchmarks (Nekrutenko) hard-pin the paper's canonical tools — bwa for
# alignment, lofreq for variant_calling — on the ECAA arm ONLY, via the
# plugin's `locked_methods(task, arm)` contract (consumed by drive_chat_intake's
# SME-named-method flag). The bare arm has no chat-intake to lock against, so it
# stays free. That asymmetry is load-bearing for interpreting the delta, so it
# must be auditable from the scorecard rather than implicit in the plugin code.

class _TaskRef:
    """Minimal stand-in carrying just ``task_id`` for ``locked_methods`` lookups
    (the method-lock contract keys on the id + arm, not on richer Task fields)."""

    def __init__(self, task_id: str):
        self.task_id = task_id


def _arm_enum_for(arm: str):
    """Map a row's arm string back to the Arm enum the plugin expects.

    Falls back to the raw string when the Arm enum can't be imported (keeps the
    helper standalone-testable with a fake plugin that accepts plain strings)."""
    try:
        from scripts.eval.benchmark import Arm
        return Arm(arm)
    except Exception:  # noqa: BLE001
        return arm


def locked_methods_meta(plugin, card: Scorecard) -> dict | None:
    """Per-arm record of the (stage, method) pairs the plugin pinned at intake.

    Sourced from ``plugin.locked_methods(task, arm)`` for every (task, arm)
    present in the card's rows. Returns a dict shaped::

        {
          "<arm>": {
            "any_locked": bool,
            "pairs": [{"stage": "alignment", "method": "bwa"}, ...],
            "by_task": {"<task_id>": [{"stage", "method"}, ...]},
          },
          ...,
          "asymmetric": bool,   # True when arms differ in what was locked
        }

    Returns ``None`` when no arm locked anything (the "free" benchmarks), so
    open evals don't grow an empty section. Never raises: a plugin without a
    usable ``locked_methods`` yields ``None``."""
    lock_fn = getattr(plugin, "locked_methods", None)
    if not callable(lock_fn):
        return None
    # Recover the (arm, task_id) universe from the rows. We only have task_ids,
    # not Task objects, so synthesize a minimal stand-in carrying the id — the
    # method-lock contract keys on the task id / arm, not on richer Task fields.
    arms: list[str] = []
    tasks_for_arm: dict[str, list[str]] = {}
    for r in card.rows:
        if r.arm not in tasks_for_arm:
            tasks_for_arm[r.arm] = []
            arms.append(r.arm)
        if r.task_id not in tasks_for_arm[r.arm]:
            tasks_for_arm[r.arm].append(r.task_id)

    per_arm: dict = {}
    any_locked = False
    for arm in arms:
        arm_enum = _arm_enum_for(arm)
        by_task: dict[str, list[dict]] = {}
        pair_set: list[dict] = []
        seen: set[tuple[str, str]] = set()
        for tid in tasks_for_arm[arm]:
            try:
                pairs = lock_fn(_TaskRef(tid), arm_enum)
            except Exception:  # noqa: BLE001 — auditing must never crash a write
                pairs = []
            norm = [{"stage": str(s), "method": str(m)} for (s, m) in (pairs or [])]
            if norm:
                any_locked = True
            by_task[tid] = norm
            for s, m in (pairs or []):
                if (str(s), str(m)) not in seen:
                    seen.add((str(s), str(m)))
                    pair_set.append({"stage": str(s), "method": str(m)})
        per_arm[arm] = {
            "any_locked": bool(pair_set),
            "pairs": pair_set,
            "by_task": by_task,
        }
    if not any_locked:
        return None
    locked_arms = {a for a, v in per_arm.items() if v["any_locked"]}
    per_arm["asymmetric"] = bool(locked_arms) and locked_arms != set(arms)
    return per_arm


def _render_locked_methods(per_arm: dict) -> list[str]:
    arms = [a for a in per_arm if a != "asymmetric"]
    lines = ["", "## Method lock (recipe arms pin canonical tools)", ""]
    if per_arm.get("asymmetric"):
        lines.append(
            "**Method-lock asymmetry:** the arms below did NOT have the same "
            "methods pinned at intake. Read the delta with this in mind — a "
            "locked arm was constrained to the paper's canonical tools while a "
            "free arm chose methods at runtime."
        )
        lines.append("")
    lines.append("| arm | locked | pinned (stage = method) |")
    lines.append("| --- | --- | --- |")
    for arm in sorted(arms):
        v = per_arm[arm]
        if v["pairs"]:
            pinned = ", ".join(f"{p['stage']} = {p['method']}" for p in v["pairs"])
        else:
            pinned = "_(none — free)_"
        lines.append(f"| {arm} | {'yes' if v['any_locked'] else 'no'} | {pinned} |")
    return lines


# ── eval-04: per-(task,trial) paired delta + bootstrap CI ────────────────────

def _paired_deltas(card: Scorecard) -> tuple[list[float], list[str]]:
    """Pair ecaa vs claude-direct on (task_id, trial) and return the list of
    per-pair deltas (ecaa - claude-direct) plus the pair keys. Only pairs where
    BOTH arms produced a score are included."""
    ecaa: dict[tuple[str, int], float] = {}
    direct: dict[tuple[str, int], float] = {}
    for r in card.rows:
        if _is_partial_judging(r):
            continue  # keep the paired headline on the Gemini judge only
        key = (r.task_id, r.trial)
        if r.arm == "ecaa":
            ecaa[key] = r.overall
        elif r.arm == "claude-direct":
            direct[key] = r.overall
    keys = sorted(set(ecaa) & set(direct))
    deltas = [ecaa[k] - direct[k] for k in keys]
    pair_ids = [f"{t}:{tr}" for (t, tr) in keys]
    return deltas, pair_ids


def _bootstrap_ci(deltas: list[float], *, resamples: int = _BOOTSTRAP_RESAMPLES,
                  alpha: float = 0.05, seed: int = _BOOTSTRAP_SEED
                  ) -> tuple[float, float]:
    """Percentile bootstrap CI for the MEAN paired delta. Stdlib-only +
    deterministic (fixed seed). Returns (lo, hi); degenerate inputs collapse to
    the point estimate."""
    n = len(deltas)
    if n == 0:
        return (0.0, 0.0)
    if n == 1:
        return (deltas[0], deltas[0])
    rng = random.Random(seed)
    means: list[float] = []
    for _ in range(resamples):
        sample = [deltas[rng.randrange(n)] for _ in range(n)]
        means.append(sum(sample) / n)
    means.sort()
    lo_idx = max(0, int(math.floor((alpha / 2) * resamples)))
    hi_idx = min(resamples - 1, int(math.ceil((1 - alpha / 2) * resamples)) - 1)
    return (means[lo_idx], means[hi_idx])


def paired_delta_summary(card: Scorecard, *, alpha: float = 0.05) -> dict | None:
    """Compute the paired ecaa-vs-direct delta with a bootstrap CI.

    Returns None when the card has no overlapping (task,trial) pairs (e.g. a
    single-arm card). Otherwise a dict carrying n_pairs, mean_delta, the CI
    bounds, and `significant` (CI excludes 0)."""
    deltas, pair_ids = _paired_deltas(card)
    if not deltas:
        return None
    n = len(deltas)
    mean_delta = sum(deltas) / n
    lo, hi = _bootstrap_ci(deltas, alpha=alpha)
    # A degenerate n==1 CI collapses to the point estimate and cannot establish
    # significance — never flag it significant (reachable via a single-task
    # Nekrutenko run or a --smoke pass). Below _MIN_POWER_PAIRS the estimate is
    # under-powered; surface that rather than letting it read as conclusive.
    significant = n >= 2 and ((lo > 0.0) or (hi < 0.0))
    return {
        "n_pairs": n,
        "pair_ids": pair_ids,
        "mean_paired_delta": mean_delta,
        "ci_lower": lo,
        "ci_upper": hi,
        "ci_level": 1 - alpha,
        "significant": significant,
        "underpowered": n < _MIN_POWER_PAIRS,
        "min_power_pairs": _MIN_POWER_PAIRS,
    }


def _render_paired_delta(summary: dict) -> list[str]:
    n = summary["n_pairs"]
    md = summary["mean_paired_delta"]
    lo, hi = summary["ci_lower"], summary["ci_upper"]
    level = int(round(summary["ci_level"] * 100))
    lines = ["", "## Paired delta (ecaa - claude-direct)", ""]
    if summary.get("underpowered"):
        mn = summary.get("min_power_pairs", _MIN_POWER_PAIRS)
        lines.append(
            f"> **UNDERPOWERED — n={n} < {mn} paired observations.** Read the delta "
            f"and CI as indicative only; raise `--trials` (and, for Nekrutenko, note "
            f"the single task caps n at the trial count) before drawing conclusions."
        )
        lines.append("")
    lines.append(f"- **n (paired task/trial):** {n}")
    lines.append(
        f"- **mean paired delta:** {md:+.2f} "
        f"({level}% bootstrap CI [{lo:+.2f}, {hi:+.2f}])"
    )
    if summary["significant"]:
        lines.append(
            f"- Significant at n={n}: the {level}% CI excludes 0."
        )
    else:
        lines.append(
            f"- NOT significant at n={n} (CI crosses 0). A larger trial count "
            f"is needed to distinguish the arms."
        )
    return lines


# ---------------------------------------------------------------------------
# F12 readiness gate: which invariants the A-vs-B' contrast actually measures.
#
# Mirrors the Rust readiness table in
# `crates/core/src/audit_proof/bench_readiness.rs::readiness_for`. An invariant
# is benchmarkable only once the phase that makes it non-vacuous has landed:
#   - claim_completeness / cross_graph_integrity  ⇐ Phase 1 (signed verdict sink)
#   - equivalence_failure                         ⇐ Phase 3 (ecaa:refs / 04-C5)
#   - evidence_coverage                           ⇐ Phase 3 (04-C2)
#   - decision_justification / substrate_validity ⇐ referential (Phase 0)
#
# Threading this into the scorecard meta makes the contrast self-describing: a
# reader sees exactly which invariants were measured and why the rest were
# excluded, so a still-vacuous invariant can never silently confound the
# headline. The probe booleans default to the live pre-Phase-1/3 state; flip
# them as each phase lands (and re-run the null-treatment control first).
# ---------------------------------------------------------------------------

_READINESS_RULES = {
    "claim_completeness": ("signed_sink", "requires Phase 1 signed verdict sink (F1)"),
    "cross_graph_integrity": ("signed_sink", "requires Phase 1 signed verdict sink (F1)"),
    "equivalence_failure": ("refs_projected", "requires Phase 3 ecaa:refs context + refs projection (04-C5)"),
    "evidence_coverage": ("evidence_from_proofs", "requires Phase 3 evidence_coverage-from-proofs (04-C2/F6)"),
    "decision_justification": (None, None),  # referential — always ready
    "substrate_validity": (None, None),  # referential — always ready
}


def benchmarkable_set_meta(
    *,
    signed_sink: bool = False,
    refs_projected: bool = False,
    evidence_from_proofs: bool = False,
) -> dict:
    """Compute the benchmarkable invariant set + per-invariant exclusion reasons
    from the live de-vacuifying state. Mirrors the Rust readiness table."""
    probes = {
        "signed_sink": signed_sink,
        "refs_projected": refs_projected,
        "evidence_from_proofs": evidence_from_proofs,
    }
    ready: list[str] = []
    excluded: dict[str, str] = {}
    for inv, (gate, reason) in _READINESS_RULES.items():
        if gate is None or probes.get(gate, False):
            ready.append(inv)
        else:
            excluded[inv] = reason
    return {
        "ready": sorted(ready),
        "excluded": dict(sorted(excluded.items())),
        "probes": probes,
    }


def probe_devacuifiers(package_dir) -> dict:
    """Structural disk-probe of the de-vacuifying artifacts on one executed ECAA
    package. Mirrors the Rust conformance probes in
    `crates/ecaa-conformance/tests/conformance/benchmark_readiness.rs` so the
    Python scorecard's published `benchmarkable_set` agrees with the Rust
    readiness gate (`no_vacuous_invariant_is_benchmarked`):

      - ``evidence_from_proofs`` ⇐ ``runtime/proofs.jsonl`` exists AND carries at
        least one non-blank line (the 04-C2 de-vacuifier for Inv 3).
      - ``signed_sink`` ⇐ ``runtime/verification-reports/claim-verification.signed.json``
        exists (Phase 1 signed verdict sink for Inv 1/5).
      - ``refs_projected`` ⇐ honest ``False``: the corpus carries 0 ecaa:refs, so
        Inv 4 stays vacuous (matches the Rust probe, which keeps refs=false).

    Never raises: a missing/unreadable package yields all-``False``.
    """
    pkg = Path(package_dir)
    proofs = pkg / "runtime" / "proofs.jsonl"
    try:
        evidence_from_proofs = any(line.strip() for line in proofs.read_text().splitlines())
    except OSError:
        evidence_from_proofs = False
    signed_sink = (
        pkg / "runtime" / "verification-reports" / "claim-verification.signed.json"
    ).exists()
    return {
        "signed_sink": signed_sink,
        "refs_projected": False,
        "evidence_from_proofs": evidence_from_proofs,
    }


def _render_benchmarkable_set(meta: dict) -> list[str]:
    lines = ["", "## Benchmarkable invariant set (F12 readiness gate)", ""]
    ready = meta.get("ready", [])
    excluded = meta.get("excluded", {})
    lines.append(f"- **Measured by the A-vs-B' contrast:** {', '.join(ready) or '(none)'}")
    if excluded:
        lines.append("- **Excluded (still vacuous):**")
        for inv, reason in excluded.items():
            lines.append(f"  - `{inv}` — {reason}")
    return lines


def _markdown(card: Scorecard) -> str:
    lines = [f"# {card.benchmark} scorecard", ""]
    # Render scalar meta keys (skip the rich-object keys handled below).
    _RICH_KEYS = {"error_matrix", "dimensions", "judge_agreement", "published_best",
                  "cost", "paired_delta", "guard_outcomes", "locked_methods",
                  # eval-05: the dimension caveat is surfaced loudly inside the
                  # Per-dimension section, not as a stray scalar bullet up top.
                  "dimension_source", "dimension_note",
                  # surfaced via the partial-judging caveat block, not a bullet.
                  "partial_judging_excluded", "dimension_caveat",
                  # F12: rendered as its own readiness-gate section, not bullets.
                  "benchmarkable_set"}
    if card.meta:
        for k, v in card.meta.items():
            if k not in _RICH_KEYS:
                lines.append(f"- **{k}:** {v}")
        lines.append("")
    arms = _by_arm(card)
    lines += ["| arm | n (trials) | mean | sd |", "|---|---|---|---|"]
    for arm, vals in sorted(arms.items()):
        sd = pstdev(vals) if len(vals) > 1 else 0.0
        lines.append(f"| {arm} | {len(vals)} | {mean(vals):.1f} | {sd:.1f} |")
    lines.append("")
    n_partial = _partial_judging_count(card)
    if n_partial:
        lines.append(
            f"> **Partial-judging rows excluded:** {n_partial} row(s) lost the "
            f"Gemini headline judge and were scored by the Opus cross-check only. "
            f"They are EXCLUDED from the means, paired delta, and per-dimension "
            f"figures above so two judge models aren't blended; re-run with "
            f"`--resume` once Gemini credit returns to fold them in."
        )
        lines.append("")
    if "ecaa" in arms and "claude-direct" in arms:
        delta = mean(arms["ecaa"]) - mean(arms["claude-direct"])
        lines.append(f"**ecaa - claude-direct raw-mean delta:** {delta:+.1f}")

    # eval-04: per-(task,trial) paired delta + bootstrap CI (the honest
    # headline). Falls back to nothing when there are no overlapping pairs.
    paired = paired_delta_summary(card)
    if paired is not None:
        lines += _render_paired_delta(paired)

    # eval-02: guard-outcome dimension (ECAA error-catching, measured).
    guard = _aggregate_guard_outcomes(card)
    if guard:
        lines += _render_guard_outcomes(guard)

    # Optional rich sections.
    if card.meta:
        # F12: the readiness gate — which invariants the contrast measured and
        # why the rest were excluded (vacuous). Surfaced before the arm-fairness
        # sections so a reader scopes the headline correctly.
        if card.meta.get("benchmarkable_set"):
            lines += _render_benchmarkable_set(card.meta["benchmarkable_set"])
        # eval-03: method-lock asymmetry (recipe arms pin canonical tools).
        if card.meta.get("locked_methods"):
            lines += _render_locked_methods(card.meta["locked_methods"])
        if "error_matrix" in card.meta:
            lines += _render_error_matrix(card.meta["error_matrix"])
        if "dimensions" in card.meta:
            lines += _render_dimensions(card.meta)
        if "judge_agreement" in card.meta:
            lines.append("")
            lines += _render_judge_agreement(card.meta["judge_agreement"])
        if "cost" in card.meta:
            cost = card.meta["cost"] or {}
            lines.append("")
            lines.append(f"Judge cost (USD): {cost.get('judge_usd', '')}")

    return "\n".join(lines) + "\n"


def write_scorecard(card: Scorecard, out_dir: Path, *, plugin=None, package_dir=None) -> Path:
    """Emit the machine (JSON) + human (markdown) scorecards.

    ``plugin`` (optional) is the Benchmark plugin that produced the card. When
    supplied, the method-lock asymmetry (eval-03) is sourced from
    ``plugin.locked_methods(task, arm)`` and surfaced under
    ``meta["locked_methods"]``; a recipe arm's pinned tools then appear in both
    the JSON and the markdown. Omitting it keeps the legacy call shape working
    (the runner passes the plugin so the field auto-populates on real runs).

    ``package_dir`` (optional) is a representative executed ECAA package whose
    de-vacuifying artifacts are disk-probed (see ``probe_devacuifiers``) to
    compute the published ``benchmarkable_set``. With a real package present the
    set reflects reality — e.g. a non-empty ``runtime/proofs.jsonl`` reports
    ``evidence_coverage`` as benchmarkable — so the Python scorecard agrees with
    the Rust readiness gate instead of hardcoding the all-false pre-Phase state."""
    out_dir.mkdir(parents=True, exist_ok=True)
    # Surface the derived eval-04 paired stats and eval-02 guard-outcome
    # aggregates in the machine scorecard's meta (without mutating the caller's
    # card). A pre-set meta value (e.g. from a plugin) wins.
    meta = dict(card.meta or {})
    paired = paired_delta_summary(card)
    if paired is not None and "paired_delta" not in meta:
        meta["paired_delta"] = paired
    guard = _aggregate_guard_outcomes(card)
    if guard and "guard_outcomes" not in meta:
        meta["guard_outcomes"] = guard
    # eval-03: record the (stage, method) pairs locked at intake, per arm, so the
    # method-lock asymmetry (ECAA arm pinned, bare arm free) is auditable from
    # the output. Sourced from the plugin's locked_methods contract.
    if plugin is not None and "locked_methods" not in meta:
        locked = locked_methods_meta(plugin, card)
        if locked is not None:
            meta["locked_methods"] = locked
    # eval-05: persist the heuristic-dimension caveat as an explicit, top-level
    # JSON field (not just buried in the markdown) so an automated reader can't
    # lift the per-dimension numbers without seeing the warning.
    caveat = _dimension_caveat_text(meta)
    if caveat and "dimension_caveat" not in meta:
        meta["dimension_caveat"] = caveat
    # Count of Opus-only fallback rows excluded from the Gemini headline (a
    # first-class caveat, not buried in the cost block).
    n_partial = _partial_judging_count(card)
    if n_partial and "partial_judging_excluded" not in meta:
        meta["partial_judging_excluded"] = n_partial
    # F12: record the benchmarkable invariant set + per-invariant exclusion
    # reasons so the A-vs-B' contrast is self-describing. When a representative
    # executed ECAA package is supplied, disk-probe its de-vacuifying artifacts
    # (proofs.jsonl / signed sink) so the published set mirrors the Rust
    # readiness gate — a non-empty proofs.jsonl reports evidence_coverage as
    # benchmarkable. Absent a package, fall back to the honest all-false
    # pre-Phase state. A caller/plugin that pre-set meta["benchmarkable_set"]
    # still wins.
    if "benchmarkable_set" not in meta:
        probes = probe_devacuifiers(package_dir) if package_dir is not None else {}
        meta["benchmarkable_set"] = benchmarkable_set_meta(**probes)
    # Render markdown from a card carrying the derived/injected meta so the
    # human scorecard shows the same locked-methods + caveat sections.
    render_card = Scorecard(benchmark=card.benchmark, rows=card.rows, meta=meta)
    payload = {"benchmark": card.benchmark, "meta": meta,
               "rows": [asdict(r) for r in card.rows]}
    (out_dir / "scorecard.json").write_text(json.dumps(payload, indent=2, default=str))
    (out_dir / "scorecard.md").write_text(_markdown(render_card))
    return out_dir
