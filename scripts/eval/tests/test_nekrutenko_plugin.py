from pathlib import Path
from scripts.eval.benchmark import Arm, Score, Task
from scripts.eval.plugins.nekrutenko import (Nekrutenko, _FAULT_PATTERNS, _SEEDS,
                                             _SAMPLE_NAMES, _target_n)

def test_report_aggregates_jaccard_by_arm():
    rows = [
        Score("mtdna","ecaa",0, 100.0, {}, jaccard=1.0, error_cells=[], judge_id="deterministic"),
        Score("mtdna","claude-direct",0, 50.0, {}, jaccard=0.5, error_cells=[], judge_id="deterministic"),
    ]
    card = Nekrutenko().report(rows)
    assert card.benchmark == "nekrutenko"
    assert len(card.rows) == 2


def test_headline_is_macro_m3_flat_pool_kept_secondary():
    """The Nekrutenko headline is the paper's per-sample macro M3; the flat-pool
    Jaccard is kept COMPUTED and VISIBLE but labeled secondary.

    Fixture: one ECAA row whose flat-pool Jaccard is 0.667 (Score.overall=66.7,
    Score.jaccard=0.667) while the per-sample macro M3 is 0.917 — the canonical
    case where pooling all four samples into one denominator amplifies a single
    low-AF heteroplasmy miss ~4x below the macro-mean. The headline must map to
    0.917 (M3), the flat-pool must stay reported at 0.667, and Score.overall (the
    serialization contract) must be untouched."""
    rows = [
        Score("mtdna", "ecaa", 0, 66.7, {}, jaccard=0.667, error_cells=[],
              judge_id="deterministic",
              extra={"per_sample_macro_jaccard": 0.917}),
    ]
    card = Nekrutenko().report(rows)

    # Score.overall (serialization contract) is unchanged — still flat-pool×100.
    assert card.rows[0].overall == 66.7
    assert card.rows[0].jaccard == 0.667

    # BOTH metrics present in meta, per arm.
    assert card.meta["per_sample_macro_jaccard"] == {"ecaa": 0.917}
    assert card.meta["flat_pool_jaccard"] == {"ecaa": 0.667}

    # The headline block leads with M3 (primary) and keeps flat-pool secondary.
    hl = card.meta["nekrutenko_headline"]
    assert hl["primary_metric"] == "per_sample_macro_jaccard"
    assert hl["per_sample_macro_jaccard"]["ecaa"] == 0.917      # headline value
    assert hl["secondary_metric"] == "flat_pool_jaccard"
    assert hl["flat_pool_jaccard"]["ecaa"] == 0.667             # secondary, kept
    assert "paper primary" in hl["primary_label"].lower()
    assert "secondary" in hl["secondary_label"].lower()
    # The note must explain the ~4x amplification AND that the choice is
    # arm-agnostic (so the metric switch can't be read as a results-favoring
    # shortcut).
    assert "4x" in hl["note"] and "arm-agnostic" in hl["note"].lower()

    # The rendered markdown headline section maps to the macro (0.917) and still
    # shows the flat-pool (0.667), clearly labeled secondary.
    import tempfile
    from scripts.eval.services.scorecard import write_scorecard
    with tempfile.TemporaryDirectory() as td:
        md = (write_scorecard(card, Path(td)) / "scorecard.md").read_text()
    assert "Jaccard headline" in md
    assert "per-sample macro M3 (paper primary)" in md
    assert "amplifies single-site misses" in md
    assert "0.9170" in md and "0.6670" in md


def test_nekrutenko_error_matrix_uses_classify_cell(tmp_path):
    """error_matrix returns cells whose handle/recover/diagnose match classify_cell
    for the simulated outcomes produced by the fake run_fn."""

    # Build a minimal task with no real handle dir (shim dir will be synthesised
    # inside error_matrix from the cell tempdir, which is fine for an offline test).
    task = Task(task_id="mtdna", prompt="call variants", inputs={},
                rubric=None, answer_key=None, meta={})

    # Fake run_fn: writes one VCF per CANONICAL sample into cell_workdir and
    # returns exit_ok. With all 4 samples present, every cell's m_handle is
    # "recover" (n_valid == n_samples); m_recover (binary) is pattern-specific
    # via target_n and is asserted per-pattern below.
    def fake_run_fn(cell_workdir, env):
        state = Path(env["EVAL_INJECT_STATE"])
        state.mkdir(parents=True, exist_ok=True)
        (state / f"invoked.{env['EVAL_INJECT_TARGET']}").write_text("1")
        for s in _SAMPLE_NAMES:
            (cell_workdir / f"{s}.vcf").write_text(
                "##fileformat=VCFv4.2\n"
                "#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"
                "chrM\t152\t.\tT\tC\t.\tPASS\tAF=0.99\n"
            )

        class _R:
            exit_ok = True
            stdout = ""
        return _R()

    plug = Nekrutenko()
    cells = plug.error_matrix(task, Arm.ECAA_WORKFLOW, tmp_path, fake_run_fn)

    # Must return EXACTLY 36 cells: 12 (pattern,tool) combinations × 3 seeds.
    assert cells is not None, "error_matrix should return a list, not None"
    assert len(cells) == 36, (
        f"expected 36 cells (12 pattern×tool combinations × 3 seeds), got {len(cells)}"
    )
    assert len(_FAULT_PATTERNS) == 12, (
        f"_FAULT_PATTERNS must have 12 entries, got {len(_FAULT_PATTERNS)}"
    )
    assert len(_SEEDS) == 3, f"_SEEDS must have 3 seeds, got {len(_SEEDS)}"

    # Every cell must carry pattern, tool, seed, and the classify_cell keys.
    for cell in cells:
        assert "pattern" in cell, f"cell missing 'pattern': {cell}"
        assert "tool" in cell, f"cell missing 'tool': {cell}"
        assert "seed" in cell, f"cell missing 'seed': {cell}"
        assert "handle" in cell, f"cell missing 'handle': {cell}"
        assert "recover" in cell, f"cell missing 'recover': {cell}"
        assert "diagnose" in cell, f"cell missing 'diagnose': {cell}"

    # All 12 distinct (pattern,tool) combinations must each appear for all 3 seeds.
    from collections import Counter
    combo_counts: Counter = Counter((c["pattern"], c["tool"]) for c in cells)
    assert len(combo_counts) == 12, (
        f"expected 12 distinct (pattern,tool) combos, got {len(combo_counts)}: "
        f"{list(combo_counts.keys())}"
    )
    for combo, count in combo_counts.items():
        assert count == 3, (
            f"combo {combo} appears {count} times, expected 3 (once per seed)"
        )

    # All 3 seeds must appear for each combination.
    seed_set = {c["seed"] for c in cells}
    assert seed_set == set(_SEEDS), (
        f"expected seeds {set(_SEEDS)}, got {seed_set}"
    )

    # All 4 samples present + exit_ok + no failures.log: every cell's m_handle is
    # "recover" (n_valid == n_samples), but m_recover (binary) is pattern-specific
    # — True only when n_valid == target_n (so the target_n=0/3 patterns are
    # NOT binary-recovered by a run that emitted all 4).
    for cell in cells:
        assert cell["handle"] == "recover", (
            f"cell {cell['pattern']}/{cell['tool']}@seed{cell['seed']}: "
            f"expected handle='recover', got {cell['handle']!r}"
        )
        assert cell["recover"] is (4 == _target_n(cell["pattern"])), (
            f"cell {cell['pattern']}: recover should be {4 == _target_n(cell['pattern'])}"
        )
        assert cell["diagnose"] is False

    # Verify the full (pattern,tool)×seed coverage matches _FAULT_PATTERNS × _SEEDS.
    seen_combos_seeds = {(c["pattern"], c["tool"], c["seed"]) for c in cells}
    expected_combos_seeds = {(p, t, s) for p, t in _FAULT_PATTERNS for s in _SEEDS}
    assert seen_combos_seeds == expected_combos_seeds, (
        "Not all (pattern,tool,seed) triples were covered"
    )
