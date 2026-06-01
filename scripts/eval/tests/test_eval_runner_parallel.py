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


def test_failed_base_run_journaled_surfaced_and_retried_on_resume(monkeypatch, tmp_path):
    _patch_runs(monkeypatch, tmp_path)
    orig = eval_runner.run_base
    state = {"fail_t1": True}

    def maybe_fail(plugin, task, arm, trial, wd, mi):
        if task.task_id == "t1" and state["fail_t1"]:
            raise RuntimeError("boom t1")
        return orig(plugin, task, arm, trial, wd, mi)

    monkeypatch.setattr(eval_runner, "run_base", maybe_fail)
    rc = eval_runner.main(["stub", "--arms", "ecaa", "--trials", "1", "--max-parallel", "2"])
    assert rc == 0
    run_dir = next((tmp_path / "runs").glob("stub-*"))
    recs = [json.loads(l) for l in (run_dir / "journal.jsonl").read_text().splitlines()]
    failed = [r for r in recs if r["kind"] == "base_failed"]
    assert len(failed) == 1 and failed[0]["fail_of"] == "t1:ecaa:0" and "key" not in failed[0]
    card = json.loads((run_dir / "scorecard.json").read_text())
    assert card["meta"]["incomplete_scorecard"]["scored"] == 1
    assert "t1:ecaa:0" in card["meta"]["incomplete_scorecard"]["missing"]
    state["fail_t1"] = False
    calls = {"n": 0}

    def counting(*a, **k):
        calls["n"] += 1
        return orig(*a, **k)

    monkeypatch.setattr(eval_runner, "run_base", counting)
    rc = eval_runner.main(["stub", "--arms", "ecaa", "--trials", "1",
                           "--max-parallel", "2", "--resume", str(run_dir)])
    assert rc == 0 and calls["n"] == 1


def test_judged_row_with_no_verdict_left_unscored_not_re_judged(monkeypatch, tmp_path):
    """Every judge failing (empty verdicts) leaves the row unscored; the sync
    score() path is NOT invoked (no live re-judging)."""
    from scripts.eval.benchmark import Benchmark, Output, RunSpec, Score, Scorecard, Task
    from scripts.eval.services import judge as judge_mod

    class _JudgedStub(Benchmark):
        @property
        def name(self): return "jstub"
        def fetch(self, c): return tmp_path
        def tasks(self, h, *, smoke):
            return [Task("t0", "p", {}, rubric={"criteria": []}, answer_key=None)]
        def build_run(self, task, arm, wd):
            wd.mkdir(parents=True, exist_ok=True)
            return RunSpec(arm, wd, "bare", "i")
        def collect(self, spec, rd): return Output("tr", "an", {}, True, 0.0)
        def judge_requests(self, task, arm, out):
            return [{"role": "headline", "judge_id": "gemini-3.1-pro",
                     "rubric": task.rubric, "trace": out.trace_md, "answer": out.answer_txt}]
        def assemble_score(self, task, arm, out, trial, verdicts):
            return Score(task.task_id, arm.value, trial, 1.0, {}, None, None, "g", extra={})
        def score(self, task, arm, out, trial):
            raise AssertionError("score() must NOT be called on empty verdicts")
        def report(self, scores): return Scorecard(self.name, scores, meta={})

    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")
    monkeypatch.setenv("ECAA_EVAL_RUNS_DIR", str(tmp_path / "runs"))
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr(eval_runner, "PLUGINS", {"jstub": _JudgedStub})
    monkeypatch.setattr(eval_runner.agent_runner, "run_bare",
                        lambda wd, instr, **kw: eval_runner.agent_runner.RunResult(True, 0.1, wd))
    monkeypatch.setattr(judge_mod, "judge_batch", lambda reqs: {})  # all judges failed

    rc = eval_runner.main(["jstub", "--arms", "ecaa", "--trials", "1", "--max-parallel", "1"])
    assert rc == 1  # row unscored -> arm has zero rows -> surfaced, not silently re-judged
