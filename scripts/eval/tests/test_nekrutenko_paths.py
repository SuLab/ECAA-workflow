"""Fixture-based path tests for the Nekrutenko plugin.

Builds a minimal directory tree mirroring the CONFIRMED layout of the pinned
``nekrut/LLM-eval-paper`` repo (SHA 1175f72a…):

  data/raw/           — 4 paired-end .fq.gz samples (8 files total)
  ground_truth/results/ — 4 canonical .vcf.gz answer-key files
  harness/error_shims/  — flat dir with bwa + lofreq wrapper scripts + shim.py
  plan/PLAN.md          — default v2 plan file

No real bioinformatics tools are needed; we just assert discovery logic.
"""
from __future__ import annotations
from pathlib import Path
import os
import pytest

from scripts.eval.plugins.nekrutenko import (
    Nekrutenko,
    _PLAN,
    _SAMPLES,
    _ANSWER_KEY,
)

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

SAMPLE_NAMES = ("M117-bl", "M117-ch", "M117C1-bl", "M117C1-ch")


def _build_fixture(root: Path) -> Path:
    """Build a minimal directory tree matching the real repo layout."""
    # data/raw/ — 4 × 2 paired-end .fq.gz files
    raw = root / "data" / "raw"
    raw.mkdir(parents=True)
    for sample in SAMPLE_NAMES:
        (raw / f"{sample}_1.fq.gz").write_bytes(b"")
        (raw / f"{sample}_2.fq.gz").write_bytes(b"")

    # ground_truth/results/ — 4 .vcf.gz answer-key files
    gt = root / "ground_truth" / "results"
    gt.mkdir(parents=True)
    for sample in SAMPLE_NAMES:
        (gt / f"{sample}.vcf.gz").write_bytes(b"")

    # harness/error_shims/ — flat dir with bwa, lofreq wrapper scripts + shim.py
    shims = root / "harness" / "error_shims"
    shims.mkdir(parents=True)
    for name in ("bwa", "lofreq"):
        sh = shims / name
        sh.write_text("#!/bin/bash\nexec python3 \"$(dirname \"$(readlink -f \"$0\")\")/shim.py\" "
                      + name + " \"$@\"\n")
        sh.chmod(0o755)
    (shims / "shim.py").write_text("# stub\n")

    # plan/PLAN.md
    plan = root / "plan"
    plan.mkdir()
    (plan / "PLAN.md").write_text("# stub plan\n")

    return root


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------

def test_path_constants_match_real_layout():
    """_PLAN / _SAMPLES / _ANSWER_KEY must match the real repo layout."""
    assert _PLAN == "plan/PLAN.md", f"unexpected _PLAN: {_PLAN!r}"
    assert _SAMPLES == "data/raw", f"unexpected _SAMPLES: {_SAMPLES!r}"
    assert _ANSWER_KEY == "ground_truth/results", f"unexpected _ANSWER_KEY: {_ANSWER_KEY!r}"


def test_tasks_finds_all_fastq_pairs(tmp_path):
    """tasks() must discover all 8 .fq.gz sample files (4 samples × 2 reads)."""
    handle = _build_fixture(tmp_path)
    plugin = Nekrutenko()
    tasks = plugin.tasks(handle, smoke=False)

    assert len(tasks) == 1, "should return exactly one task"
    task = tasks[0]

    # 8 inputs: 4 samples × _1/_2
    assert len(task.inputs) == 8, (
        f"expected 8 .fq.gz inputs, got {len(task.inputs)}: {sorted(task.inputs)}"
    )

    # All files must actually exist in the fixture
    for name, path in task.inputs.items():
        assert path.exists(), f"input file missing: {path}"
        assert name.endswith(".fq.gz"), f"unexpected extension: {name!r}"


def test_tasks_answer_key_is_dir(tmp_path):
    """task.answer_key must resolve to an existing directory."""
    handle = _build_fixture(tmp_path)
    plugin = Nekrutenko()
    task = plugin.tasks(handle, smoke=False)[0]

    assert os.path.isdir(str(task.answer_key)), (
        f"answer_key should be a directory, got: {task.answer_key!r}"
    )


def test_tasks_answer_key_contains_vcf_gz(tmp_path):
    """The answer_key directory must contain exactly 4 .vcf.gz files."""
    handle = _build_fixture(tmp_path)
    plugin = Nekrutenko()
    task = plugin.tasks(handle, smoke=False)[0]

    vcfs = sorted(Path(task.answer_key).glob("*.vcf.gz"))
    assert len(vcfs) == 4, (
        f"expected 4 .vcf.gz files in answer_key, got {len(vcfs)}: {vcfs}"
    )
    for v in vcfs:
        assert v.name.endswith(".vcf.gz"), f"unexpected file: {v.name}"


def test_plan_file_exists_in_fixture(tmp_path):
    """The plan file referenced by _PLAN must exist in the fixture tree."""
    handle = _build_fixture(tmp_path)
    plan_path = handle / _PLAN
    assert plan_path.exists(), f"plan file not found at {plan_path}"


def test_tasks_meta_carries_handle(tmp_path):
    """task.meta['handle'] must be set to the handle path string."""
    handle = _build_fixture(tmp_path)
    plugin = Nekrutenko()
    task = plugin.tasks(handle, smoke=False)[0]

    assert "handle" in task.meta, "task.meta must contain 'handle' key"
    assert task.meta["handle"] == str(handle)


def test_tasks_smoke_returns_same_task_count(tmp_path):
    """smoke=True must return the same single task (no smoke-filter logic yet)."""
    handle = _build_fixture(tmp_path)
    plugin = Nekrutenko()
    tasks_full = plugin.tasks(handle, smoke=False)
    tasks_smoke = plugin.tasks(handle, smoke=True)
    assert len(tasks_full) == len(tasks_smoke) == 1
