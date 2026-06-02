from pathlib import Path
from scripts.eval.plugins.nekrutenko import (Nekrutenko, _SAMPLE_NAMES, _target_n,
                                             _present_sample_count)
from scripts.eval.benchmark import Task, Arm

_REC = ("##fileformat=VCFv4.2\n"
        "#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"
        "chrM\t152\t.\tT\tC\t.\tPASS\tAF=0.9\n")
_HEADER_ONLY = "##fileformat=VCFv4.2\n#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"


def _task():
    return Task(task_id="mtdna", prompt="p", inputs={}, rubric=None,
                answer_key=None, meta={})  # no handle -> shims_root None


def _write(cell_dir, names, content):
    for n in names:
        (Path(cell_dir) / f"{n}.vcf").write_text(content)


# ── target_n map (faithful to score_run.py error_handling) ───────────────────

def test_target_n_pattern_map():
    assert _target_n("one_sample_fails") == len(_SAMPLE_NAMES) - 1   # 3
    assert _target_n("missing_lib_error") == 0
    assert _target_n("silent_truncation") == 0
    assert _target_n("wrong_format_output") == 0
    for p in ("flake_first_call", "slow_tool", "stderr_warning_storm", "none"):
        assert _target_n(p) == len(_SAMPLE_NAMES)                    # 4


# ── _present_sample_count: header-only counts, 0-byte does not ───────────────

def test_present_count_records(tmp_path):
    _write(tmp_path, _SAMPLE_NAMES, _REC)
    assert _present_sample_count(tmp_path) == 4


def test_present_count_header_only_counts(tmp_path):
    # The reference _samples_with_valid_vcf accepts a header-only VCF as
    # structurally present (biology missing is detected via M3, not M_recover).
    _write(tmp_path, _SAMPLE_NAMES, _HEADER_ONLY)
    assert _present_sample_count(tmp_path) == 4


def test_present_count_empty_excluded(tmp_path):
    _write(tmp_path, _SAMPLE_NAMES, "")  # 0-byte (silent_truncation)
    assert _present_sample_count(tmp_path) == 0


def test_present_count_ignores_unrecognised_names(tmp_path):
    _write(tmp_path, ("a", "b", "c", "d"), _REC)  # not canonical sample stems
    assert _present_sample_count(tmp_path) == 0


# ── run_error_cell: faithful recover for the 4 inverted patterns ─────────────

def _run_with_outputs(pattern, tool, names, content, tmp_path, monkeypatch,
                      mark_invoked=True):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True
        stdout = ""

    def run_fn(cell_dir, env):
        if mark_invoked:
            state = Path(env["EVAL_INJECT_STATE"])
            state.mkdir(parents=True, exist_ok=True)
            (state / f"invoked.{tool}").write_text("1")
        _write(cell_dir, names, content)
        return FakeResult()

    return Nekrutenko().run_error_cell(_task(), (pattern, tool, 42), run_fn)


def test_recover_one_sample_fails_three_valid_is_recovery(tmp_path, monkeypatch):
    # one_sample_fails: producing 3 valid (skipping the injected sample) IS the
    # best achievable -> recover True (the bug fixed: was False under >=4).
    cell = _run_with_outputs("one_sample_fails", "lofreq",
                             _SAMPLE_NAMES[:3], _REC, tmp_path, monkeypatch)
    assert cell["recover"] is True


def test_recover_missing_lib_zero_valid_is_recovery(tmp_path, monkeypatch):
    # missing_lib_error: nothing should work; 0 valid is best achievable.
    cell = _run_with_outputs("missing_lib_error", "lofreq",
                             (), _REC, tmp_path, monkeypatch)
    assert cell["recover"] is True


def test_naive_header_only_wrong_format_is_not_recovery(tmp_path, monkeypatch):
    # wrong_format_output target_n=0: 4 header-only files (counted present) means
    # the run failed to detect the corruption -> NOT a recovery.
    cell = _run_with_outputs("wrong_format_output", "lofreq",
                             _SAMPLE_NAMES, _HEADER_ONLY, tmp_path, monkeypatch)
    assert cell["recover"] is False


def test_detect_and_skip_wrong_format_is_recovery(tmp_path, monkeypatch):
    # A run that detects every corrupt output and emits none -> 0 present ->
    # recover True for the target_n=0 pattern.
    cell = _run_with_outputs("wrong_format_output", "lofreq",
                             (), _REC, tmp_path, monkeypatch)
    assert cell["recover"] is True


def test_flake_full_recovery(tmp_path, monkeypatch):
    cell = _run_with_outputs("flake_first_call", "bwa",
                             _SAMPLE_NAMES, _REC, tmp_path, monkeypatch)
    assert cell["recover"] is True
    assert cell["handle"] == "recover"


# ── exec-log diagnose (captured agent stdout/stderr) ─────────────────────────

def test_diagnose_from_exec_log_summary_line(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True
        stdout = "pipeline complete: 3/4 samples produced output"

    def run_fn(cell_dir, env):
        state = Path(env["EVAL_INJECT_STATE"])
        state.mkdir(parents=True, exist_ok=True)
        (state / "invoked.lofreq").write_text("1")
        _write(cell_dir, _SAMPLE_NAMES[:3], _REC)
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("one_sample_fails", "lofreq", 42), run_fn)
    assert cell["diagnose"] is True


# ── unchanged invariants ─────────────────────────────────────────────────────

def test_error_matrix_specs_is_36():
    specs = Nekrutenko().error_matrix_specs()
    assert len(specs) == 36
    combos = {(p, t) for p, t, _ in specs}
    assert len(combos) == 12
    assert {s for _, _, s in specs} == {42, 43, 44}


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


def test_report_aggregates_handle_histogram():
    from scripts.eval.benchmark import Score

    def cell(handle):
        return {"pattern": "flake_first_call", "tool": "bwa", "seed": 42,
                "handle": handle, "recover": handle == "recover", "diagnose": True}

    rows = [Score("mtdna", "ecaa", 0, 100.0, {}, 1.0,
                  [cell("recover"), cell("partial"), cell("crash"), cell("recover")],
                  "deterministic")]
    em = Nekrutenko().report(rows).meta["error_matrix"]["ecaa"]
    assert em["handle_counts"]["recover"] == 2
    assert em["handle_counts"]["partial"] == 1
    assert em["handle_counts"]["crash"] == 1
    assert em["handle_counts"].get("propagate", 0) == 0


def test_error_matrix_still_returns_36_cells(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True
        stdout = ""

    cells = Nekrutenko().error_matrix(_task(), Arm.ECAA_WORKFLOW, tmp_path,
                                      lambda cd, env: FakeResult())
    assert len(cells) == 36


def test_run_error_cell_sets_shim_env_and_shim_dir(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True
        stdout = ""

    captured = {}

    def run_fn(cell_dir, env):
        captured.update(env)
        Path(env["EVAL_INJECT_STATE"]).mkdir(parents=True, exist_ok=True)
        (Path(env["EVAL_INJECT_STATE"]) / "invoked.lofreq").write_text("1")
        _write(cell_dir, _SAMPLE_NAMES, _REC)
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
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(tmp_path))

    class FakeResult:
        exit_ok = True
        stdout = ""

    def run_fn(cell_dir, env):
        _write(cell_dir, _SAMPLE_NAMES, _REC)  # produces VCFs but never touches state dir
        return FakeResult()

    cell = Nekrutenko().run_error_cell(_task(), ("flake_first_call", "bwa", 42), run_fn)
    assert cell["inconclusive"] is True
    assert cell["shim_invoked"] is False


def test_run_error_cell_invoked_not_inconclusive_and_recovers(tmp_path, monkeypatch):
    cell = _run_with_outputs("flake_first_call", "bwa", _SAMPLE_NAMES, _REC,
                             tmp_path, monkeypatch)
    assert cell.get("inconclusive") is not True
    assert cell["shim_invoked"] is True
    assert cell["recover"] is True
