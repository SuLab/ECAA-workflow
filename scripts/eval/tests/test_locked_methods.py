"""Per-plugin method-lock contract: recipe evals lock, open evals stay free."""
from __future__ import annotations
from pathlib import Path

from scripts.eval.benchmark import Arm, Task
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.plugins.nekrutenko import Nekrutenko


def _nekrut_task():
    return Task(task_id="mtdna", prompt="call variants", inputs={},
                rubric=None, answer_key=Path("/dev/null"), meta={})


def _biomni_task():
    return Task(task_id="da-x", prompt="analyze", inputs={},
                rubric={"criteria": []}, answer_key=None, meta={})


def test_nekrutenko_locks_nothing_on_ecaa_arm():
    # WS-E: the ECAA method-lock is dropped; discovery drives both arms.
    plug = Nekrutenko()
    assert plug.locked_methods(_nekrut_task(), Arm.ECAA_WORKFLOW) == []


def test_nekrutenko_locks_nothing_on_bare_arm():
    plug = Nekrutenko()
    assert plug.locked_methods(_nekrut_task(), Arm.CLAUDE_CODE_DIRECT) == []


def test_report_records_method_lock_dropped_in_meta():
    from scripts.eval.benchmark import Score
    rows = [
        Score("mtdna", "ecaa", 0, 100.0, {}, 1.0, [], "deterministic"),
        Score("mtdna", "claude-direct", 0, 50.0, {}, 0.5, [], "deterministic"),
    ]
    card = Nekrutenko().report(rows)
    ml = card.meta["method_lock"]
    assert ml["ecaa"]["any_locked"] is False
    assert ml["claude-direct"]["any_locked"] is False
    assert ml["asymmetric"] is False
    assert "DROPPED" in ml["note"]


def test_biomnibench_locks_nothing_either_arm():
    plug = BiomniBench()
    assert plug.locked_methods(_biomni_task(), Arm.ECAA_WORKFLOW) == []
    assert plug.locked_methods(_biomni_task(), Arm.CLAUDE_CODE_DIRECT) == []


def test_nekrutenko_rejects_hypothesized_proposals_for_recipe_fidelity():
    # Recipe eval: decline gap-fill nodes so the emitted DAG stays the pinned
    # bwa+lofreq reference recipe.
    assert Nekrutenko().proposal_policy(_nekrut_task(), Arm.ECAA_WORKFLOW) == "reject"


def test_biomnibench_signs_off_hypothesized_proposals():
    # Open eval: accept the LLM's gap-fill so ECAA's full compose behavior runs.
    assert BiomniBench().proposal_policy(_biomni_task(), Arm.ECAA_WORKFLOW) == "signoff"
