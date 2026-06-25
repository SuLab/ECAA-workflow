#!/usr/bin/env python3
"""Strict RO-Crate 1.1 + WRROC conformance gate for an emitted crate dir.

EXECUTION-AWARE: the profile set that is validated depends on whether the crate
carries real executed `CreateAction`s (i.e. is a fully-executed package):

  Plan crate (no CreateAction):
    - validates: ro-crate-1.1
    - does NOT validate the three WRROC run profiles — a plan crate that lacks
      real retrospective CreateActions cannot truthfully claim
      process/workflow/provenance-run-crate-0.5, and `roc-validator` would fail
      those profiles for exactly that reason.

  Executed crate (has ≥1 CreateAction):
    - validates: ro-crate-1.1, process-run-crate-0.5,
                 workflow-run-crate-0.5, provenance-run-crate-0.5

HONEST RESIDUAL: real packages may contain script-less tasks (e.g. a
`data_acquisition` task that has no executor script because it records a
download rather than a compute step). Such tasks produce no `instrument` on
their CreateAction, so the real package does NOT fully pass
provenance-run-crate-0.5. The gate's executed-crate conformance proof therefore
uses the offline driver crate (fresh_executed_crate.rs), which is fully
scripted. The gate documents this residual explicitly: real packages conform to
the extent their producing tasks record scripts.

Exits non-zero if any REQUIRED-severity check fails. Used by `make conformance`
and the `make roc-gate` target."""
import json
import sys
import rocrate_validator.services as svc
from rocrate_validator.models.settings import ValidationSettings
from pathlib import Path

PLAN_PROFILES = ["ro-crate-1.1"]

EXECUTED_PROFILES = [
    "ro-crate-1.1",
    "process-run-crate-0.5",
    "workflow-run-crate-0.5",
    "provenance-run-crate-0.5",
]


def crate_has_create_action(crate_dir: str) -> bool:
    """Return True iff the crate's @graph contains a real executed CreateAction.

    Mirrors the Rust `graph_has_run_create_action` predicate in
    `crates/core/src/ro_crate.rs`: an entity is a real run CreateAction when it
    is typed `CreateAction` (scalar or array). Entities whose type is the plain
    `Action` supertype (prospective plan steps) are NOT counted.
    """
    meta = Path(crate_dir) / "ro-crate-metadata.json"
    if not meta.exists():
        return False
    with meta.open() as f:
        doc = json.load(f)
    graph = doc.get("@graph", [])
    for entity in graph:
        t = entity.get("@type", "")
        if isinstance(t, str):
            if t == "CreateAction":
                return True
        elif isinstance(t, list):
            if "CreateAction" in t:
                return True
    return False


def main(crate_dir: str) -> int:
    executed = crate_has_create_action(crate_dir)
    profiles = EXECUTED_PROFILES if executed else PLAN_PROFILES
    crate_kind = "executed" if executed else "plan"
    print(f"[roc-validate-strict] crate={crate_dir!r}  kind={crate_kind}  "
          f"profiles={profiles}")
    if not executed:
        print("  NOTE: plan crate — WRROC run profiles not tested (crate does "
              "not claim them; it cannot truthfully satisfy them without real "
              "CreateActions). ro-crate-1.1 is the only applicable roc-validator "
              "profile for a pre-execution package.")

    any_fail = False
    for pid in profiles:
        s = ValidationSettings(rocrate_uri=crate_dir, profile_identifier=pid,
                               requirement_severity="REQUIRED",
                               metadata_only=True, skip_availability_check=True)
        r = svc.validate(s)
        names = sorted({getattr(getattr(i, "check", None), "name", str(i))
                        for i in r.get_issues()})
        status = "PASS" if r.passed() else "FAIL"
        print(f"  [{status}] {pid}" + ("" if r.passed() else f" -> {names}"))
        any_fail = any_fail or not r.passed()
    return 1 if any_fail else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
