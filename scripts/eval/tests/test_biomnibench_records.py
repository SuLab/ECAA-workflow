"""Offline tests for BiomniBench-DA record loading and rubric normalization.

Fixtures in tests/fixtures/ encode the real BiomniBench-DA schema (per-task
directory layout: da-{paper}-{task}/instruction.md + tests/rubric.txt +
environment/data/). These tests exercise load_records + normalize_rubric
without any network access.
"""
import json
from pathlib import Path

import pytest

from scripts.eval.rubric_normalize import normalize_rubric
from scripts.eval.services.datasets import load_records
from scripts.eval.services.judge import parse_verdict

FIXTURES = Path(__file__).parent / "fixtures"


def test_bbench_record_fixture_has_required_keys():
    """The representative fixture encodes the real load_records output shape."""
    rec = json.loads((FIXTURES / "bbench_record.json").read_text())
    assert set(rec) >= {"task_id", "question", "rubric", "data_files"}
    assert rec["task_id"] == "da-1-3"
    assert len(rec["question"]) > 50
    assert isinstance(rec["rubric"], str) and len(rec["rubric"]) > 20
    assert isinstance(rec["data_files"], list) and len(rec["data_files"]) > 0


def test_bbench_rubric_fixture_is_plain_text():
    """bbench_rubric.json stores a JSON string (plain-text rubric.txt content)."""
    raw = json.loads((FIXTURES / "bbench_rubric.json").read_text())
    assert isinstance(raw, str)
    assert len(raw) > 20


def test_normalize_rubric_from_text_fixture():
    """normalize_rubric wraps a text rubric as one holistic criterion."""
    raw = json.loads((FIXTURES / "bbench_rubric.json").read_text())
    norm = normalize_rubric(raw)
    assert "criteria" in norm
    assert len(norm["criteria"]) == 1
    c = norm["criteria"][0]
    assert set(c) >= {"id", "dimension", "points", "levels"}
    assert c["id"] == "overall"
    assert c["dimension"] == "scientific_reasoning"
    assert c["levels"] == {"A": 1.0, "B": 0.5, "C": 0.0}
    assert c["text"].strip() == raw.strip()


def test_normalize_rubric_text_parse_verdict_a():
    raw = json.loads((FIXTURES / "bbench_rubric.json").read_text())
    norm = normalize_rubric(raw)
    verdict = parse_verdict(norm, "overall: A")
    assert verdict["overall"] == 100.0
    assert verdict["levels"] == {"overall": "A"}
    assert "scientific_reasoning" in verdict["dimensions"]


def test_normalize_rubric_text_parse_verdict_b():
    raw = json.loads((FIXTURES / "bbench_rubric.json").read_text())
    norm = normalize_rubric(raw)
    verdict = parse_verdict(norm, "overall: B")
    assert verdict["overall"] == 50.0


def test_normalize_rubric_text_parse_verdict_c():
    raw = json.loads((FIXTURES / "bbench_rubric.json").read_text())
    norm = normalize_rubric(raw)
    verdict = parse_verdict(norm, "overall: C")
    assert verdict["overall"] == 0.0


def test_load_records_per_task_dir_layout(tmp_path):
    """load_records handles BiomniBench-DA da-{paper}-{task}/ directory structure."""
    # Construct a minimal representative directory tree
    for task_id in ["da-1-3", "da-2-1"]:
        td = tmp_path / task_id
        (td / "tests").mkdir(parents=True)
        (td / "environment" / "data").mkdir(parents=True)
        (td / "instruction.md").write_text(f"Analyze dataset for task {task_id}.")
        (td / "tests" / "rubric.txt").write_text(f"Grade the analysis for {task_id}.")
        (td / "environment" / "data" / "counts.csv").write_text("gene,sample1\nTP53,10\n")

    records = load_records(tmp_path)
    assert len(records) == 2
    ids = {r["task_id"] for r in records}
    assert ids == {"da-1-3", "da-2-1"}

    r = next(r for r in records if r["task_id"] == "da-1-3")
    assert "Analyze dataset" in r["question"]
    assert "Grade the analysis" in r["rubric"]
    assert any("counts.csv" in f for f in r["data_files"])


def test_load_records_per_task_dir_ignores_non_task_dirs(tmp_path):
    """Non-da-* directories are ignored in the per-task-dir layout."""
    (tmp_path / "da-1-3").mkdir()
    (tmp_path / "da-1-3" / "instruction.md").write_text("Q")
    (tmp_path / "da-1-3" / "tests").mkdir()
    (tmp_path / "da-1-3" / "tests" / "rubric.txt").write_text("R")
    (tmp_path / "README.md").write_text("# ignored")
    (tmp_path / "some_other_dir").mkdir()

    records = load_records(tmp_path)
    assert len(records) == 1
    assert records[0]["task_id"] == "da-1-3"


def test_load_records_per_task_dir_missing_rubric(tmp_path):
    """Missing rubric.txt results in empty rubric string (not an error)."""
    td = tmp_path / "da-5-1"
    td.mkdir()
    (td / "instruction.md").write_text("Some task question.")
    (td / "tests").mkdir()
    # No rubric.txt

    records = load_records(tmp_path)
    assert len(records) == 1
    assert records[0]["rubric"] == ""
    assert records[0]["question"] == "Some task question."


def test_full_pipeline_text_rubric_from_record_fixture():
    """Full pipeline: record fixture -> normalize_rubric -> parse_verdict."""
    rec = json.loads((FIXTURES / "bbench_record.json").read_text())
    norm = normalize_rubric(rec["rubric"])
    assert len(norm["criteria"]) >= 1
    verdict = parse_verdict(norm, "overall: A")
    assert verdict["overall"] == 100.0
    assert verdict["levels"].get("overall") == "A"
