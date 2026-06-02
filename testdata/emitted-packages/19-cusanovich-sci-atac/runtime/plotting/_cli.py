"""Thin CLI driver for the plot-affordance registry.

Usage:
    python3 -m lib.plotting._cli <stage_id> --input-dir <path> --output-dir <path>

Calls ``generate(stage_id, outputs_dir=input_dir, figures_dir=output_dir)``
and writes ``figures/manifest.json`` into ``output_dir`` (done automatically
by ``generate()`` when ``write_manifest=True``).

Exit code:
  0  — at least one figure was written without error.
  1  — no figures written (all skipped or errored).
  2  — argument error.
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser(
        prog="python3 -m lib.plotting._cli",
        description="Run a stage renderer on a stub input directory.",
    )
    parser.add_argument("stage_id", help="Stage id (e.g. differential_expression)")
    parser.add_argument(
        "--input-dir",
        required=True,
        help="Directory containing stage outputs/manifest.json",
    )
    parser.add_argument("--output-dir", required=True, help="Directory to write figures into")
    args = parser.parse_args()

    # Ensure lib.plotting is importable from repo root.
    repo_root = Path(__file__).resolve().parent.parent.parent
    if str(repo_root) not in sys.path:
        sys.path.insert(0, str(repo_root))

    from lib.plotting.core import generate  # noqa: PLC0415

    mf = generate(
        stage_id=args.stage_id,
        outputs_dir=Path(args.input_dir),
        figures_dir=Path(args.output_dir),
        required=None,
        write_manifest=True,
    )

    report = mf.to_json()
    print(json.dumps(report, indent=2, default=str), file=sys.stderr)

    if mf.written:
        sys.exit(0)
    else:
        # All figures either skipped or errored.
        sys.exit(1)


if __name__ == "__main__":
    main()
