"""Pinned dataset fetch + cache. Pins live in scripts/eval/datasets.lock.

HF datasets are fetched by revision via huggingface_hub (content-addressed
by revision); git repos by clone+checkout. Fetched into ECAA_EVAL_CACHE_DIR
(default ~/.ecaa-workflow/eval-cache), which is gitignored.
"""
from __future__ import annotations
import json
import os
import shutil
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


def scratch_root() -> Path:
    """Base dir for eval workdirs + staged task inputs.

    Defaults to a sibling of the dataset cache so that multi-GB staging lands on
    the same (large, mounted) disk as the cache rather than on the root
    filesystem / /tmp — BiomniBench task inputs reach 15+ GB and would otherwise
    fill the root device (ENOSPC). Override with ECAA_EVAL_SCRATCH_DIR.
    """
    base = Path(os.environ.get("ECAA_EVAL_SCRATCH_DIR",
                               cache_root().parent / "eval-scratch"))
    base.mkdir(parents=True, exist_ok=True)
    return base


def eval_runs_dir() -> Path:
    """Base dir for scorecards + journals. Honors ECAA_EVAL_RUNS_DIR so a
    multi-day run's durable output lands on the large mounted disk rather than
    the repo's runtime/ on the (small) root filesystem."""
    root = Path(os.environ.get("ECAA_EVAL_RUNS_DIR",
                               Path(__file__).resolve().parents[3] / "runtime" / "eval-runs"))
    root.mkdir(parents=True, exist_ok=True)
    return root


def stage_file(src: Path, dst: Path) -> None:
    """Stage a task input into an agent workdir.

    Hardlinks when src and dst share a filesystem — instant and using no extra
    space, which matters because inputs reach 15+ GB and scratch_root() is on
    the same mounted disk as the dataset cache. Falls back to a byte copy across
    devices (EXDEV) or any other link failure. Overwrites an existing dst.
    """
    src = Path(src)
    dst = Path(dst)
    if dst.exists() or dst.is_symlink():
        dst.unlink()
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy(src, dst)


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


def fetch_complete(entry: LockEntry) -> Path:
    """Like ensure() but ALWAYS runs snapshot_download, even when the dest dir
    already exists — used to fill a partial dataset copy up to the full pinned
    revision. huggingface_hub skips files already present (size/etag match) and
    writes new files via temp+atomic-rename, so hardlinked inodes shared with
    another copy are never truncated in place. Only valid for hf_dataset."""
    if entry.kind != "hf_dataset":
        raise ValueError(f"fetch_complete only supports hf_dataset, got {entry.kind}")
    short = entry.revision[:12]
    dest = cache_root() / f"{entry.name.replace('/', '__')}@{short}"
    from huggingface_hub import snapshot_download
    token = os.environ.get("HF_TOKEN")
    snapshot_download(repo_id=entry.name, repo_type="dataset",
                      revision=entry.revision, local_dir=str(dest), token=token)
    return dest


def load_records(root) -> list[dict]:
    """Load benchmark task records from a fetched dataset dir.

    Supports four layouts (tried in order):
    1. parquet files (preferred; via lazy pyarrow import)
    2. jsonl file (one record per line)
    3. per-task json files named task*.json
    4. BiomniBench-DA per-task directory layout: da-{paper}-{task}/
       Each task dir contains instruction.md (question), tests/rubric.txt,
       and environment/data/ listing (data file refs). Returns dicts with
       keys question/rubric/data_files/task_id.
    """
    from pathlib import Path as _P
    import re as _re
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
    task_json = sorted(root.rglob("task*.json"))
    if task_json:
        return [json.loads(p.read_text()) for p in task_json]
    # BiomniBench-DA per-task directory layout: da-{paper}-{task}/
    task_dirs = sorted(
        [d for d in root.iterdir() if d.is_dir() and _re.match(r"da-\d+-\d+$", d.name)],
        key=lambda d: d.name,
    )
    if task_dirs:
        rows = []
        for td in task_dirs:
            instruction = td / "instruction.md"
            rubric = td / "tests" / "rubric.txt"
            data_dir = td / "environment" / "data"
            question = instruction.read_text() if instruction.exists() else ""
            rubric_text = rubric.read_text() if rubric.exists() else ""
            data_files = (
                [str(f.relative_to(root)) for f in sorted(data_dir.rglob("*")) if f.is_file()]
                if data_dir.exists() else []
            )
            rows.append({
                "task_id": td.name,
                "question": question,
                "rubric": rubric_text,
                "data_files": data_files,
            })
        return rows
    return []
