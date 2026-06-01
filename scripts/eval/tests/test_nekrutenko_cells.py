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
        # produce the 4 expected vcfs so it classifies as a clean recover
        for s in ("a", "b", "c", "d"):
            (Path(cell_dir) / f"{s}.vcf").write_text("##fileformat=VCFv4.2\n")
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("flake_first_call", "bwa", 42), run_fn)
    assert cell["pattern"] == "flake_first_call"
    assert cell["tool"] == "bwa"
    assert cell["seed"] == 42
    assert "recover" in cell and "diagnose" in cell


def test_error_matrix_still_returns_36_cells(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True

    cells = Nekrutenko().error_matrix(_task(), Arm.ECAA_WORKFLOW, tmp_path,
                                      lambda cd, env: FakeResult())
    assert len(cells) == 36
