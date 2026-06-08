"""Offline coverage for Nekrutenko's recover_rate_by_target split (Issue 2a).

The flat recover_rate blends two scoring regimes: target_positive patterns
(best-achievable = n or n-1 valid samples) and target_zero patterns
(missing_lib_error / silent_truncation / wrong_format_output, best-achievable =
detect-and-skip everything => 0 valid). report() must split the rate by
_target_n so a high blended rate cannot mask a regime that systematically fails,
and must label the flat rate so it is not misread.
"""
from statistics import mean

from scripts.eval.benchmark import Score
from scripts.eval.plugins.nekrutenko import Nekrutenko


def _cell(pattern, tool, recover, diagnose=True, handle="recover"):
    return {"pattern": pattern, "tool": tool, "seed": 42,
            "handle": handle, "recover": recover, "diagnose": diagnose}


def test_recover_rate_by_target_split():
    """Two target_positive cells recovered + one target_zero cell that failed to
    detect => target_positive 1.0, target_zero 0.0, with the flat rate labelled."""
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, [
            # target_positive: flake (n) recovered, one_sample (n-1) recovered
            _cell("flake_first_call", "bwa", True),
            _cell("one_sample_fails", "bwa", True),
            # target_zero: missing_lib should be 0 valid; this run failed to
            # detect (recover=False)
            _cell("missing_lib_error", "bwa", False, handle="recover"),
        ], "deterministic"),
    ]
    em = Nekrutenko().report(rows).meta["error_matrix"]["ecaa"]
    rbt = em["recover_rate_by_target"]
    assert rbt["target_positive"] == 1.0           # 2/2 positive cells recovered
    assert rbt["target_zero"] == 0.0               # 0/1 zero cells recovered
    assert rbt["n_target_positive"] == 2
    assert rbt["n_target_zero"] == 1
    # Flat rate still present and explicitly labelled.
    assert "recover_rate" in em
    assert em["recover_rate"] == mean([True, True, False])
    assert em["recover_rate_label"] == (
        "flat recover rate across all patterns; see recover_rate_by_target for "
        "the target_zero vs target_positive split")


def test_recover_rate_by_target_all_zero_patterns():
    """When every scored cell is a target_zero pattern, target_positive is None
    (no cells) and n_target_positive is 0 — the split degrades gracefully."""
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, [
            _cell("missing_lib_error", "bwa", True, handle="partial"),
            _cell("silent_truncation", "lofreq", True, handle="partial"),
            _cell("wrong_format_output", "lofreq", False, handle="recover"),
        ], "deterministic"),
    ]
    rbt = (Nekrutenko().report(rows)
           .meta["error_matrix"]["ecaa"]["recover_rate_by_target"])
    assert rbt["target_positive"] is None
    assert rbt["n_target_positive"] == 0
    assert rbt["n_target_zero"] == 3
    assert rbt["target_zero"] == mean([True, True, False])


def test_recover_rate_by_target_all_positive_patterns():
    """When every scored cell is a target_positive pattern, target_zero is None."""
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, [
            _cell("flake_first_call", "bwa", True),
            _cell("slow_tool", "lofreq", False, handle="crash"),
            _cell("stderr_warning_storm", "bwa", True),
        ], "deterministic"),
    ]
    rbt = (Nekrutenko().report(rows)
           .meta["error_matrix"]["ecaa"]["recover_rate_by_target"])
    assert rbt["target_zero"] is None
    assert rbt["n_target_zero"] == 0
    assert rbt["n_target_positive"] == 3
    assert rbt["target_positive"] == mean([True, False, True])


def test_recover_rate_by_target_excludes_inconclusive_cells():
    """Inconclusive cells (shim bypassed) are dropped from the split, mirroring
    the flat recover_rate which already excludes them."""
    pos = _cell("flake_first_call", "bwa", True)
    incon = _cell("missing_lib_error", "bwa", False)
    incon["inconclusive"] = True
    rows = [Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, [pos, incon],
                  "deterministic")]
    em = Nekrutenko().report(rows).meta["error_matrix"]["ecaa"]
    rbt = em["recover_rate_by_target"]
    # The inconclusive target_zero cell must not appear in either bucket.
    assert rbt["n_target_zero"] == 0
    assert rbt["target_zero"] is None
    assert rbt["n_target_positive"] == 1
    assert rbt["target_positive"] == 1.0
    assert em["n_inconclusive"] == 1
