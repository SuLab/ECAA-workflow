import json
from scripts.eval.services.datasets import load_records

def test_load_records_jsonl(tmp_path):
    (tmp_path / "tasks.jsonl").write_text(
        json.dumps({"id":"t1","question":"q1"}) + "\n" +
        json.dumps({"id":"t2","question":"q2"}) + "\n")
    rows = load_records(tmp_path)
    assert [r["id"] for r in rows] == ["t1","t2"]

def test_load_records_per_task_json(tmp_path):
    (tmp_path / "task01.json").write_text(json.dumps({"id":"a"}))
    (tmp_path / "task02.json").write_text(json.dumps({"id":"b"}))
    rows = load_records(tmp_path)
    assert sorted(r["id"] for r in rows) == ["a","b"]
