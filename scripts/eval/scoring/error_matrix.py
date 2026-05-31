"""Classify one PATH-shim fault-injection cell.

handle:
  crash      -> nonzero exit, no failure log (broken run, no diagnosis)
  propagate  -> exit 0 but produced fewer valid VCFs than expected AND no log
  partial    -> handled the bad sample (skipped/logged), produced exactly the
                achievable count, recorded the failure
  recover    -> produced the full expected count despite the injection
recover  = produced_valid >= expected_valid
diagnose = the run announced the failure (failures.log / summary line non-empty)
"""
from __future__ import annotations
import re

# Word-boundary match avoids false positives like "no steps skipped".
_SKIP_RE = re.compile(r"\b(skip|skipped|omit|omitted|drop|dropped)\b", re.IGNORECASE)


def classify_cell(*, exit_code: int, failures_log: str,
                  produced_valid: int, expected_valid: int) -> dict:
    diagnosed = bool(failures_log.strip())
    recovered = produced_valid >= expected_valid
    skipped = bool(_SKIP_RE.search(failures_log))
    if exit_code != 0 and not diagnosed:
        handle = "crash"
    elif recovered and not skipped:
        handle = "recover"
    elif diagnosed and produced_valid > 0:
        handle = "partial"
    else:
        handle = "propagate"
    return {"handle": handle, "recover": recovered, "diagnose": diagnosed}
