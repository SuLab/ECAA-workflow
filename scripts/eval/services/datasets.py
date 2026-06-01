"""Pinned dataset fetch + cache. Pins live in scripts/eval/datasets.lock.

HF datasets are fetched by revision via huggingface_hub (content-addressed
by revision); git repos by clone+checkout. Fetched into ECAA_EVAL_CACHE_DIR
(default ~/.ecaa-workflow/eval-cache), which is gitignored.
"""
from __future__ import annotations
import json
import os
import subprocess
import tomllib
from dataclasses import dataclass
from pathlib import Path


@dataclass
class LockEntry:
    name: str
    kind: str       # "hf_dataset" | "git_repo"
    revision: str
    sha256: str | None = None


def load_lock(path: Path) -> dict[str, LockEntry]:
    data = tomllib.loads(Path(path).read_text())
    out: dict[str, LockEntry] = {}
    for e in data.get("entries", []):
        rev = e["revision"]
        if len(rev) < 40 and e["kind"] == "git_repo":
            raise ValueError(f"{e['name']}: git revision must be a full 40-char SHA")
        out[e["name"]] = LockEntry(e["name"], e["kind"], rev, e.get("sha256"))
    return out


def cache_root() -> Path:
    root = Path(os.environ.get("ECAA_EVAL_CACHE_DIR",
                               Path.home() / ".ecaa-workflow" / "eval-cache"))
    root.mkdir(parents=True, exist_ok=True)
    return root


def ensure(entry: LockEntry) -> Path:
    """Return the local path of the pinned dataset, fetching if absent."""
    short = entry.revision[:12]
    dest = cache_root() / f"{entry.name.replace('/', '__')}@{short}"
    if dest.exists():
        return dest
    if entry.kind == "hf_dataset":
        from huggingface_hub import snapshot_download
        token = os.environ.get("HF_TOKEN")
        snapshot_download(repo_id=entry.name, repo_type="dataset",
                          revision=entry.revision, local_dir=str(dest), token=token)
    elif entry.kind == "git_repo":
        url = f"https://github.com/{entry.name}.git"
        subprocess.run(["git", "clone", "--quiet", url, str(dest)], check=True)
        subprocess.run(["git", "-C", str(dest), "checkout", "--quiet", entry.revision],
                       check=True)
    else:
        raise ValueError(f"unknown kind: {entry.kind}")
    return dest


def load_records(root) -> list[dict]:
    """Load benchmark task records from a fetched dataset dir.

    Supports parquet (preferred; via lazy pyarrow import), then jsonl, then
    per-task json files named task*.json. Returns a list of row dicts.
    """
    from pathlib import Path as _P
    root = _P(root)
    pq = sorted(root.rglob("*.parquet"))
    if pq:
        import pyarrow.parquet as papq          # live-only path (needs pyarrow)
        rows: list[dict] = []
        for f in pq:
            rows.extend(papq.read_table(f).to_pylist())
        return rows
    jl = next(iter(sorted(root.rglob("*.jsonl"))), None)
    if jl:
        return [json.loads(l) for l in jl.read_text().splitlines() if l.strip()]
    return [json.loads(p.read_text()) for p in sorted(root.rglob("task*.json"))]
