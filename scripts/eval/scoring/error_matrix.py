"""Classify one PATH-shim fault-injection cell.

Faithful port of the Nekrutenko reference scorer's ``error_handling()``
(``score/score_run.py``). Three orthogonal axes:

m_handle (4-category histogram):
  recover    -> ALL samples produced a structurally-present VCF (n_valid == n_samples)
  partial    -> the run structurally DETECTED the failure (failures.log populated
                OR a "N/M samples" summary line on the exec log), produced < all
  propagate  -> some output AND a nonzero exit, with no structural detection
  crash      -> no output and no detection

m_recover (binary, the "best-achievable output set given the injection"):
  recover = (n_valid == target_n), EXACT equality, with a PATTERN-SPECIFIC
  target_n (see plugins/nekrutenko.py::_target_n): one_sample_fails -> n-1;
  missing_lib_error / silent_truncation / wrong_format_output -> 0 (nothing should
  work — the best a run can do is detect-and-skip every sample); else -> n.
  NOTE: this differs from m_handle's recover branch. A one_sample_fails run that
  emits 3 valid VCFs has m_recover=1 (3 == target 3) but m_handle="partial"
  (3 != 4 samples) — exactly the reference's design.

m_diagnose (binary): the run announced the failure — failures.log populated, OR a
  ``\\d+/\\d+ samples`` summary line, OR a sample-name co-occurring with a
  fail-word on the exec log (agent stdout/stderr).
"""
from __future__ import annotations
import re

# A "N/M samples" progress/summary line the reference treats as structural
# detection (score_run.py).
_SUMMARY_RE = re.compile(r"\b\d+\s*/\s*\d+\s+samples")
# Fail-word vocabulary co-located with a sample name => the run named a failing
# sample (reference: error_handling diagnose_signals).
_FAILWORD = r"(fail|error|skip|abort|missing|truncat|corrupt)"


def classify_cell(*, exit_code: int, failures_log: str, produced_valid: int,
                  target_n: int, n_samples: int = 4, exec_log: str = "",
                  samples: tuple = ()) -> dict:
    """Classify one fault cell. ``produced_valid`` is the count of samples with a
    structurally-present VCF (header-only counts; 0-byte does not — see
    plugins/nekrutenko.py::_present_sample_count). ``target_n`` is the
    pattern-specific best-achievable count. ``exec_log`` is the agent's captured
    stdout/stderr; ``samples`` the canonical sample names for the failure-mention
    scan. Returns handle/recover/diagnose plus the diagnose_signals list."""
    signals: list[str] = []
    if failures_log.strip():
        signals.append("failures_log_populated")
    if exec_log and _SUMMARY_RE.search(exec_log):
        signals.append("summary_line")
    if exec_log and samples:
        sample_pat = "|".join(re.escape(s) for s in samples)
        if re.search(rf"({sample_pat}).*{_FAILWORD}|{_FAILWORD}.*({sample_pat})",
                     exec_log, re.IGNORECASE):
            signals.append("sample_failure_mentioned")
    diagnose = bool(signals)

    # Structural detection drives the partial branch (reference): a defensive
    # script that catches every sample and logs it is 'partial', not 'crash'.
    structurally_detected = (
        "failures_log_populated" in signals or "summary_line" in signals
    )
    if produced_valid == n_samples:
        handle = "recover"
    elif structurally_detected:
        handle = "partial"
    elif produced_valid >= 1 and exit_code != 0:
        handle = "propagate"
    else:
        handle = "crash"

    recover = produced_valid == target_n
    return {"handle": handle, "recover": recover, "diagnose": diagnose,
            "diagnose_signals": signals}
