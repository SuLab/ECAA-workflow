import os
from pathlib import Path
from unittest import mock
from scripts.eval.services import datasets
from scripts.eval.services.datasets import LockEntry


def test_eval_runs_dir_defaults_to_repo_runtime(monkeypatch):
    monkeypatch.delenv("ECAA_EVAL_RUNS_DIR", raising=False)
    d = datasets.eval_runs_dir()
    assert d.name == "eval-runs"
    assert d.parent.name == "runtime"
    assert d.is_dir()


def test_eval_runs_dir_honors_env(monkeypatch, tmp_path):
    target = tmp_path / "runs"
    monkeypatch.setenv("ECAA_EVAL_RUNS_DIR", str(target))
    d = datasets.eval_runs_dir()
    assert d == target
    assert d.is_dir()


def test_fetch_complete_calls_snapshot_even_when_dest_exists(monkeypatch, tmp_path):
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path))
    entry = LockEntry("phylobio/BiomniBench-DA", "hf_dataset", "a" * 40)
    dest = tmp_path / "phylobio__BiomniBench-DA@aaaaaaaaaaaa"
    dest.mkdir(parents=True)  # simulate a partial copy already present
    calls = {}
    fake_hub = mock.MagicMock()

    def fake_snapshot(**kw):
        calls.update(kw)
        return str(dest)

    fake_hub.snapshot_download = fake_snapshot
    with mock.patch.dict("sys.modules", {"huggingface_hub": fake_hub}):
        out = datasets.fetch_complete(entry)
    assert out == dest
    assert calls["repo_id"] == "phylobio/BiomniBench-DA"
    assert calls["revision"] == "a" * 40
    assert calls["local_dir"] == str(dest)
