"""Append-only JSONL journal for crash-safe, resumable eval runs.

Each completed base run / error-matrix cell / judge verdict appends one line
keyed by a stable id. ``--resume`` reads ``completed_keys()`` to skip work
already done (agent runs are expensive; never repeat them on restart)."""
from __future__ import annotations
import fcntl
import json
import os
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
        # O_APPEND makes the kernel resolve the write offset to EOF atomically,
        # and the advisory flock(LOCK_EX) serializes concurrent appenders across
        # processes; the thread lock keeps the same guarantee within a process.
        line = json.dumps(record, sort_keys=True) + "\n"
        with self._lock:
            with self.path.open("a", encoding="utf-8") as fh:
                fcntl.flock(fh.fileno(), fcntl.LOCK_EX)
                try:
                    fh.write(line)
                    fh.flush()
                    os.fsync(fh.fileno())
                finally:
                    fcntl.flock(fh.fileno(), fcntl.LOCK_UN)

    def records(self) -> list[dict]:
        if not self.path.exists():
            return []
        return [json.loads(ln) for ln in self.path.read_text().splitlines() if ln.strip()]

    def completed_keys(self) -> set[str]:
        return {r["key"] for r in self.records() if "key" in r}

    def recovered_keys(self) -> set[str]:
        """Keys whose record carries a truthy ``recovered`` flag — records replayed
        from a prior run's journal on --resume rather than freshly executed. Lets a
        post-hoc audit separate re-run work from journal-reconstructed work."""
        return {r["key"] for r in self.records()
                if "key" in r and r.get("recovered")}
