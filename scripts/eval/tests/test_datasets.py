# scripts/eval/tests/test_datasets.py
from pathlib import Path
from scripts.eval.services.datasets import load_lock, LockEntry

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
