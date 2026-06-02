#!/usr/bin/env python3
"""Harvest real audit-proof invariant violations into regression fixtures.

Every case in the invariant-utility matrix (`invariant_utility.rs`) is a
synthetic JSON corruption. This harvester captures a handful of *real*
non-`Pass` packages — produced by actual agent/eval runs, not hand-mutation —
into `crates/ecaa-conformance/tests/fixtures/harvested-violations/<slug>/`,
giving the conformance suite ecological validity and guarding the invariants
against regressions on shapes that occur in practice.

Scan roots (in order):
  1. ~/.ecaa-workflow/packages              (the live package store)
  2. ~/.ecaa-workflow/qa-*/packages         (QA campaign stores)
  3. ~/.ecaa-workflow/atom-campaign/packages (atom-coverage campaign store)
  4. testdata/emitted-packages              (the committed corpus)

For each package whose `runtime/audit-proof-report.json` carries a `warn` or
`fail` verdict on a *blocking-or-evidence* invariant, copy the minimal sidecar
set the audit-proof loader reads (10 `runtime/*` files + the descriptor) into a
fixture dir and write `EXPECTED.json` = the observed `{DebugId: status}` map.

Real `Fail` packages are rare (a `Fail` blocks emission), so it is expected
that the natural captures are `Warn`s (e.g. `evidence_coverage = warn`).

Capping: at most MAX_FIXTURES fixtures are written. Nothing is silently
truncated — every candidate that is dropped (because the cap was hit, or
because a fixture slug already exists) is printed.

Usage:
    python3 scripts/harvest-invariant-violations.py [--packages-root DIR ...]
                                                    [--max N] [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import shutil
import sys
from pathlib import Path

# The audit-proof loader (crates/core/src/audit_proof/loader.rs) reads exactly
# these runtime sidecars; the substrate invariant additionally reads the
# root-level descriptor. Copying just these keeps fixtures minimal and the
# verdicts reproducible under NoopWrrocValidator.
RUNTIME_SIDECARS = [
    "intake-conversation.jsonl",
    "decisions.jsonl",
    "validation-reports.jsonl",
    "proofs.jsonl",
    "claim-verification.json",
    "verifier-decisions.jsonl",
    "assumptions.jsonl",
    "determinism-shim.json",
    "security-policy.json",
    "plot_affordances.jsonl",
]
ROOT_FILES = ["ro-crate-metadata.json"]

# Invariants whose warn/fail is worth capturing as a regression: the four that
# can block emission plus the evidence-coverage signal. decision_justification
# is excluded — its non-Pass states are informational, not a violation we guard.
BLOCKING_OR_EVIDENCE = {
    "claim_completeness",
    "equivalence_failure",
    "cross_graph_integrity",
    "substrate_validity",
    "evidence_coverage",
}

# EXPECTED.json keys must match the Rust test's `format!("{:?}", v.id)`, which
# renders the InvariantId enum variants in PascalCase. The report JSON stores
# the serde snake_case form, so we map snake_case -> PascalCase here.
SNAKE_TO_DEBUG = {
    "claim_completeness": "ClaimCompleteness",
    "decision_justification": "DecisionJustification",
    "evidence_coverage": "EvidenceCoverage",
    "equivalence_failure": "EquivalenceFailure",
    "cross_graph_integrity": "CrossGraphIntegrity",
    "substrate_validity": "SubstrateValidity",
}

MAX_FIXTURES = 6

REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_BASE = (
    REPO_ROOT
    / "crates"
    / "ecaa-conformance"
    / "tests"
    / "fixtures"
    / "harvested-violations"
)


def default_scan_roots() -> list[Path]:
    home = Path.home() / ".ecaa-workflow"
    roots: list[Path] = []
    if (home / "packages").is_dir():
        roots.append(home / "packages")
    # qa-* campaign stores
    for qa in sorted(home.glob("qa-*")):
        if (qa / "packages").is_dir():
            roots.append(qa / "packages")
    # atom-campaign store
    if (home / "atom-campaign" / "packages").is_dir():
        roots.append(home / "atom-campaign" / "packages")
    # the committed corpus
    corpus = REPO_ROOT / "testdata" / "emitted-packages"
    if corpus.is_dir():
        roots.append(corpus)
    return roots


def iter_packages(root: Path):
    """Yield package dirs directly under `root` that carry an audit report."""
    if not root.is_dir():
        return
    for child in sorted(root.iterdir()):
        if not child.is_dir():
            continue
        if (child / "runtime" / "audit-proof-report.json").is_file():
            yield child


def read_report(pkg: Path):
    path = pkg / "runtime" / "audit-proof-report.json"
    try:
        with path.open() as fh:
            return json.load(fh)
    except (OSError, json.JSONDecodeError) as exc:
        print(f"  WARN: unreadable report {path}: {exc}", file=sys.stderr)
        return None


def observed_map(report: dict) -> dict[str, str]:
    """Return {snake_id: status} for every verdict in the report."""
    out: dict[str, str] = {}
    for v in report.get("verdicts", []):
        vid = v.get("id")
        status = v.get("status")
        if isinstance(vid, str) and isinstance(status, str):
            out[vid] = status.lower()
    return out


def triggers(obs: dict[str, str]) -> dict[str, str]:
    """Subset of `obs` that is a warn/fail on a blocking-or-evidence invariant."""
    return {
        vid: status
        for vid, status in obs.items()
        if vid in BLOCKING_OR_EVIDENCE and status in ("warn", "fail")
    }


def slug_for(pkg: Path, fired: dict[str, str]) -> str:
    """A stable, descriptive fixture slug: <pkg-name>__<dominant-signal>."""
    # Prefer a fail over a warn in the slug suffix; pick a deterministic one.
    fail = sorted(k for k, s in fired.items() if s == "fail")
    warn = sorted(k for k, s in fired.items() if s == "warn")
    signal = (fail[0] if fail else warn[0]) + ("_fail" if fail else "_warn")
    return f"{pkg.name}__{signal}"


def copy_fixture(pkg: Path, dest: Path, obs: dict[str, str], dry_run: bool) -> None:
    if dry_run:
        return
    dest.mkdir(parents=True, exist_ok=True)
    rt_src = pkg / "runtime"
    rt_dst = dest / "runtime"
    rt_dst.mkdir(parents=True, exist_ok=True)
    for name in RUNTIME_SIDECARS:
        src = rt_src / name
        if src.is_file():
            shutil.copy2(src, rt_dst / name)
    # The audit-proof-report.json is copied too (provenance: the observed
    # verdicts we are asserting against), though the evaluator re-derives them.
    src_report = rt_src / "audit-proof-report.json"
    if src_report.is_file():
        shutil.copy2(src_report, rt_dst / "audit-proof-report.json")
    for name in ROOT_FILES:
        src = pkg / name
        if src.is_file():
            shutil.copy2(src, dest / name)
    # EXPECTED.json: debug-formatted id -> observed status, over ALL verdicts so
    # the regression pins the full signature (not just the firing invariants).
    expected = {
        SNAKE_TO_DEBUG[vid]: status
        for vid, status in obs.items()
        if vid in SNAKE_TO_DEBUG
    }
    with (dest / "EXPECTED.json").open("w") as fh:
        json.dump(expected, fh, indent=2, sort_keys=True)
        fh.write("\n")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--packages-root",
        action="append",
        type=Path,
        default=None,
        help="override scan root(s); repeatable. Defaults to the ~/.ecaa-workflow stores + testdata corpus.",
    )
    ap.add_argument("--max", type=int, default=MAX_FIXTURES, help="cap on fixtures written")
    ap.add_argument("--dry-run", action="store_true", help="report candidates without writing")
    args = ap.parse_args()

    roots = args.packages_root if args.packages_root else default_scan_roots()
    print(f"Scan roots ({len(roots)}):")
    for r in roots:
        print(f"  - {r}  {'(exists)' if r.is_dir() else '(MISSING)'}")

    # Gather candidates: dedupe by slug, prefer fail-bearing and earlier-seen.
    captured: list[tuple[str, Path, dict]] = []  # (slug, pkg, obs)
    seen_slugs: set[str] = set()
    dropped_existing: list[str] = []
    dropped_capped: list[str] = []
    scanned = 0
    candidates = 0

    for root in roots:
        for pkg in iter_packages(root):
            scanned += 1
            report = read_report(pkg)
            if report is None:
                continue
            obs = observed_map(report)
            fired = triggers(obs)
            if not fired:
                continue
            candidates += 1
            slug = slug_for(pkg, fired)
            if slug in seen_slugs or (FIXTURE_BASE / slug).exists():
                dropped_existing.append(f"{slug}  (duplicate/existing slug; {pkg})")
                continue
            seen_slugs.add(slug)
            if len(captured) >= args.max:
                dropped_capped.append(f"{slug}  fired={fired}  ({pkg})")
                continue
            captured.append((slug, pkg, obs))

    print(
        f"\nScanned {scanned} package(s) with an audit report; "
        f"{candidates} carry a blocking-or-evidence warn/fail."
    )

    if not captured and candidates == 0:
        print(
            "\nNO violations captured: every scanned package is clean on the "
            "blocking-or-evidence invariants. (Real Fail packages are rare — a "
            "Fail blocks emission. If you expected a natural evidence_coverage=warn, "
            "confirm the scan roots above are non-empty.)"
        )
        return 0

    print(f"\nCaptured {len(captured)} fixture(s):")
    for slug, pkg, obs in captured:
        dest = FIXTURE_BASE / slug
        copy_fixture(pkg, dest, obs, args.dry_run)
        fired = triggers(obs)
        action = "WOULD WRITE" if args.dry_run else "WROTE"
        print(f"  {action}: {dest.relative_to(REPO_ROOT)}")
        print(f"      source : {pkg}")
        print(f"      fired  : {fired}")
        print(f"      full   : {obs}")

    if dropped_capped:
        print(
            f"\nDROPPED {len(dropped_capped)} candidate(s) — fixture cap "
            f"(--max={args.max}) reached (NOT silently truncated):"
        )
        for d in dropped_capped:
            print(f"  - {d}")

    if dropped_existing:
        print(
            f"\nSKIPPED {len(dropped_existing)} candidate(s) — slug already "
            f"present (duplicate signature):"
        )
        for d in dropped_existing[:20]:
            print(f"  - {d}")
        if len(dropped_existing) > 20:
            print(f"  ... and {len(dropped_existing) - 20} more")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
