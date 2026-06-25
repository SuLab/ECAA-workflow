#!/usr/bin/env python3
"""Strict RO-Crate 1.1 + WRROC conformance gate for an emitted crate dir.
Exits non-zero if any REQUIRED-severity check fails. Used by `make conformance`."""
import sys
import rocrate_validator.services as svc
from rocrate_validator.models.settings import ValidationSettings

PROFILES = ["ro-crate-1.1", "process-run-crate-0.5",
            "workflow-run-crate-0.5", "provenance-run-crate-0.5"]

def main(crate_dir: str) -> int:
    any_fail = False
    for pid in PROFILES:
        s = ValidationSettings(rocrate_uri=crate_dir, profile_identifier=pid,
                               requirement_severity="REQUIRED",
                               metadata_only=True, skip_availability_check=True)
        r = svc.validate(s)
        names = sorted({getattr(getattr(i, "check", None), "name", str(i))
                        for i in r.get_issues()})
        status = "PASS" if r.passed() else "FAIL"
        print(f"[{status}] {pid}" + ("" if r.passed() else f" -> {names}"))
        any_fail = any_fail or not r.passed()
    return 1 if any_fail else 0

if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
