"""Print the cached BiomniBench-DA scenario catalog so an operator can pick a
``TASKS=`` slice for ``make eval-tractable``. Offline + read-only: it scans the
already-fetched dataset under the eval cache and never hits the network.

For each per-task dir (``phylobio__BiomniBench-DA*/da-{paper}-{task}/``) it prints
``id  category  difficulty  data-size``:

* ``category`` / ``difficulty`` are read from the task's ``task.toml`` when one
  is present (tolerated keys ``category``/``difficulty``, nested under an optional
  ``[task]`` table). The published BiomniBench-DA layout does not guarantee a
  ``task.toml``, so a missing/unreadable file degrades to ``?`` rather than
  failing the listing.
* ``data-size`` is ``du -sh`` over the task's ``environment/data`` dir (``-`` when
  absent).

If the cache dir or dataset is missing, it prints the dir it scanned and exits 0
(no hard failure) so the target is safe to run before any dataset is fetched.
"""
from __future__ import annotations

import glob
import os
import subprocess
import sys
from pathlib import Path


def _cache_dir() -> Path:
    """Resolve the eval cache dir the same way services.datasets.cache_root does,
    WITHOUT importing it (that creates the dir + warns on low space). Read-only."""
    return Path(os.environ.get("ECAA_EVAL_CACHE_DIR",
                               Path.home() / ".ecaa-workflow" / "eval-cache"))


def _read_task_meta(task_dir: Path) -> tuple[str, str]:
    """Return (category, difficulty) from a task dir's task.toml, or ('?','?').

    Tolerant: a missing/unparsable task.toml, or one without the keys, yields
    '?' — the published per-task layout is not guaranteed to carry one."""
    toml_path = task_dir / "task.toml"
    if not toml_path.exists():
        return "?", "?"
    try:
        import tomllib
        data = tomllib.loads(toml_path.read_text())
    except (OSError, ValueError):
        return "?", "?"
    # Accept either top-level keys or a nested [task] table.
    table = data.get("task") if isinstance(data.get("task"), dict) else data
    cat = table.get("category", data.get("category", "?"))
    diff = table.get("difficulty", data.get("difficulty", "?"))
    return str(cat), str(diff)


def _data_size(task_dir: Path) -> str:
    """Human ``du -sh`` size of the task's environment/data dir ('-' when absent)."""
    data_dir = task_dir / "environment" / "data"
    if not data_dir.exists():
        return "-"
    try:
        out = subprocess.run(["du", "-sh", str(data_dir)],
                             capture_output=True, text=True, check=False)
        first = out.stdout.split()
        return first[0] if first else "-"
    except OSError:
        return "-"


def main(argv: list[str] | None = None) -> int:
    cache = _cache_dir()
    print(f"[eval-list-bbench] scanning cache: {cache}")
    datasets = sorted(glob.glob(str(cache / "phylobio__BiomniBench-DA*")))
    if not datasets:
        print("  (no cached BiomniBench-DA dataset found under that dir; run an "
              "eval once to fetch it, or set ECAA_EVAL_CACHE_DIR)")
        return 0
    for d in datasets:
        print(f"  dataset: {d}")
    print()
    print(f"{'id':<14} {'category':<22} {'difficulty':<10} data-size")
    found = False
    for d in datasets:
        for td in sorted(glob.glob(str(Path(d) / "da-*"))):
            task_dir = Path(td)
            if not task_dir.is_dir():
                continue
            found = True
            cat, diff = _read_task_meta(task_dir)
            size = _data_size(task_dir)
            print(f"{task_dir.name:<14} {cat:<22} {diff:<10} {size}")
    if not found:
        print("  (dataset dir present but no da-* task dirs found)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
