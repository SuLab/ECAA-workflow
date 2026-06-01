# scripts/eval/tests/test_datasets.py
import os
from pathlib import Path
from scripts.eval.services.datasets import load_lock, LockEntry, scratch_root, stage_file

def test_load_lock(tmp_path):
    lock = tmp_path / "datasets.lock"
    lock.write_text(
        '[[entries]]\n'
        'name = "phylobio/BiomniBench-DA"\n'
        'kind = "hf_dataset"\n'
        'revision = "0000000000000000000000000000000000000000"\n'
        '\n'
        '[[entries]]\n'
        'name = "nekrut/LLM-eval-paper"\n'
        'kind = "git_repo"\n'
        'revision = "1111111111111111111111111111111111111111"\n'
    )
    entries = load_lock(lock)
    assert entries["phylobio/BiomniBench-DA"].kind == "hf_dataset"
    assert len(entries["nekrut/LLM-eval-paper"].revision) == 40


def test_scratch_root_honors_env(tmp_path, monkeypatch):
    target = tmp_path / "big-disk" / "eval-scratch"
    monkeypatch.setenv("ECAA_EVAL_SCRATCH_DIR", str(target))
    got = scratch_root()
    assert got == target
    assert got.is_dir()  # created on demand


def test_scratch_root_defaults_beside_cache(tmp_path, monkeypatch):
    monkeypatch.setenv("ECAA_EVAL_CACHE_DIR", str(tmp_path / "c" / "eval-cache"))
    monkeypatch.delenv("ECAA_EVAL_SCRATCH_DIR", raising=False)
    # Default sits beside the cache (same parent), so it lands on the same disk.
    assert scratch_root() == (tmp_path / "c" / "eval-scratch")


def test_stage_file_hardlinks_same_fs(tmp_path):
    src = tmp_path / "counts.mtx"
    src.write_text("matrix-bytes")
    dst = tmp_path / "work" / "counts.mtx"
    dst.parent.mkdir()
    stage_file(src, dst)
    assert dst.read_text() == "matrix-bytes"
    # Same filesystem -> hardlink (same inode), so no extra space is used.
    assert os.stat(src).st_ino == os.stat(dst).st_ino
    assert os.stat(src).st_nlink >= 2


def test_stage_file_overwrites_existing_dst(tmp_path):
    src = tmp_path / "a.txt"
    src.write_text("new")
    dst = tmp_path / "b.txt"
    dst.write_text("stale")
    stage_file(src, dst)
    assert dst.read_text() == "new"
