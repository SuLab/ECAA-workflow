# scripts/eval/tests/test_tasks_filter.py
"""Unit coverage for the --tasks allowlist primitive and the pinned baseline
subset manifest that `make eval-baseline` reads. Offline; no live API."""
import tomllib
from pathlib import Path

import pytest

from scripts.eval.eval_runner import _apply_task_filter
from scripts.eval.benchmark import Task


def _task(tid: str) -> Task:
    return Task(task_id=tid, prompt="", inputs={}, rubric=None, answer_key=None)


def _ids(tasks) -> list[str]:
    return [t.task_id for t in tasks]


def test_none_keeps_all():
    tasks = [_task("da-1-1"), _task("da-2-2")]
    assert _apply_task_filter(tasks, None) is tasks


def test_empty_string_keeps_all():
    tasks = [_task("da-1-1"), _task("da-2-2")]
    assert _ids(_apply_task_filter(tasks, "")) == ["da-1-1", "da-2-2"]


def test_selects_subset():
    tasks = [_task("da-1-1"), _task("da-2-2"), _task("da-3-3")]
    assert _ids(_apply_task_filter(tasks, "da-1-1,da-3-3")) == ["da-1-1", "da-3-3"]


def test_allowlist_order_is_respected():
    """Returned order follows the allowlist, not the input order."""
    tasks = [_task("da-1-1"), _task("da-2-2"), _task("da-3-3")]
    assert _ids(_apply_task_filter(tasks, "da-3-3,da-1-1")) == ["da-3-3", "da-1-1"]


def test_whitespace_tolerated():
    tasks = [_task("da-1-1"), _task("da-2-2")]
    assert _ids(_apply_task_filter(tasks, " da-2-2 , da-1-1 ")) == ["da-2-2", "da-1-1"]


def test_unknown_id_raises_with_context():
    tasks = [_task("da-1-1")]
    with pytest.raises(ValueError) as ei:
        _apply_task_filter(tasks, "da-1-1,da-9-9")
    msg = str(ei.value)
    assert "da-9-9" in msg          # names the offender
    assert "da-1-1" in msg          # lists what's available


def test_single_task_plugin_either_survives_or_empties():
    """Nekrutenko has one 'mtdna' task: it survives an allowlist that names it,
    and a non-matching allowlist is a hard error (never a silent empty run)."""
    tasks = [_task("mtdna")]
    assert _ids(_apply_task_filter(tasks, "mtdna")) == ["mtdna"]
    with pytest.raises(ValueError):
        _apply_task_filter(tasks, "da-1-1")


# ── Baseline manifest shape (the contract `make eval-baseline` depends on) ────

_MANIFEST = Path(__file__).resolve().parents[1] / "subsets" / "baseline.toml"


def test_baseline_manifest_parses_and_has_expected_shape():
    data = tomllib.loads(_MANIFEST.read_text())
    ids = data["biomnibench"]["task_ids"]
    assert isinstance(ids, list) and ids, "biomnibench.task_ids must be a non-empty list"
    assert all(isinstance(i, str) and i.startswith("da-") for i in ids)
    assert len(ids) == len(set(ids)), "task_ids must be unique"
    nek = data["nekrutenko"]
    assert isinstance(nek["seeds"], list) and nek["seeds"], "nekrutenko.seeds non-empty"
    assert nek["error_matrix"] is True
