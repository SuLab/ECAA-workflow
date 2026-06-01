"""Append-only JSONL journal for crash-safe, resumable eval runs.

Each completed base run / error-matrix cell / judge verdict appends one line
keyed by a stable id. ``--resume`` reads ``completed_keys()`` to skip work
already done (agent runs are expensive; never repeat them on restart)."""
from __future__ import annotations
import json
import threading
from pathlib import Path


class Journal:
    def __init__(self, run_dir: Path):
        self.run_dir = Path(run_dir)
        self.run_dir.mkdir(parents=True, exist_ok=True)
        self.path = self.run_dir / "journal.jsonl"
        self._lock = threading.Lock()
        self.path.touch(exist_ok=True)

    def append(self, record: dict) -> None:
        line = json.dumps(record, sort_keys=True)
        with self._lock:
            with self.path.open("a") as fh:
                fh.write(line + "\n")
                fh.flush()

    def records(self) -> list[dict]:
        if not self.path.exists():
            return []
        return [json.loads(ln) for ln in self.path.read_text().splitlines() if ln.strip()]

    def completed_keys(self) -> set[str]:
        return {r["key"] for r in self.records() if "key" in r}
