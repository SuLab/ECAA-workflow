# scripts/eval/tests/test_error_matrix.py
from scripts.eval.scoring.error_matrix import classify_cell

def test_crash():
    c = classify_cell(exit_code=127, failures_log="", produced_valid=0, expected_valid=4)
    assert c["handle"] == "crash" and c["recover"] is False and c["diagnose"] is False

def test_recover_full():
    c = classify_cell(exit_code=0, failures_log="bwa flake on M117C1: retried",
                      produced_valid=4, expected_valid=4)
    assert c["handle"] == "recover" and c["recover"] is True and c["diagnose"] is True

def test_partial_skips_bad_sample():
    c = classify_cell(exit_code=0, failures_log="skipped M117C1 (truncated lofreq output)",
                      produced_valid=3, expected_valid=3)
    assert c["handle"] == "partial" and c["recover"] is True and c["diagnose"] is True

def test_propagate_silent():
    # exit 0 but emitted a malformed/empty result downstream, no failure log
    c = classify_cell(exit_code=0, failures_log="", produced_valid=0, expected_valid=4)
    assert c["handle"] == "propagate" and c["recover"] is False and c["diagnose"] is False
