"""Render-as-Contract: package-portable render CLI entrypoint.

The decouple ("rendering is a fixed, non-LLM step over a data contract") needs
a render entrypoint that runs INSIDE an emitted package as
`python3 -m runtime.plotting render --stage <id> --outputs <dir> --required ...`,
over the standardized compute output tables — with NO agent/LLM authoring a
figure script. This exercises `lib/plotting/__main__.py` via `-m lib.plotting`
(the relative-import form that is parent-package-agnostic, so the identical
module works as `runtime.plotting` in a shipped package).
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[3]

# Minimal differential_expression figure-data contract table: the columns the
# Python DE renderers resolve by alias (gene/log2FoldChange/pvalue/padj/baseMean).
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


def _run_cli(outputs: Path, required: str) -> subprocess.CompletedProcess:
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
            required,
        ],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
    )


def test_render_cli_produces_required_figures_without_an_agent(tmp_path):
    outputs = tmp_path / "differential_expression"
    (outputs / "treated_vs_untreated").mkdir(parents=True)
    (outputs / "treated_vs_untreated" / "de_table.tsv").write_text(_DE_TABLE)

    proc = _run_cli(outputs, "volcano,ma_plot")

    assert proc.returncode == 0, f"render CLI failed:\nstdout={proc.stdout}\nstderr={proc.stderr}"
    # The result manifest is emitted as JSON on stdout for the wrapper to capture.
    report = json.loads(proc.stdout)
    assert set(report["written"]) >= {"volcano", "ma_plot"}, report
    for fig in ("volcano", "ma_plot"):
        png = outputs / "figures" / f"{fig}.png"
        assert png.exists() and png.stat().st_size > 0, f"missing rendered figure {png}"


def test_render_cli_reports_unknown_stage_without_crashing(tmp_path):
    outputs = tmp_path / "nope"
    outputs.mkdir()
    proc = subprocess.run(
        [
            sys.executable,
            "-m",
            "lib.plotting",
            "render",
            "--stage",
            "not_a_real_stage",
            "--outputs",
            str(outputs),
        ],
        cwd=str(REPO_ROOT),
        capture_output=True,
        text=True,
    )
    # Unknown stage is a soft no-op (not a crash): non-zero exit, JSON report,
    # no traceback. Mirrors generate()'s "unknown stage returns empty manifest".
    assert proc.returncode != 0
    report = json.loads(proc.stdout)
    assert report["written"] == {}
    assert "Traceback" not in proc.stderr
