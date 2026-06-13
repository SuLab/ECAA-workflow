# scripts/eval/tests/test_error_matrix_report.py
from scripts.eval.benchmark import Score
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.services.scorecard import write_scorecard


def _cells(rec):
    return [
        {
            "pattern": "flake_first_call",
            "tool": "bwa",
            "seed": s,
            "handle": "recover" if rec else "crash",
            "recover": rec,
            "diagnose": rec,
        }
        for s in (42, 43, 44)
    ]


def test_error_matrix_rollup_and_render(tmp_path):
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, _cells(True), "deterministic"),
        Score("mtdna", "claude-direct", 0, 50.0, {}, 0.5, _cells(False), "deterministic"),
    ]
    card = Nekrutenko().report(rows)
    em = card.meta["error_matrix"]
    assert em["ecaa"]["recover_rate"] == 1.0 and em["claude-direct"]["recover_rate"] == 0.0
    md = (write_scorecard(card, tmp_path) / "scorecard.md").read_text()
    assert "Error matrix" in md and "flake_first_call" in md


def test_errored_cells_recorded_inconclusive_not_dropped():
    """An arm whose cells ERRORED (e.g. the ECAA DAG blocked under fault
    injection) is recorded as inconclusive-with-reason: counted under
    n_inconclusive, the reason surfaced in inconclusive_reasons, and EXCLUDED
    from the recover/diagnose rates — never a silently-empty arm."""
    from scripts.eval.eval_runner import _errored_cell_record
    scored = _cells(True)  # 3 genuine scored cells (recover=diagnose=True)
    errored = [
        _errored_cell_record(("flake_first_call", "bwa", s),
                             RuntimeError("UnrunnablePackageError: 0 runnable tasks"))
        for s in (42, 43, 44)
    ]
    rows = [Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, scored + errored, "deterministic")]
    em = Nekrutenko().report(rows).meta["error_matrix"]["ecaa"]
    assert em["n_cells"] == 3            # only the scored cells
    assert em["n_inconclusive"] == 3     # the errored cells are COUNTED, not dropped
    assert any("UnrunnablePackageError" in r for r in em["inconclusive_reasons"])
    assert em["recover_rate"] == 1.0     # rate driven by scored cells only
    # A bare RuntimeError (the type name is NOT "UnrunnablePackage") classifies
    # as `unknown` — a potential arm limitation, not silently excluded as infra.
    assert em["inconclusive_kinds"] == {"infra": 0, "unknown": 3}


def test_report_surfaces_infra_vs_arm_inconclusive_kinds():
    """report() surfaces a per-arm infra-vs-arm tally so an infra failure
    (correctly excluded) is distinguishable from a potential arm-limitation that
    should NOT be silently excluded."""
    import subprocess
    from scripts.eval.eval_runner import _errored_cell_record
    scored = _cells(True)  # 3 genuine scored cells
    infra = [
        _errored_cell_record(("flake_first_call", "bwa", 10),
                             subprocess.TimeoutExpired(cmd="harness", timeout=7200)),
        _errored_cell_record(("flake_first_call", "bwa", 11),
                             OSError("No space left on device")),
    ]
    arm_limit = [
        _errored_cell_record(("flake_first_call", "bwa", 12),
                             RuntimeError("agent emitted no variant table")),
    ]
    rows = [Score("mtdna", "ecaa", 0, 100.0, {}, 1.0,
                  scored + infra + arm_limit, "deterministic")]
    em = Nekrutenko().report(rows).meta["error_matrix"]["ecaa"]
    assert em["n_inconclusive"] == 3
    assert em["inconclusive_kinds"] == {"infra": 2, "unknown": 1}
