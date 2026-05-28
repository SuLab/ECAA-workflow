#!/usr/bin/env python3
"""WRROC v0.5 conformance validator.

Uses `runcrate report` (Tier-1 parseability proxy; `runcrate validate`
has never shipped) plus four explicit Tier-3 conformance checks:
descriptor `conformsTo` includes the three WRROC profile IRIs,
at least one `ParameterConnection`, and at least one `p-plan:Plan`.

Usage:
    python scripts/wrroc-validate.py <package-dir> [<package-dir> ...]

Emits JSON to stdout:
    {
      "validated": [
        {"path": "/tmp/pkg1", "ok": true, "profiles": ["process/0.5", ...]},
        {"path": "/tmp/pkg2", "ok": false, "errors": ["..."]}
      ],
      "summary": {"total": N, "passed": M, "failed": N-M}
    }

Exit code: 0 iff every package validated; 1 otherwise.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


REQUIRED_PROFILES = [
    "https://w3id.org/ro/crate/1.1",
    "https://w3id.org/ro/wfrun/process/0.5",
    "https://w3id.org/ro/wfrun/workflow/0.5",
    "https://w3id.org/ro/wfrun/provenance/0.5",
]


def validate_one(pkg_dir: Path) -> dict:
    """Run runcrate validate on a single package directory."""
    if not (pkg_dir / "ro-crate-metadata.json").exists():
        return {
            "path": str(pkg_dir),
            "ok": False,
            "errors": ["missing ro-crate-metadata.json"],
        }

    # 1. Tier-1 RO-Crate 1.1 parseability check.
    #
    # No released runcrate version (0.1.0–0.6.2) ships a `validate`
    # subcommand. Use `runcrate report` as the parseability proxy: it
    # constructs an `ROCrate(crate)` over `ro-crate-metadata.json` and
    # walks the action graph, which fails fast on any malformed
    # JSON-LD, missing descriptor, or schema violation. Combined with
    # the explicit conformsTo + ParameterConnection + p-plan:Plan
    # checks in the steps below, this is the WRROC v0.5 conformance
    # contract.
    #
    # Warnings on stderr (which `runcrate report` emits when iterating
    # an unusual action shape) do not fail the check; only a non-zero
    # exit or a Python traceback in stderr indicates a real failure.
    errors = []
    try:
        result = subprocess.run(
            ["runcrate", "report", str(pkg_dir)],
            capture_output=True,
            text=True,
            timeout=60,
        )
    except FileNotFoundError:
        return {
            "path": str(pkg_dir),
            "ok": False,
            "errors": ["runcrate not installed (pip install runcrate>=0.5.0)"],
        }
    except subprocess.TimeoutExpired:
        return {
            "path": str(pkg_dir),
            "ok": False,
            "errors": ["runcrate report timed out after 60s"],
        }

    if result.returncode != 0 or "Traceback" in result.stderr:
        errors.append(
            f"runcrate report (parseability) exit={result.returncode}: "
            f"{result.stderr.strip().splitlines()[-1] if result.stderr.strip() else ''}"
        )

    # 2. Confirm the descriptor's conformsTo lists every required profile.
    with (pkg_dir / "ro-crate-metadata.json").open() as f:
        metadata = json.load(f)
    graph = metadata.get("@graph", [])
    descriptor = next(
        (e for e in graph if e.get("@id") == "ro-crate-metadata.json"),
        None,
    )
    if descriptor is None:
        errors.append("ro-crate-metadata.json descriptor entry not in @graph")
    else:
        conforms = descriptor.get("conformsTo", [])
        if isinstance(conforms, dict):
            conforms = [conforms]
        conform_ids = {c.get("@id") for c in conforms}
        for required in REQUIRED_PROFILES:
            if required not in conform_ids:
                errors.append(f"missing conformsTo: {required}")

    # 3. Confirm at least one ParameterConnection entity (Tier-3)
    has_connection = any(
        e.get("@type") == "ParameterConnection" for e in graph
    )
    if not has_connection:
        errors.append(
            "no ParameterConnection entities (WRROC Tier-3 requires per-edge connections)"
        )

    # 4. Confirm at least one p-plan:Plan entity
    has_plan = any(
        "p-plan:Plan" in (e.get("@type") if isinstance(e.get("@type"), list) else [e.get("@type")])
        for e in graph
    )
    if not has_plan:
        errors.append("no p-plan:Plan entity (WRROC Tier-3 requires prospective plan)")

    return {
        "path": str(pkg_dir),
        "ok": len(errors) == 0,
        "errors": errors,
        "profiles": list(REQUIRED_PROFILES) if not errors else [],
    }


def main(argv: list[str]) -> int:
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2

    results = [validate_one(Path(p).resolve()) for p in argv[1:]]
    passed = sum(1 for r in results if r["ok"])
    failed = len(results) - passed

    out = {
        "validated": results,
        "summary": {
            "total": len(results),
            "passed": passed,
            "failed": failed,
        },
    }
    json.dump(out, sys.stdout, indent=2)
    print()
    return 0 if failed == 0 else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
