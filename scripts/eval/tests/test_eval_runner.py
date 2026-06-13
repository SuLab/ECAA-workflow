# scripts/eval/tests/test_eval_runner.py
import json
import pytest
from pathlib import Path
from scripts.eval import eval_runner  # see Step 3: module named eval_runner.py
from scripts.eval import benchmark as bench_mod
from scripts.eval.benchmark import Arm, Output, RunSpec, Score, Scorecard, Task
from scripts.eval.services import judge as judge_mod
from scripts.eval.services.journal import Journal

def test_registry_has_both():
    assert set(eval_runner.PLUGINS) == {"biomnibench", "nekrutenko"}


def test_errored_cell_record_is_inconclusive_with_reason():
    """A cell that RAISES is converted to an inconclusive-with-reason record so
    the matrix can never silently lose a cell for any arm/atom (the defect that
    dropped all 12 ECAA Nekrutenko cells)."""
    rec = eval_runner._errored_cell_record(
        ("flake_first_call", "bwa", 42),
        TimeoutError("harness exceeded 7200s"))
    assert rec["inconclusive"] is True
    assert rec["shim_invoked"] is False
    assert (rec["pattern"], rec["tool"], rec["seed"]) == ("flake_first_call", "bwa", 42)
    assert "TimeoutError" in rec["error"] and "7200s" in rec["error"]


# --- Scoped error-cell reset (efficiency fix): re-run only the fault-targeted
# recipe stage + downstream, reusing the base run's upstream outputs ---

def test_downstream_closure_includes_anchor_and_dependents():
    tasks = {
        "data_acquisition": {"depends_on": []},
        "alignment": {"depends_on": ["data_acquisition"]},
        "variant_calling": {"depends_on": ["alignment"]},
        "validate_variant_calling": {"depends_on": ["variant_calling"]},
        "variant_filtering": {"depends_on": ["variant_calling"]},
    }
    # lofreq anchor: only variant_calling + what consumes it.
    assert eval_runner._downstream_closure(tasks, "variant_calling") == {
        "variant_calling", "validate_variant_calling", "variant_filtering"}
    # bwa anchor: alignment + everything downstream (propagates to the scored VCF).
    assert eval_runner._downstream_closure(tasks, "alignment") == {
        "alignment", "variant_calling", "validate_variant_calling", "variant_filtering"}
    # upstream is NOT in the closure (reused, not re-run).
    assert "data_acquisition" not in eval_runner._downstream_closure(tasks, "alignment")
    # absent anchor -> empty (caller falls back to full reset).
    assert eval_runner._downstream_closure(tasks, "absent") == set()


def test_scoped_reset_keeps_upstream_completed_and_outputs(tmp_path):
    wf = {"tasks": {
        "data_acquisition": {"state": {"status": "completed"}, "depends_on": []},
        "alignment": {"state": {"status": "completed"}, "depends_on": ["data_acquisition"]},
        "variant_calling": {"state": {"status": "blocked"}, "depends_on": ["alignment"]},
        "variant_filtering": {"state": {"status": "pending"}, "depends_on": ["variant_calling"]},
    }}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    out = tmp_path / "runtime" / "outputs"
    for tid in ("data_acquisition", "alignment", "variant_calling"):
        (out / tid).mkdir(parents=True)
        (out / tid / "result.json").write_text("{}")

    n = eval_runner._scoped_reset_from_anchor(tmp_path, "variant_calling")
    assert n == 2  # variant_calling + variant_filtering

    data = json.loads((tmp_path / "WORKFLOW.json").read_text())["tasks"]
    # Upstream kept terminal, outputs STAGED (reused, not re-run).
    assert data["data_acquisition"]["state"] == {"status": "completed"}
    assert data["alignment"]["state"] == {"status": "completed"}
    assert (out / "alignment" / "result.json").exists()
    assert (out / "data_acquisition" / "result.json").exists()
    # Anchor + downstream reset to pending; their stale outputs cleared.
    assert data["variant_calling"]["state"] == {"status": "pending"}
    assert data["variant_filtering"]["state"] == {"status": "pending"}
    assert not (out / "variant_calling").exists()


def test_scoped_reset_absent_anchor_no_mutation(tmp_path):
    wf = {"tasks": {"alignment": {"state": {"status": "completed"}, "depends_on": []}}}
    (tmp_path / "WORKFLOW.json").write_text(json.dumps(wf))
    assert eval_runner._scoped_reset_from_anchor(tmp_path, "nope") == 0
    data = json.loads((tmp_path / "WORKFLOW.json").read_text())["tasks"]
    assert data["alignment"]["state"] == {"status": "completed"}  # untouched


def test_isolated_pkg_copy_scoped_vs_full(tmp_path):
    src = tmp_path / "src"
    src.mkdir()
    wf = {"tasks": {
        "alignment": {"state": {"status": "completed"}, "depends_on": []},
        "variant_calling": {"state": {"status": "completed"}, "depends_on": ["alignment"]},
    }}
    (src / "WORKFLOW.json").write_text(json.dumps(wf))
    (src / "runtime" / "outputs" / "alignment").mkdir(parents=True)
    (src / "runtime" / "outputs" / "alignment" / "bam.txt").write_text("BAM")

    # Scoped: alignment kept completed + its BAM staged; only variant_calling reset.
    d1 = eval_runner._isolated_pkg_copy(src, tmp_path / "scoped", reset_anchor="variant_calling")
    s = json.loads((d1 / "WORKFLOW.json").read_text())["tasks"]
    assert s["alignment"]["state"] == {"status": "completed"}
    assert (d1 / "runtime" / "outputs" / "alignment" / "bam.txt").exists()
    assert s["variant_calling"]["state"] == {"status": "pending"}

    # Full (no anchor): everything reset to pending, all outputs deleted.
    d2 = eval_runner._isolated_pkg_copy(src, tmp_path / "full", reset_anchor=None)
    f = json.loads((d2 / "WORKFLOW.json").read_text())["tasks"]
    assert f["alignment"]["state"] == {"status": "pending"}
    assert not (d2 / "runtime" / "outputs").exists()

    # Anchor absent from the DAG -> falls back to full reset.
    d3 = eval_runner._isolated_pkg_copy(src, tmp_path / "fallback", reset_anchor="nonexistent")
    f3 = json.loads((d3 / "WORKFLOW.json").read_text())["tasks"]
    assert f3["alignment"]["state"] == {"status": "pending"}
    assert not (d3 / "runtime" / "outputs").exists()


def test_nekrutenko_recipe_stage_for_tool_map():
    from scripts.eval.plugins.nekrutenko import _RECIPE_STAGE_FOR_TOOL
    assert _RECIPE_STAGE_FOR_TOOL["bwa"] == "alignment"
    assert _RECIPE_STAGE_FOR_TOOL["lofreq"] == "variant_calling"


def test_errored_cell_record_tags_error_kind():
    """error_kind classifies an environmental/harness failure (`infra`, correctly
    excluded from arm comparison) distinctly from a potential arm-limitation
    (`unknown`, must NOT be silently excluded)."""
    import subprocess
    # infra: TimeoutError
    infra = eval_runner._errored_cell_record(
        ("flake_first_call", "bwa", 42), TimeoutError("harness exceeded 7200s"))
    assert infra["error_kind"] == "infra"
    # infra: subprocess.TimeoutExpired
    infra2 = eval_runner._errored_cell_record(
        ("slow_tool", "lofreq", 1),
        subprocess.TimeoutExpired(cmd="harness", timeout=7200))
    assert infra2["error_kind"] == "infra"
    # infra: OSError / disk-space
    infra3 = eval_runner._errored_cell_record(
        ("flake_first_call", "bwa", 2), OSError("No space left on device"))
    assert infra3["error_kind"] == "infra"

    # infra: package-level UnrunnablePackage classified by type-name (no import).
    class UnrunnablePackageError(Exception):
        pass
    infra4 = eval_runner._errored_cell_record(
        ("flake_first_call", "bwa", 3), UnrunnablePackageError("blocked DAG"))
    assert infra4["error_kind"] == "infra"

    # unknown: a generic RuntimeError might be an arm limitation, not infra.
    unknown = eval_runner._errored_cell_record(
        ("flake_first_call", "bwa", 43), RuntimeError("agent produced no VCF"))
    assert unknown["error_kind"] == "unknown"

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
                        lambda card, out_dir, **_kw: written.append(card))

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
                        lambda card, out_dir, **_kw: written.append(card))

    rc = eval_runner.main(["biomnibench", "--smoke", "--arms", "claude-direct", "--trials", "1"])
    assert rc == 0

    # judge_batch should NOT have been called (no requests accumulated).
    assert batch_calls == [], "judge_batch should not be called when no requests"

    card = written[0]
    for s in card.rows:
        assert s.overall == 42.0
        assert s.judge_id == "deterministic"


# ---------------------------------------------------------------------------
# WS0.1: _isolated_pkg_copy resets task states + clears runtime/outputs
# ---------------------------------------------------------------------------

def test_isolated_pkg_copy_resets_task_states_to_pending(tmp_path):
    """A cell copy must start every task at pending (not whatever terminal state
    the base run left), or the harness sees a fully-completed DAG and never
    re-runs — silently scoring the cell against the base run's clean outputs."""
    src = tmp_path / "src_pkg"
    src.mkdir()
    (src / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "align": {"state": {"status": "completed"}},
        "call": {"state": {"status": "blocked",
                           "record": {"reason": "[validation_failed] x"}}},
        "report": {"state": {"status": "failed"}},
    }}))

    dest = tmp_path / "cell0"
    eval_runner._isolated_pkg_copy(src, dest)

    data = json.loads((dest / "WORKFLOW.json").read_text())
    for tid, t in data["tasks"].items():
        assert t["state"] == {"status": "pending"}, f"{tid} not reset to pending"


def test_isolated_pkg_copy_deletes_outputs_preserves_inputs_and_reference(tmp_path):
    """Cell copies must drop the base run's runtime/outputs/* (stale VCFs/result
    json) but KEEP inputs/ and any staged reference so the re-run has its data."""
    src = tmp_path / "src_pkg"
    (src / "runtime" / "outputs" / "align").mkdir(parents=True)
    (src / "runtime" / "outputs" / "align" / "result.json").write_text("{}")
    (src / "runtime").joinpath("state.json").write_text("{}")
    (src / "inputs").mkdir()
    (src / "inputs" / "M117-bl.fq.gz").write_text("reads")
    (src / "inputs" / "chrM.fa.gz").write_text("ref")
    (src / "WORKFLOW.json").write_text(json.dumps({"tasks": {}}))

    dest = tmp_path / "cell0"
    eval_runner._isolated_pkg_copy(src, dest)

    assert not (dest / "runtime" / "outputs").exists(), "stale outputs not cleared"
    assert (dest / "runtime" / "state.json").exists(), "other runtime/ files kept"
    assert (dest / "inputs" / "M117-bl.fq.gz").read_text() == "reads"
    assert (dest / "inputs" / "chrM.fa.gz").read_text() == "ref"


# ---------------------------------------------------------------------------
# WS0.2: _ready_or_pending_task_count
# ---------------------------------------------------------------------------

def test_ready_or_pending_task_count_counts_runnable_states(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "pending"}},
        "b": {"state": {"status": "ready"}},
        "c": {"state": {"status": "completed"}},
        "d": {"state": {"status": "blocked", "record": {"reason": "x"}}},
    }}))
    assert eval_runner._ready_or_pending_task_count(pkg) == 2


def test_ready_or_pending_task_count_zero_when_all_terminal(tmp_path):
    pkg = tmp_path / "pkg"
    pkg.mkdir()
    (pkg / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}},
        "b": {"state": {"status": "failed"}},
    }}))
    assert eval_runner._ready_or_pending_task_count(pkg) == 0


def test_ready_or_pending_task_count_zero_on_missing_file(tmp_path):
    assert eval_runner._ready_or_pending_task_count(tmp_path / "nope") == 0


# ---------------------------------------------------------------------------
# WS0.3: _ensure_package_for_cells rejects a non-runnable reused dir
# ---------------------------------------------------------------------------

class _EcaaCellPlugin(bench_mod.Benchmark):
    """Minimal ECAA plugin: build_run returns an ecaa_package spec so the cell
    prep path (not the bare passthrough) is exercised."""

    @property
    def name(self):
        return "ecaa-cell"

    def fetch(self, cache_dir):
        return Path("/fake")

    def tasks(self, handle, *, smoke):
        return [Task("t1", "prompt1", {}, None, None)]

    def build_run(self, task, arm, workdir):
        workdir.mkdir(parents=True, exist_ok=True)
        return RunSpec(arm, workdir, "ecaa_package", task.prompt)

    def collect(self, spec, run_dir):
        return Output("", "", {}, True, 0.0)

    def score(self, task, arm, output, trial):
        return Score(task.task_id, arm.value, trial, 0.0, {}, None, None, "det", extra={})

    def judge_requests(self, task, arm, output):
        return []

    def report(self, scores):
        return Scorecard(benchmark=self.name, rows=scores, meta={})


def _cell_task():
    return Task("t1", "prompt1", {}, None, None)


def test_ensure_package_for_cells_accepts_terminal_reused_dir(monkeypatch, tmp_path):
    """A journal-reused base package is normally fully terminal (the base ran to
    completion). _ensure_package_for_cells accepts it as a copy SOURCE — each cell
    copies it via _isolated_pkg_copy, which resets task states to pending; the
    >=1-runnable-task gate is enforced post-reset in _cell_run_fn, not here."""
    existing = tmp_path / "prior-pkg"
    existing.mkdir()
    (existing / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}},
        "b": {"state": {"status": "failed"}},
    }}))
    base_rec = {"package_dir": str(existing), "session_id": "sid-old"}

    # The reused-dir branch must NOT re-drive intake.
    monkeypatch.setattr(eval_runner, "_chat_intake_or_cli",
                        lambda *a, **k: (_ for _ in ()).throw(
                            AssertionError("should not re-drive intake")))

    spec = eval_runner._ensure_package_for_cells(
        _EcaaCellPlugin(), _cell_task(), Arm.ECAA_WORKFLOW, base_rec,
        tmp_path / "wd", None)
    assert spec.package_dir == existing
    assert spec.session_id == "sid-old"


def test_ensure_package_for_cells_accepts_runnable_reused_dir(monkeypatch, tmp_path):
    existing = tmp_path / "prior-pkg"
    existing.mkdir()
    (existing / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}},
        "b": {"state": {"status": "pending"}},
    }}))
    base_rec = {"package_dir": str(existing), "session_id": "sid-old"}
    monkeypatch.setattr(eval_runner, "_chat_intake_or_cli",
                        lambda *a, **k: (_ for _ in ()).throw(
                            AssertionError("should not re-drive intake")))

    spec = eval_runner._ensure_package_for_cells(
        _EcaaCellPlugin(), _cell_task(), Arm.ECAA_WORKFLOW, base_rec,
        tmp_path / "wd", None)
    assert spec.package_dir == existing


# ---------------------------------------------------------------------------
# WS0.4: _cell_run_fn marks the cell errored when the copy is not runnable
# ---------------------------------------------------------------------------

def test_cell_run_fn_raises_when_copy_not_runnable(tmp_path):
    """After the per-cell copy, if there is nothing the harness can run we raise
    UnrunnablePackageError so the cell is recorded errored, not as a clean pass
    against stale outputs. A `tasks: []` package resets to 0 runnable tasks."""
    src = tmp_path / "src_pkg"
    src.mkdir()
    (src / "WORKFLOW.json").write_text(json.dumps({"tasks": []}))
    spec = RunSpec(Arm.ECAA_WORKFLOW, src, "ecaa_package", "instr")
    spec.package_dir = src

    fn = eval_runner._cell_run_fn(spec, max_iter=3)
    with pytest.raises(eval_runner.UnrunnablePackageError):
        fn(tmp_path / "cell", {"PATH": "/x"})


def test_cell_run_fn_invokes_harness_when_runnable(monkeypatch, tmp_path):
    src = tmp_path / "src_pkg"
    src.mkdir()
    # completed in the source — the per-cell copy resets it to pending, so the
    # copy IS runnable and the harness must be invoked.
    (src / "WORKFLOW.json").write_text(json.dumps({"tasks": {
        "a": {"state": {"status": "completed"}}}}))
    spec = RunSpec(Arm.ECAA_WORKFLOW, src, "ecaa_package", "instr")
    spec.package_dir = src

    seen = {}

    def _fake_run(pkg, **kw):
        seen["pkg"] = pkg
        return object()

    monkeypatch.setattr(eval_runner.agent_runner, "run_ecaa_package", _fake_run)
    fn = eval_runner._cell_run_fn(spec, max_iter=3)
    fn(tmp_path / "cell", {"PATH": "/x"})
    assert seen["pkg"] == tmp_path / "cell" / "pkg"


# ---------------------------------------------------------------------------
# WS0.5: agent-claude.sh container PATH parity with _bare_agent.sh (/opt/conda/bin)
# ---------------------------------------------------------------------------

_REPO = Path(__file__).resolve().parents[3]


def _read_repo(p):
    return (_REPO / p).read_text()


def test_bare_agent_path_has_conda():
    # The reference both arms must match.
    assert "/opt/conda/bin" in _read_repo("scripts/eval/_bare_agent.sh")


def test_agent_claude_container_path_has_conda():
    txt = _read_repo("scripts/agent-claude.sh")
    path_lines = [ln for ln in txt.splitlines()
                  if '-e "PATH=' in ln or "-e 'PATH=" in ln]
    assert path_lines, "no container PATH export line found in agent-claude.sh"
    assert any("/opt/conda/bin" in ln for ln in path_lines), (
        "agent-claude.sh container PATH must include /opt/conda/bin to match "
        "_bare_agent.sh (arm-fairness)")


# ---------------------------------------------------------------------------
# WS0.10: Journal.recovered_keys
# ---------------------------------------------------------------------------

def test_recovered_keys_returns_keys_with_recovered_flag(tmp_path):
    j = Journal(tmp_path / "run")
    j.append({"kind": "base", "key": "k1", "recovered": True})
    j.append({"kind": "base", "key": "k2"})
    j.append({"kind": "cell", "key": "k3", "recovered": True})
    assert j.recovered_keys() == {"k1", "k3"}


def test_recovered_keys_empty_when_no_flags(tmp_path):
    j = Journal(tmp_path / "run")
    j.append({"kind": "base", "key": "k1"})
    assert j.recovered_keys() == set()


# ---------------------------------------------------------------------------
# WS4.1: validate_lock_pins rejects unpinned dataset revisions
# ---------------------------------------------------------------------------

def _lock_entry(rev, kind="hf_dataset"):
    from scripts.eval.services.datasets import LockEntry
    return {"d": LockEntry("d", kind, rev)}


def test_validate_lock_pins_accepts_full_40_hex():
    from scripts.eval.services.datasets import validate_lock_pins
    rev = "810b6c54a81e98019bb6c36bdbdc1d4e93dd46d1"
    assert validate_lock_pins(_lock_entry(rev)) == [("d", rev)]


def test_validate_lock_pins_rejects_branch_name_main():
    from scripts.eval.services.datasets import validate_lock_pins
    with pytest.raises(ValueError, match="unpinned"):
        validate_lock_pins(_lock_entry("main"))


def test_validate_lock_pins_rejects_head_case_insensitive():
    from scripts.eval.services.datasets import validate_lock_pins
    with pytest.raises(ValueError, match="unpinned"):
        validate_lock_pins(_lock_entry("HEAD"))


def test_validate_lock_pins_rejects_short_sha_for_hf_dataset():
    from scripts.eval.services.datasets import validate_lock_pins
    with pytest.raises(ValueError, match="40-char"):
        validate_lock_pins(_lock_entry("810b6c5", kind="hf_dataset"))


def test_validate_lock_pins_rejects_non_hex_40_chars():
    from scripts.eval.services.datasets import validate_lock_pins
    with pytest.raises(ValueError, match="40-char"):
        validate_lock_pins(_lock_entry("z" * 40))


def test_validate_lock_pins_empty_lock_rejected():
    from scripts.eval.services.datasets import validate_lock_pins
    with pytest.raises(ValueError, match="no entries"):
        validate_lock_pins({})


# ---------------------------------------------------------------------------
# WS4.2: eval_runner.main() gates on pinned dataset revisions
# ---------------------------------------------------------------------------

def test_main_aborts_on_unpinned_lock(tmp_path, monkeypatch, capsys):
    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    bad_lock = tmp_path / "datasets.lock"
    bad_lock.write_text(
        '[[entries]]\n'
        'name = "phylobio/BiomniBench-DA"\n'
        'kind = "hf_dataset"\n'
        'revision = "main"\n'
    )
    monkeypatch.setattr(eval_runner, "_datasets_lock_path", lambda: bad_lock)
    rc = eval_runner.main(["biomnibench", "--smoke"])
    assert rc == 2
    assert "unpinned" in capsys.readouterr().err.lower()


def test_main_validates_real_lock_is_pinned(tmp_path, monkeypatch):
    # The committed datasets.lock must always pass validation.
    from scripts.eval.services.datasets import load_lock, validate_lock_pins
    real = eval_runner._datasets_lock_path()
    validate_lock_pins(load_lock(real))  # must not raise


# ---------------------------------------------------------------------------
# WS4.3 / WS4.4: campaign freeze record + CAMPAIGN-FREEZE.json
# ---------------------------------------------------------------------------

def test_freeze_record_shape(monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "a" * 40)
    rec = eval_runner._campaign_freeze_record(
        benchmark="biomnibench", arms=["ecaa", "claude-direct"], trials=3,
        datasets_lock="phylobio/BiomniBench-DA=810b6c54a8")
    assert rec["git_head"] == "a" * 40
    assert rec["benchmark"] == "biomnibench"
    assert rec["arms"] == ["ecaa", "claude-direct"]
    assert rec["trials"] == 3
    assert rec["seed"] == 1729  # mirrors scorecard._BOOTSTRAP_SEED
    assert rec["datasets_lock"] == "phylobio/BiomniBench-DA=810b6c54a8"
    assert rec["frozen_at"].endswith("Z")


def test_freeze_record_unknown_head_is_carried(monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "unknown")
    rec = eval_runner._campaign_freeze_record(
        benchmark="nekrutenko", arms=["ecaa"], trials=1, datasets_lock="x=y")
    assert rec["git_head"] == "unknown"


def test_write_campaign_freeze_creates_file(tmp_path, monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "b" * 40)
    rec = eval_runner._campaign_freeze_record(
        benchmark="biomnibench", arms=["ecaa"], trials=1, datasets_lock="x=y")
    out = eval_runner._write_campaign_freeze(tmp_path, rec, resuming=False)
    assert out == tmp_path / "CAMPAIGN-FREEZE.json"
    loaded = json.loads(out.read_text())
    assert loaded["git_head"] == "b" * 40


def test_write_campaign_freeze_idempotent_on_resume(tmp_path):
    freeze = tmp_path / "CAMPAIGN-FREEZE.json"
    freeze.write_text(json.dumps({"git_head": "original", "benchmark": "x"}))
    new_rec = {"git_head": "DIFFERENT", "benchmark": "x"}
    out = eval_runner._write_campaign_freeze(tmp_path, new_rec, resuming=True)
    # Resume must NOT clobber the original freeze.
    assert json.loads(out.read_text())["git_head"] == "original"


def test_write_campaign_freeze_resume_writes_if_absent(tmp_path):
    rec = {"git_head": "late", "benchmark": "x"}
    out = eval_runner._write_campaign_freeze(tmp_path, rec, resuming=True)
    assert json.loads(out.read_text())["git_head"] == "late"


# ---------------------------------------------------------------------------
# WS4.5: run-manifest.json + HEAD-unchanged check
# ---------------------------------------------------------------------------

def test_run_manifest_shape(tmp_path):
    out = eval_runner._write_run_manifest(
        tmp_path,
        benchmark="biomnibench",
        argv=["biomnibench", "--smoke", "--arms", "ecaa"],
        arms=["ecaa"], trials=1, max_iterations=60,
        intake_mode="chat", error_matrix=False, resuming=False,
        freeze_head="c" * 40)
    assert out == tmp_path / "run-manifest.json"
    m = json.loads(out.read_text())
    assert m["benchmark"] == "biomnibench"
    assert m["argv"] == ["biomnibench", "--smoke", "--arms", "ecaa"]
    assert m["intake_mode"] == "chat"
    assert m["freeze_head"] == "c" * 40
    assert m["max_iterations"] == 60
    assert m["resuming"] is False


def test_head_unchanged_passes_when_equal(monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "d" * 40)
    eval_runner._assert_head_unchanged("d" * 40)  # no raise


def test_head_unchanged_raises_on_drift(monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "e" * 40)
    with pytest.raises(RuntimeError, match="HEAD moved"):
        eval_runner._assert_head_unchanged("f" * 40)


def test_head_unchanged_skips_unknown_freeze(monkeypatch):
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "a" * 40)
    eval_runner._assert_head_unchanged("unknown")  # no raise


# ---------------------------------------------------------------------------
# WS4.6: main writes the freeze + manifest into the run dir
# ---------------------------------------------------------------------------

def test_main_writes_freeze_and_manifest(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_LIVE", "1")
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path / "cache"))
    monkeypatch.setenv("ECAA_EVAL_RUNS_DIR", str(tmp_path / "runs"))
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr(eval_runner, "_validate_datasets_lock", lambda: None)
    monkeypatch.setattr(eval_runner, "_git_head", lambda: "1" * 40)

    class _Plugin:
        def fetch(self, c): return Path("/fake")
        def tasks(self, h, *, smoke): return [Task("t1", "p", {}, {}, None)]
        def build_run(self, task, arm, wd):
            wd.mkdir(parents=True, exist_ok=True)
            return RunSpec(arm, wd, "bare", task.prompt)
        def collect(self, spec, rd):
            return Output("tr", "ans", {}, True, 0.0)
        def score(self, task, arm, out, trial):
            return Score(task.task_id, arm.value, trial, 50.0, {}, None, None,
                         "deterministic", extra={})
        def judge_requests(self, t, a, o): return []
        def report(self, scores):
            return Scorecard(benchmark="biomnibench", rows=scores, meta={})

    monkeypatch.setitem(eval_runner.PLUGINS, "biomnibench", _Plugin)
    monkeypatch.setattr(eval_runner.agent_runner, "run_bare",
                        lambda wd, instr, **kw: type("R", (), {
                            "exit_ok": True, "wall_secs": 0.1, "stdout": ""})())
    monkeypatch.setattr(eval_runner, "write_scorecard", lambda *a, **k: None)
    import scripts.eval.services.scorecard as sc
    monkeypatch.setattr(sc, "write_public_scorecard", lambda *a, **k: None)

    rc = eval_runner.main(["biomnibench", "--smoke", "--arms", "claude-direct",
                           "--trials", "1"])
    assert rc == 0
    runs = list((tmp_path / "runs").glob("biomnibench-*"))
    assert len(runs) == 1
    freeze = json.loads((runs[0] / "CAMPAIGN-FREEZE.json").read_text())
    assert freeze["git_head"] == "1" * 40
    manifest = json.loads((runs[0] / "run-manifest.json").read_text())
    assert manifest["freeze_head"] == "1" * 40
    assert manifest["intake_mode"] == "chat"


# ---------------------------------------------------------------------------
# WS4.7: prereg freeze-stanza renderer
# ---------------------------------------------------------------------------

def test_render_prereg_freeze_stanza(tmp_path):
    rec = {
        "benchmark": "biomnibench",
        "git_head": "9" * 40,
        "datasets_lock": "phylobio/BiomniBench-DA=810b6c54a8",
        "arms": ["ecaa", "claude-direct"],
        "trials": 3,
        "seed": 1729,
        "frozen_at": "2026-06-08T00:00:00Z",
    }
    freeze = tmp_path / "CAMPAIGN-FREEZE.json"
    freeze.write_text(json.dumps(rec))
    md = eval_runner.render_prereg_freeze_stanza(freeze)
    assert "**Frozen-at commit SHA:** `" + "9" * 40 + "`" in md
    assert "**Benchmark:** biomnibench" in md
    assert "**Arms:** ecaa, claude-direct" in md
    assert "**Datasets lock:** phylobio/BiomniBench-DA=810b6c54a8" in md
    assert "**Seed:** 1729" in md
