import json
import threading
from pathlib import Path
from scripts.eval.services.journal import Journal


def test_append_and_records(tmp_path):
    j = Journal(tmp_path)
    j.append({"key": "a", "v": 1})
    j.append({"key": "b", "v": 2})
    recs = j.records()
    assert [r["key"] for r in recs] == ["a", "b"]
    assert (tmp_path / "journal.jsonl").exists()


def test_completed_keys(tmp_path):
    j = Journal(tmp_path)
    j.append({"key": "x:0", "v": 1})
    j.append({"key": "y:1", "v": 2})
    assert j.completed_keys() == {"x:0", "y:1"}


def test_reopen_reads_existing(tmp_path):
    Journal(tmp_path).append({"key": "k1"})
    j2 = Journal(tmp_path)
    assert j2.completed_keys() == {"k1"}


def test_append_is_thread_safe(tmp_path):
    j = Journal(tmp_path)
    def worker(n):
        for i in range(20):
            j.append({"key": f"{n}:{i}"})
    threads = [threading.Thread(target=worker, args=(n,)) for n in range(8)]
    for t in threads: t.start()
    for t in threads: t.join()
    lines = (tmp_path / "journal.jsonl").read_text().splitlines()
    assert len(lines) == 160
    for ln in lines:
        json.loads(ln)  # every line is valid JSON (no interleaving)
    assert len(j.completed_keys()) == 160
