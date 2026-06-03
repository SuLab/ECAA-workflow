"""Copy the redacted public scorecard files out of a run dir into the tracked
docs/eval-results/ evidence dir. Non-gated, no LLM, no network — pure file copy.
Refuses to publish a run that has no public (redacted) scorecard, so a raw
cost-carrying private scorecard can never be committed by accident."""
from __future__ import annotations
import shutil
import sys
from pathlib import Path

_PUBLIC_FILES = ("scorecard.public.json", "scorecard.public.md")


def publish_run(run_dir: Path, dest_root: Path) -> Path:
    run_dir = Path(run_dir)
    pub = run_dir / "scorecard.public.json"
    if not pub.exists():
        raise FileNotFoundError(
            f"{pub} not found — run the eval to produce a public scorecard "
            "before publishing (write_public_scorecard runs in the eval_runner).")
    dest = Path(dest_root) / run_dir.name
    dest.mkdir(parents=True, exist_ok=True)
    for name in _PUBLIC_FILES:
        src = run_dir / name
        if src.exists():
            shutil.copy2(src, dest / name)
    return dest


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print("usage: python3 -m scripts.eval.publish <run_dir>", file=sys.stderr)
        return 2
    repo_root = Path(__file__).resolve().parents[2]
    dest_root = repo_root / "docs" / "eval-results"
    out = publish_run(Path(argv[0]), dest_root)
    print(f"published public scorecard to {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
