from pathlib import Path
from scripts.eval.plugins.nekrutenko import Nekrutenko
from scripts.eval.benchmark import Task, Arm


def _task():
    return Task(task_id="mtdna", prompt="p", inputs={}, rubric=None,
                answer_key=None, meta={})  # no handle -> shims_root None


def test_error_matrix_specs_is_36():
    specs = Nekrutenko().error_matrix_specs()
    assert len(specs) == 36
    # 12 (pattern,tool) combos x 3 seeds; all seeds present per combo
    combos = {(p, t) for p, t, _ in specs}
    assert len(combos) == 12
    assert {s for _, _, s in specs} == {42, 43, 44}


def test_run_error_cell_classifies_via_run_fn(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    def run_fn(cell_dir, env):
        # shim env contract is set by the plugin
        assert env["EVAL_INJECT_PATTERN"] == "flake_first_call"
        assert env["EVAL_INJECT_TARGET"] == "bwa"
        assert "EVAL_INJECT_STATE" in env
        # produce the 4 expected vcfs WITH a variant record -> clean recover
        for s in ("a", "b", "c", "d"):
            (Path(cell_dir) / f"{s}.vcf").write_text(
                "##fileformat=VCFv4.2\nchrM\t152\t.\tT\tC\t.\tPASS\tAF=0.9\n")
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("flake_first_call", "bwa", 42), run_fn)
    assert cell["pattern"] == "flake_first_call"
    assert cell["tool"] == "bwa"
    assert cell["seed"] == 42
    assert cell["recover"] is True


def test_header_only_and_empty_vcfs_not_counted_as_recovered(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    def run_fn(cell_dir, env):
        # header-only (wrong_format_output) + empty (silent_truncation) -> 0 valid
        (Path(cell_dir) / "a.vcf").write_text("##fileformat=VCFv4.2\n#CHROM\tPOS\n")
        (Path(cell_dir) / "b.vcf").write_text("")
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("wrong_format_output", "lofreq", 42), run_fn)
    assert cell["recover"] is False  # no valid VCF content -> not a recovery


def test_report_aggregates_cells_across_trials_and_excludes_inconclusive():
    from scripts.eval.benchmark import Score

    def cell(rec, diag, inconclusive=False):
        c = {"pattern": "flake_first_call", "tool": "bwa", "seed": 42,
             "handle": "recover", "recover": rec, "diagnose": diag}
        if inconclusive:
            c["inconclusive"] = True
        return c

    r0 = Score("mtdna", "ecaa", 0, 100.0, {}, 1.0,
               [cell(True, True), cell(False, True)], "deterministic")
    r1 = Score("mtdna", "ecaa", 1, 100.0, {}, 1.0,
               [cell(True, False), cell(True, True, inconclusive=True)], "deterministic")
    em = Nekrutenko().report([r0, r1]).meta["error_matrix"]["ecaa"]
    assert em["n_cells"] == 3            # 4 cells over 2 trials, 1 inconclusive excluded
    assert em["n_inconclusive"] == 1
    assert abs(em["recover_rate"] - 2 / 3) < 1e-9   # True,False,True over scored cells


def test_error_matrix_still_returns_36_cells(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    cells = Nekrutenko().error_matrix(_task(), Arm.ECAA_WORKFLOW, tmp_path,
                                      lambda cd, env: FakeResult())
    assert len(cells) == 36


def test_run_error_cell_sets_shim_env_and_shim_dir(tmp_path, monkeypatch):
    """The cell env carries the shim contract: pattern/target/state + the
    abs ECAA_EVAL_SHIM_DIR pointing at scripts/eval/_eval_shim."""
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    captured = {}

    def run_fn(cell_dir, env):
        captured.update(env)
        # honest agent: mark the tool invoked + produce 4 valid VCFs
        Path(env["EVAL_INJECT_STATE"]).mkdir(parents=True, exist_ok=True)
        (Path(env["EVAL_INJECT_STATE"]) / "invoked.lofreq").write_text("1")
        for s in ("a", "b", "c", "d"):
            (Path(cell_dir) / f"{s}.vcf").write_text(
                "##fileformat=VCFv4.2\nchrM\t152\t.\tT\tC\t.\tPASS\tAF=0.9\n")
        return FakeResult()

    Nekrutenko().run_error_cell(_task(), ("missing_lib_error", "lofreq", 42), run_fn)
    assert captured["EVAL_INJECT_PATTERN"] == "missing_lib_error"
    assert captured["EVAL_INJECT_TARGET"] == "lofreq"
    assert "EVAL_INJECT_STATE" in captured
    shim_dir = Path(captured["ECAA_EVAL_SHIM_DIR"])
    assert shim_dir.is_absolute()
    assert shim_dir.name == "_eval_shim"
    assert (shim_dir / "shim.py").exists()


def test_run_error_cell_bypass_marks_inconclusive(tmp_path, monkeypatch):
    """A run_fn that NEVER writes the invoked.<tool> marker (the agent bypassed
    the shim) => cell['inconclusive'] is True and shim_invoked is False."""
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    def run_fn(cell_dir, env):
        # produces VCFs but does NOT touch the state dir -> bypass
        for s in ("a", "b", "c", "d"):
            (Path(cell_dir) / f"{s}.vcf").write_text(
                "##fileformat=VCFv4.2\nchrM\t152\t.\tT\tC\t.\tPASS\tAF=0.9\n")
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("flake_first_call", "bwa", 42), run_fn)
    assert cell["inconclusive"] is True
    assert cell["shim_invoked"] is False


def test_run_error_cell_invoked_not_inconclusive_and_recovers(tmp_path, monkeypatch):
    """A run_fn that writes state_dir/invoked.<tool> + 4 valid VCFs =>
    NOT inconclusive, shim_invoked True, recover True."""
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    def run_fn(cell_dir, env):
        state = Path(env["EVAL_INJECT_STATE"])
        state.mkdir(parents=True, exist_ok=True)
        (state / "invoked.bwa").write_text("1")     # shim was exercised
        for s in ("a", "b", "c", "d"):
            (Path(cell_dir) / f"{s}.vcf").write_text(
                "##fileformat=VCFv4.2\nchrM\t152\t.\tT\tC\t.\tPASS\tAF=0.9\n")
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("flake_first_call", "bwa", 42), run_fn)
    assert cell.get("inconclusive") is not True
    assert cell["shim_invoked"] is True
    assert cell["recover"] is True
