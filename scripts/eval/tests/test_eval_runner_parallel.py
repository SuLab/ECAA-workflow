import json
from pathlib import Path
from scripts.eval import eval_runner
from scripts.eval.benchmark import (Arm, Benchmark, Output, RunSpec, Score,
                                     Scorecard, Task)


class _StubBench(Benchmark):
    """Deterministic stub: N tasks, no judge."""
    @property
    def name(self): return "stub"
    def fetch(self, cache_dir): return Path("/tmp")
    def tasks(self, handle, *, smoke):
        n = 1 if smoke else 2
        return [Task(task_id=f"t{i}", prompt="p", inputs={}, rubric=None,
                     answer_key=None, meta={}) for i in range(n)]
    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        return RunSpec(arm, workdir, "bare", "instr")
    def collect(self, spec, run_dir):
        return Output("", "", {}, exit_ok=True, wall_secs=1.0)
    def score(self, task, arm, output, trial):
        return Score(task.task_id, arm.value, trial, overall=42.0,
                     dimensions={}, jaccard=0.42, error_cells=None,
                     judge_id="deterministic", extra={"judge_cost_usd": 0.0})
    def report(self, scores):
        return Scorecard(self.name, scores, meta={})


def _patch_runs(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")
    monkeypatch.setenv("ECAA_EVAL_RUNS_DIR", str(tmp_path / "runs"))
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr(eval_runner, "PLUGINS", {"stub": _StubBench})
    monkeypatch.setattr(eval_runner.agent_runner, "run_bare",
                        lambda wd, instr, **kw: eval_runner.agent_runner.RunResult(True, 0.1, wd))


def test_parallel_run_writes_journal_and_scorecard(monkeypatch, tmp_path):
    _patch_runs(monkeypatch, tmp_path)
    rc = eval_runner.main(["stub", "--arms", "ecaa", "--trials", "2",
                           "--max-parallel", "4"])
    assert rc == 0
    runs = list((tmp_path / "runs").glob("stub-*"))
    assert len(runs) == 1
    journal = (runs[0] / "journal.jsonl").read_text().splitlines()
    base = [json.loads(l) for l in journal if json.loads(l)["kind"] == "base"]
    assert len(base) == 4  # 2 tasks x 1 arm x 2 trials
    assert (runs[0] / "scorecard.md").exists()


def test_resume_skips_completed_base_runs(monkeypatch, tmp_path):
    _patch_runs(monkeypatch, tmp_path)
    calls = {"n": 0}
    orig = eval_runner.run_base

    def counting(*a, **kw):
        calls["n"] += 1
        return orig(*a, **kw)

    monkeypatch.setattr(eval_runner, "run_base", counting)
    rc = eval_runner.main(["stub", "--arms", "ecaa", "--trials", "2",
                           "--max-parallel", "2"])
    assert rc == 0 and calls["n"] == 4
    run_dir = next((tmp_path / "runs").glob("stub-*"))
    calls["n"] = 0
    rc = eval_runner.main(["stub", "--arms", "ecaa", "--trials", "2",
                           "--max-parallel", "2", "--resume", str(run_dir)])
    assert rc == 0 and calls["n"] == 0
