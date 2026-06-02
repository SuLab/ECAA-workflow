# scripts/eval/tests/test_contamination_control.py
from pathlib import Path
from scripts.eval.benchmark import Arm, Benchmark
from scripts.eval.plugins.biomnibench import BiomniBench
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.eval_runner import _append_agent_directive


def test_base_benchmark_directive_is_none():
    # The default is opt-in: a plugin must override to enable the directive.
    assert Benchmark.contamination_directive.__doc__ is not None

    class _Stub(Benchmark):
        name = "stub"
        def fetch(self, c): ...
        def tasks(self, h, *, smoke): ...
        def build_run(self, t, a, w): ...
        def collect(self, s, r): ...
        def score(self, t, a, o, tr): ...
        def report(self, s): ...
    assert _Stub().contamination_directive() is None


def test_biomnibench_directive_present_and_specific():
    d = BiomniBench().contamination_directive()
    assert d and "do not" in d.lower()
    # Names the integrity intent without leaking method choices.
    assert "source" in d.lower() and "data" in d.lower()


def test_nekrutenko_directive_is_none():
    # Deterministic VCF task: no paper answer-key to leak; directive stays off.
    assert Nekrutenko().contamination_directive() is None


def test_bare_arm_prompt_includes_directive(tmp_path):
    from scripts.eval.benchmark import Task
    task = Task(task_id="da-1-1", prompt="Do the analysis.", inputs={},
                rubric={"scoring": "absolute", "criteria": []}, answer_key=None, meta={})
    spec = BiomniBench().build_run(task, Arm.CLAUDE_CODE_DIRECT, tmp_path / "wd")
    assert BiomniBench().contamination_directive() in spec.instruction


def test_append_agent_directive_idempotent(tmp_path):
    pkg = tmp_path / "pkg"; pkg.mkdir()
    (pkg / "PROMPT.md").write_text("# Agent Instructions\n\nDo the task.\n")
    directive = "## Evaluation integrity\nWork only from provided data."
    _append_agent_directive(pkg, directive)
    body = (pkg / "PROMPT.md").read_text()
    assert directive.strip() in body
    assert body.startswith("# Agent Instructions")  # original preserved
    # Second call must not duplicate it.
    _append_agent_directive(pkg, directive)
    assert (pkg / "PROMPT.md").read_text().count("Evaluation integrity") == 1


def test_append_agent_directive_none_is_noop(tmp_path):
    pkg = tmp_path / "pkg"; pkg.mkdir()
    (pkg / "PROMPT.md").write_text("# Agent Instructions\n")
    _append_agent_directive(pkg, None)
    assert (pkg / "PROMPT.md").read_text() == "# Agent Instructions\n"
