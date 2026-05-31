from pathlib import Path
from scripts.eval.benchmark import Arm, Score, Task
from scripts.eval.plugins.nekrutenko import Nekrutenko, _FAULT_PATTERNS, _SEEDS
from scripts.eval.scoring.error_matrix import classify_cell

def test_report_aggregates_jaccard_by_arm():
    rows = [
        Score("mtdna","ecaa",0, 100.0, {}, jaccard=1.0, error_cells=[], judge_id="deterministic"),
        Score("mtdna","claude-direct",0, 50.0, {}, jaccard=0.5, error_cells=[], judge_id="deterministic"),
    ]
    card = Nekrutenko().report(rows)
    assert card.benchmark == "nekrutenko"
    assert len(card.rows) == 2


def test_nekrutenko_error_matrix_uses_classify_cell(tmp_path):
    """error_matrix returns cells whose handle/recover/diagnose match classify_cell
    for the simulated outcomes produced by the fake run_fn."""

    # Build a minimal task with no real handle dir (shim dir will be synthesised
    # inside error_matrix from the cell tempdir, which is fine for an offline test).
    task = Task(task_id="mtdna", prompt="call variants", inputs={},
                rubric=None, answer_key=None, meta={})

    # Fake run_fn: writes N VCF files into cell_workdir and returns exit_ok.
    # We choose exit_ok=True and 4 produced VCFs -> every cell should classify
    # as "recover" (produced_valid >= expected_valid=4, no failures_log).
    def fake_run_fn(cell_workdir, env):
        for i in range(4):
            (cell_workdir / f"sample{i}.vcf").write_text(
                "##fileformat=VCFv4.2\n"
                "#CHROM\tPOS\tID\tREF\tALT\tQUAL\tFILTER\tINFO\n"
                f"chrM\t{150 + i}\t.\tT\tC\t.\tPASS\tAF=0.99\n"
            )

        class _R:
            exit_ok = True
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

    # With fake_run_fn producing 4 VCFs + exit_ok + no failures.log,
    # classify_cell should yield handle="recover" for each cell.
    expected = classify_cell(exit_code=0, failures_log="",
                             produced_valid=4, expected_valid=4)
    for cell in cells:
        assert cell["handle"] == expected["handle"], (
            f"cell {cell['pattern']}/{cell['tool']}@seed{cell['seed']}: "
            f"expected handle={expected['handle']!r}, got {cell['handle']!r}"
        )
        assert cell["recover"] == expected["recover"]
        assert cell["diagnose"] == expected["diagnose"]

    # Verify the full (pattern,tool)×seed coverage matches _FAULT_PATTERNS × _SEEDS.
    seen_combos_seeds = {(c["pattern"], c["tool"], c["seed"]) for c in cells}
    expected_combos_seeds = {(p, t, s) for p, t in _FAULT_PATTERNS for s in _SEEDS}
    assert seen_combos_seeds == expected_combos_seeds, (
        "Not all (pattern,tool,seed) triples were covered"
    )
