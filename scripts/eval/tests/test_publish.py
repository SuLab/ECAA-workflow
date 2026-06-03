"""publish.py copies the .public.* scorecard files into docs/eval-results/."""
import json
from pathlib import Path
from scripts.eval.publish import publish_run


def test_publish_copies_public_files(tmp_path):
    run_dir = tmp_path / "biomnibench-20260101T000000Z"
    run_dir.mkdir()
    (run_dir / "scorecard.public.json").write_text(
        json.dumps({"benchmark": "biomnibench",
                    "provenance": {"git_head": "abc"}}))
    (run_dir / "scorecard.public.md").write_text("# public\n")
    # A private scorecard.json must NOT be copied (cost not redacted).
    (run_dir / "scorecard.json").write_text(json.dumps({"total_cost_usd": 9.9}))
    dest_root = tmp_path / "docs" / "eval-results"
    out = publish_run(run_dir, dest_root)
    assert (out / "scorecard.public.json").exists()
    assert (out / "scorecard.public.md").exists()
    assert not (out / "scorecard.json").exists()
    # Dest dir named after the run dir.
    assert out.name == "biomnibench-20260101T000000Z"


def test_publish_refuses_run_without_public_scorecard(tmp_path):
    run_dir = tmp_path / "empty-run"
    run_dir.mkdir()
    import pytest
    with pytest.raises(FileNotFoundError, match="scorecard.public.json"):
        publish_run(run_dir, tmp_path / "dest")
