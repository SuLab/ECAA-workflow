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


def test_nekrutenko_locks_bwa_and_lofreq_on_ecaa_arm():
    plug = Nekrutenko()
    locked = plug.locked_methods(_nekrut_task(), Arm.ECAA_WORKFLOW)
    assert locked == [("alignment", "bwa"), ("variant_calling", "lofreq")]


def test_nekrutenko_locks_nothing_on_bare_arm():
    plug = Nekrutenko()
    assert plug.locked_methods(_nekrut_task(), Arm.CLAUDE_CODE_DIRECT) == []


def test_biomnibench_locks_nothing_either_arm():
    plug = BiomniBench()
    assert plug.locked_methods(_biomni_task(), Arm.ECAA_WORKFLOW) == []
    assert plug.locked_methods(_biomni_task(), Arm.CLAUDE_CODE_DIRECT) == []
