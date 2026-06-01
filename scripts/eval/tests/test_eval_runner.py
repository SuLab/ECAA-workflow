# scripts/eval/tests/test_eval_runner.py
from pathlib import Path
from scripts.eval import eval_runner  # see Step 3: module named eval_runner.py
from scripts.eval import benchmark as bench_mod
from scripts.eval.benchmark import Arm, Output, Score, Scorecard, Task
from scripts.eval.services import judge as judge_mod

def test_registry_has_both():
    assert set(eval_runner.PLUGINS) == {"biomnibench", "nekrutenko"}

def test_skip_without_live_flag(monkeypatch, capsys):
    monkeypatch.delenv("ECAA_EVAL_LIVE", raising=False)
    rc = eval_runner.main(["biomnibench", "--smoke"])
    assert rc == 0
    assert "SKIP" in capsys.readouterr().out

def test_stage_inputs_copies_into_package(tmp_path):
    """_stage_inputs creates pkg/inputs/ and copies each source file into it."""
    # Create fake source input files in a separate directory.
    src_dir = tmp_path / "sources"
    src_dir.mkdir()
    file_a = src_dir / "sample_A.fastq"
    file_b = src_dir / "sample_B.fastq"
    file_a.write_text("@read1\nACGT\n+\nIIII\n")
    file_b.write_text("@read2\nTGCA\n+\nIIII\n")

    pkg_dir = tmp_path / "pkg"
    pkg_dir.mkdir()

    inputs = {"sample_A": file_a, "sample_B": file_b}
    eval_runner._stage_inputs(pkg_dir, inputs)

    inputs_dir = pkg_dir / "inputs"
    assert inputs_dir.is_dir(), "inputs/ subdirectory should be created"
    assert (inputs_dir / "sample_A.fastq").exists(), "sample_A.fastq should be copied"
    assert (inputs_dir / "sample_B.fastq").exists(), "sample_B.fastq should be copied"
    assert (inputs_dir / "sample_A.fastq").read_text() == file_a.read_text()
    assert (inputs_dir / "sample_B.fastq").read_text() == file_b.read_text()

def test_stage_inputs_skips_missing_source(tmp_path):
    """_stage_inputs silently skips input files whose source path does not exist."""
    pkg_dir = tmp_path / "pkg"
    pkg_dir.mkdir()
    missing = tmp_path / "nonexistent.fastq"  # deliberately not created
    existing = tmp_path / "real.fastq"
    existing.write_text("data")

    eval_runner._stage_inputs(pkg_dir, {"miss": missing, "real": existing})

    inputs_dir = pkg_dir / "inputs"
    assert not (inputs_dir / "nonexistent.fastq").exists(), "missing source should be skipped"
    assert (inputs_dir / "real.fastq").exists(), "existing source should be copied"


def test_isolated_pkg_copy(tmp_path):
    """_isolated_pkg_copy produces a distinct directory with the same content."""
    # Build a fake source package tree.
    src = tmp_path / "src_pkg"
    src.mkdir()
    (src / "WORKFLOW.json").write_text('{"tasks": []}')
    runtime = src / "runtime"
    runtime.mkdir()
    (runtime / "state.json").write_text('{}')

    dest = tmp_path / "cell0"
    result = eval_runner._isolated_pkg_copy(src, dest)

    assert result == dest, "return value should be the dest path"
    assert dest.exists(), "dest directory should exist after copy"
    assert dest != src, "dest must be a different path from src"
    assert (dest / "WORKFLOW.json").exists(), "WORKFLOW.json should be copied"
    assert (dest / "WORKFLOW.json").read_text() == '{"tasks": []}'
    assert (dest / "runtime" / "state.json").exists(), "runtime/ subdir should be copied"


# ---------------------------------------------------------------------------
# Two-phase orchestration: fake plugin + monkeypatched judge_batch
# ---------------------------------------------------------------------------

_RUBRIC = {
    "criteria": [
        {"id": "c1", "dimension": "method", "points": 4,
         "levels": {"A": 1.0, "B": 0.5, "C": 0.0}},
    ]
}


class _FakeRunResult:
    exit_ok = True
    wall_secs = 0.5
    stdout = ""


class _FakeBenchmark(bench_mod.Benchmark):
    """Minimal Benchmark implementation for two-phase orchestration tests."""

    @property
    def name(self):
        return "fake"

    def fetch(self, cache_dir):
        return Path("/fake")

    def tasks(self, handle, *, smoke):
        return [
            Task("t1", "prompt1", {}, _RUBRIC, None),
            Task("t2", "prompt2", {}, _RUBRIC, None),
        ]

    def build_run(self, task, arm, workdir):
        from scripts.eval.benchmark import RunSpec
        workdir.mkdir(parents=True, exist_ok=True)
        return RunSpec(arm, workdir, "bare", task.prompt)

    def collect(self, spec, run_dir):
        return Output(trace_md=f"trace-{spec.arm.value}",
                      answer_txt="ans", artifacts={}, exit_ok=True, wall_secs=0.0)

    def score(self, task, arm, output, trial):
        # Synchronous fallback — should not be called when judge_requests is non-empty.
        return Score(task.task_id, arm.value, trial, 0.0, {}, None, None,
                     "deterministic", extra={"judge_cost_usd": 0.0})

    def judge_requests(self, task, arm, output):
        return [
            {"role": "headline", "judge_id": "gemini-3.1-pro",
             "rubric": _RUBRIC, "trace": output.trace_md, "answer": output.answer_txt},
            {"role": "cross", "judge_id": "anthropic-opus",
             "rubric": _RUBRIC, "trace": output.trace_md, "answer": output.answer_txt},
        ]

    def assemble_score(self, task, arm, output, trial, verdicts):
        headline = verdicts["headline"]
        cross = verdicts["cross"]
        return Score(task.task_id, arm.value, trial,
                     headline["overall"], headline["dimensions"],
                     None, None, "gemini-3.1-pro",
                     extra={"cross_check": cross["overall"],
                            "judge_cost_usd": headline.get("cost_usd", 0) + cross.get("cost_usd", 0)})

    def report(self, scores):
        return Scorecard(benchmark=self.name, rows=scores,
                         meta={"judge_agreement": {"exact": 1.0, "kappa": 1.0}})

    def error_matrix(self, task, arm, workdir, run_fn):
        return None


def _make_canned_verdict(overall: float) -> dict:
    return {"overall": overall, "dimensions": {"method": overall},
            "levels": {"c1": "A"}, "cost_usd": 0.01}


def test_two_phase_assembles_scores_from_batch(tmp_path, monkeypatch):
    """Two-phase orchestration calls judge_batch once and assembles scores per item."""
    # Redirect cache so no real FS side-effects.
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")

    # Track what judge_batch receives.
    captured_requests = []

    def fake_judge_batch(reqs):
        captured_requests.extend(reqs)
        # Return canned verdicts for every key.
        return {req["key"]: _make_canned_verdict(88.0) for req in reqs}

    monkeypatch.setattr(judge_mod, "judge_batch", fake_judge_batch)

    # Monkey-patch run_bare so no subprocess is spawned.
    monkeypatch.setattr(eval_runner.agent_runner, "run_bare",
                        lambda wd, instr, **kw: _FakeRunResult())

    # Swap BiomniBench for our fake plugin.
    monkeypatch.setitem(eval_runner.PLUGINS, "biomnibench", _FakeBenchmark)

    # Capture write_scorecard so we don't write to the real FS.
    written = []
    monkeypatch.setattr(eval_runner, "write_scorecard",
                        lambda card, out_dir: written.append(card))

    rc = eval_runner.main(["biomnibench", "--smoke", "--arms", "claude-direct",
                           "--trials", "1"])
    assert rc == 0, "main should exit 0"

    # One call to fake_judge_batch with requests from both tasks.
    # 2 tasks × 1 arm × 1 trial × 2 roles = 4 judge requests.
    assert len(captured_requests) == 4, \
        f"expected 4 judge requests, got {len(captured_requests)}"

    # Each request should have a "key" of the form "<idx>:<role>".
    roles_seen = {req["role"] for req in captured_requests}
    assert roles_seen == {"headline", "cross"}

    # Scorecard was written with 2 scores (one per task).
    assert len(written) == 1
    card = written[0]
    assert len(card.rows) == 2
    for s in card.rows:
        assert s.overall == 88.0
        assert s.judge_id == "gemini-3.1-pro"
        assert "cross_check" in s.extra


def test_two_phase_fallback_to_sync_score_when_no_judge_requests(tmp_path, monkeypatch):
    """When a plugin returns no judge_requests, score() is called synchronously."""
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")

    batch_calls = []

    def fake_judge_batch(reqs):
        batch_calls.extend(reqs)
        return {}

    monkeypatch.setattr(judge_mod, "judge_batch", fake_judge_batch)

    class _NoBatchPlugin(_FakeBenchmark):
        """Plugin that produces no judge requests (e.g. Nekrutenko-style)."""
        def judge_requests(self, task, arm, output):
            return []

        def score(self, task, arm, output, trial):
            return Score(task.task_id, arm.value, trial, 42.0, {}, None, None,
                         "deterministic", extra={"judge_cost_usd": 0.0})

    monkeypatch.setattr(eval_runner.agent_runner, "run_bare",
                        lambda wd, instr, **kw: _FakeRunResult())
    monkeypatch.setitem(eval_runner.PLUGINS, "biomnibench", _NoBatchPlugin)

    written = []
    monkeypatch.setattr(eval_runner, "write_scorecard",
                        lambda card, out_dir: written.append(card))

    rc = eval_runner.main(["biomnibench", "--smoke", "--arms", "claude-direct", "--trials", "1"])
    assert rc == 0

    # judge_batch should NOT have been called (no requests accumulated).
    assert batch_calls == [], "judge_batch should not be called when no requests"

    card = written[0]
    for s in card.rows:
        assert s.overall == 42.0
        assert s.judge_id == "deterministic"
