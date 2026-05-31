# scripts/eval/tests/test_benchmark.py
from pathlib import Path
from scripts.eval.benchmark import Arm, Task, RunSpec, Output, Score, Scorecard

def test_arm_values():
    assert Arm.ECAA_WORKFLOW.value == "ecaa"
    assert Arm.CLAUDE_CODE_DIRECT.value == "claude-direct"

def test_dataclasses_roundtrip():
    t = Task(task_id="t1", prompt="do X", inputs={"a": Path("/tmp/a")},
             rubric=None, answer_key=None, meta={})
    assert t.task_id == "t1"
    s = Score(task_id="t1", arm="ecaa", trial=0, overall=80.0,
              dimensions={"method_selection": 60.0}, jaccard=None,
              error_cells=None, judge_id="gemini-3.1-pro", extra={})
    sc = Scorecard(benchmark="biomnibench", rows=[s], meta={})
    assert sc.rows[0].overall == 80.0
