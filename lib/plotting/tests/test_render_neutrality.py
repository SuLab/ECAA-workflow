"""Render-as-Contract: figure provenance is independent of compute language.

The decouple turns rendering into a FIXED, non-LLM step over a standardized
figure-data contract. The compute step (an LLM agent in Python OR R) writes the
contract TABLES; it never authors a figure script. So two compute languages that
emit the IDENTICAL contract table must yield IDENTICAL figures through the same
`python3 -m runtime.plotting render ...` CLI (exercised here as `-m lib.plotting`,
the parent-package-agnostic form).

These tests pin that neutrality: same table -> same figures regardless of which
"language" produced it, and the render itself is deterministic across reruns.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

from lib.plotting.tests.helpers import structural_snapshot

REPO_ROOT = Path(__file__).resolve().parents[3]

# Minimal differential_expression figure-data contract table, identical to the
# one in test_render_cli.py: the columns the DE renderers resolve by alias
# (gene/log2FoldChange/pvalue/padj/baseMean). The same bytes stand in for "what
# a Python compute step wrote" and "what an R compute step wrote".
_DE_TABLE = "\n".join(
    [
        "gene\tbaseMean\tlog2FoldChange\tpvalue\tpadj",
        "FBgn0039155\t730.6\t-4.62\t1e-160\t1e-159",
        "FBgn0025111\t1501.4\t2.90\t1e-120\t1e-119",
        "FBgn0003360\t2189.0\t-3.05\t1e-110\t1e-109",
        "FBgn0024288\t50.2\t0.31\t0.85\t0.95",
        "FBgn0000043\t1820.0\t1.74\t1e-40\t1e-39",
        "FBgn0034736\t12.0\t0.05\t0.99\t0.99",
    ]
)

_REQUIRED = "volcano,ma_plot"


def _seed_contract(outputs: Path) -> None:
    """Write the standardized DE contract table under a fresh outputs dir."""
    contrast = outputs / "treated_vs_untreated"
    contrast.mkdir(parents=True)
    (contrast / "de_table.tsv").write_text(_DE_TABLE)


def _run_render(outputs: Path) -> subprocess.CompletedProcess:
    return subprocess.run(
        [
            sys.executable,
            "-m",
            "lib.plotting",
            "render",
            "--stage",
            "differential_expression",
            "--outputs",
            str(outputs),
            "--required",
            _REQUIRED,
        ],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
    )


def _render_and_report(outputs: Path) -> dict:
    _seed_contract(outputs)
    proc = _run_render(outputs)
    assert proc.returncode == 0, f"render failed:\nstdout={proc.stdout}\nstderr={proc.stderr}"
    return json.loads(proc.stdout)


def test_figures_are_independent_of_compute_language(tmp_path):
    """Identical contract table from two 'languages' => identical figures.

    `python_task` and `r_task` represent two compute-language agents that wrote
    the EXACT same contract table. The render CLI is the only thing that touches
    pixels, so the written figure set and each figure's structural fingerprint
    must match across the two dirs.
    """
    py_out = tmp_path / "python_task"
    r_out = tmp_path / "r_task"

    py_report = _render_and_report(py_out)
    r_report = _render_and_report(r_out)

    required = {"volcano", "ma_plot"}
    py_written = set(py_report["written"])
    r_written = set(r_report["written"])
    assert py_written >= required, py_report
    assert r_written >= required, r_report
    # Same renderer, same inputs => identical written set regardless of language.
    assert py_written == r_written, (py_written, r_written)

    for fig in sorted(py_written):
        py_png = py_out / "figures" / f"{fig}.png"
        r_png = r_out / "figures" / f"{fig}.png"
        assert py_png.exists() and py_png.stat().st_size > 0, f"missing {py_png}"
        assert r_png.exists() and r_png.stat().st_size > 0, f"missing {r_png}"
        assert structural_snapshot(py_png) == structural_snapshot(r_png), (
            f"figure {fig} differs between compute languages: the renderer is "
            f"supposed to be language-neutral"
        )


def test_render_is_deterministic_across_reruns(tmp_path):
    """Re-rendering the same contract into a fresh dir yields the same figures."""
    first = tmp_path / "run_a"
    second = tmp_path / "run_b"

    first_report = _render_and_report(first)
    second_report = _render_and_report(second)

    assert set(first_report["written"]) == set(second_report["written"]), (
        first_report,
        second_report,
    )

    for fig in sorted(set(first_report["written"])):
        a_png = first / "figures" / f"{fig}.png"
        b_png = second / "figures" / f"{fig}.png"
        assert a_png.exists() and b_png.exists()
        assert structural_snapshot(a_png) == structural_snapshot(b_png), (
            f"figure {fig} is not reproducible across identical render runs"
        )
