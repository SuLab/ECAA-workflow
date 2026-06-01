"""Fidelity tests against REAL BiomniBench-DA rubric.txt files.

These load an on-disk dataset rubric (skipped if the dataset mount is absent so
CI stays green without it) and assert that `normalize_rubric` faithfully parses
the structured `Criterion K` / `Levels: A=… B=… C=…` format into multiple
weighted criteria, and that `parse_verdict` matches criterion ids
case-insensitively.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from scripts.eval.rubric_normalize import normalize_rubric
from scripts.eval.services.judge import parse_verdict

# Dataset mount (one snapshot dir; any da-*/tests/rubric.txt suffices).
_DATASET_ROOT = Path(
    "/home/a/mounts/wadmin/home/a/benchmark_data/hf/"
    "phylobio__BiomniBench-DA@810b6c54a81e"
)


def _find_real_rubric() -> Path | None:
    if not _DATASET_ROOT.is_dir():
        return None
    for task_dir in sorted(_DATASET_ROOT.glob("da-*")):
        rubric = task_dir / "tests" / "rubric.txt"
        if rubric.is_file() and "Criterion 1:" in rubric.read_text():
            return rubric
    return None


def _load_real_rubric_text() -> str:
    path = _find_real_rubric()
    if path is None:
        pytest.skip("BiomniBench-DA dataset mount absent; fidelity test skipped")
    return path.read_text()


def test_real_rubric_yields_multiple_weighted_criteria():
    norm = normalize_rubric(_load_real_rubric_text())
    crits = norm["criteria"]
    assert len(crits) >= 2, "structured rubric must yield >=2 criteria"
    # Summed A-weight points approximate 100 (source-reliability A=0 contributes 0).
    total_points = sum(c["points"] for c in crits)
    assert abs(total_points - 100.0) <= 1.0, f"summed A-weights={total_points}, expected ~100"
    for c in crits:
        assert set(c) >= {"id", "dimension", "points", "levels"}
        assert c["levels"]["A"] == 1.0
        assert c["levels"]["C"] == 0.0


def test_real_rubric_ids_are_stable_and_sequential():
    norm = normalize_rubric(_load_real_rubric_text())
    ids = [c["id"] for c in norm["criteria"]]
    expected = [f"criterion_{i}" for i in range(1, len(ids) + 1)]
    assert ids == expected


def test_real_rubric_parse_verdict_case_insensitive():
    norm = normalize_rubric(_load_real_rubric_text())
    n = len(norm["criteria"])
    # Judge emits ids in the rubric's own casing; assert any case round-trips.
    all_A = "\n".join(f"criterion_{i}: A" for i in range(1, n + 1))
    out_lower_id = parse_verdict(norm, all_A)
    # Same verdict but with the criterion id upper-cased — must score identically.
    all_A_upper_id = "\n".join(f"CRITERION_{i}: A" for i in range(1, n + 1))
    out_upper_id = parse_verdict(norm, all_A_upper_id)
    assert out_lower_id["overall"] == out_upper_id["overall"]
    # All-A on every (non-penalty-weighted) criterion is a perfect score.
    assert out_lower_id["overall"] == 100.0


def test_real_rubric_single_criterion_case_insensitive_scores_it():
    norm = normalize_rubric(_load_real_rubric_text())
    # Scoring `Criterion 1: A` in mixed case must still credit criterion_1.
    out = parse_verdict(norm, "CrItErIoN_1: A")
    assert out["overall"] > 0.0
