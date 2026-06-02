# scripts/eval/tests/test_error_matrix.py
#
# classify_cell is a faithful port of the Nekrutenko reference scorer's
# error_handling() (score/score_run.py): m_recover = (n_valid == target_n) with
# a PATTERN-SPECIFIC target_n and EXACT equality; m_handle is the 4-category
# histogram (recover requires ALL samples, then partial/propagate/crash); and
# m_diagnose ORs failures.log + a "N/M samples" summary line + a sample/fail-word
# mention over the agent's exec log.
from scripts.eval.scoring.error_matrix import classify_cell


# ── m_recover: pattern-specific target_n, exact equality ─────────────────────

def test_recover_target4_exact():
    # flake/slow/warning patterns expect all 4 back; exact equality.
    assert classify_cell(exit_code=0, failures_log="", produced_valid=4,
                         target_n=4)["recover"] is True
    assert classify_cell(exit_code=0, failures_log="", produced_valid=3,
                         target_n=4)["recover"] is False
    # Over-count (a stray extra VCF) must NOT be credited as recovery.
    assert classify_cell(exit_code=0, failures_log="", produced_valid=5,
                         target_n=4)["recover"] is False


def test_recover_target3_one_sample_fails():
    # one_sample_fails: best achievable is 3 (the injected sample can't pass).
    assert classify_cell(exit_code=0, failures_log="skipped M117C1-ch",
                         produced_valid=3, target_n=3)["recover"] is True
    # A run that somehow emits 4 "valid" outputs did not honor the injection.
    assert classify_cell(exit_code=0, failures_log="", produced_valid=4,
                         target_n=3)["recover"] is False


def test_recover_target0_nothing_should_work():
    # missing_lib_error / silent_truncation / wrong_format_output: best
    # achievable is 0 valid samples (detect-and-skip everything).
    assert classify_cell(exit_code=0, failures_log="all samples truncated",
                         produced_valid=0, target_n=0)["recover"] is True
    # Header-only files counted as present (n_valid=4) is the OPPOSITE of best:
    # the run failed to detect the corruption.
    assert classify_cell(exit_code=0, failures_log="", produced_valid=4,
                         target_n=0)["recover"] is False


# ── m_handle: 4-category histogram (reference ladder) ────────────────────────

def test_handle_recover_requires_all_samples():
    # handle="recover" means ALL samples produced output, independent of the
    # pattern's recover target_n.
    c = classify_cell(exit_code=0, failures_log="", produced_valid=4, target_n=0)
    assert c["handle"] == "recover"


def test_handle_partial_when_structurally_detected():
    c = classify_cell(exit_code=0, failures_log="skipped M117C1-ch (truncated)",
                      produced_valid=3, target_n=3)
    assert c["handle"] == "partial"


def test_handle_propagate_when_some_output_and_nonzero_exit_no_log():
    c = classify_cell(exit_code=1, failures_log="", produced_valid=2, target_n=4)
    assert c["handle"] == "propagate"


def test_handle_crash_when_no_output_no_detection():
    c = classify_cell(exit_code=127, failures_log="", produced_valid=0, target_n=0)
    assert c["handle"] == "crash"


# ── m_diagnose: failures.log OR summary line OR sample/fail-word in exec_log ──

def test_diagnose_from_failures_log():
    c = classify_cell(exit_code=0, failures_log="lofreq failed on M117C1-ch",
                      produced_valid=3, target_n=3)
    assert c["diagnose"] is True


def test_diagnose_from_summary_line_in_exec_log():
    # The reference reads a "3/4 samples" style summary off the run's exec log.
    c = classify_cell(exit_code=0, failures_log="", produced_valid=3, target_n=3,
                      exec_log="processed 3/4 samples successfully")
    assert c["diagnose"] is True
    assert "summary_line" in c["diagnose_signals"]


def test_diagnose_from_sample_failure_mention_in_exec_log():
    c = classify_cell(exit_code=0, failures_log="", produced_valid=3, target_n=3,
                      exec_log="ERROR: sample M117C1-ch failed to align",
                      samples=("M117-bl", "M117-ch", "M117C1-bl", "M117C1-ch"))
    assert c["diagnose"] is True
    assert "sample_failure_mentioned" in c["diagnose_signals"]


def test_no_diagnose_when_silent():
    c = classify_cell(exit_code=0, failures_log="", produced_valid=0, target_n=4,
                      exec_log="done")
    assert c["diagnose"] is False
    # Reference ladder: no output + no structural detection falls through to the
    # else branch -> crash (even on a clean exit 0).
    assert c["handle"] == "crash"
