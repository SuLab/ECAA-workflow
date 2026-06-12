"""Package-portable render entrypoint for the shipped plotting library.

Render-as-Contract: figure rendering is a FIXED, non-LLM step over the
standardized compute-output tables (the figure-data contract). This module is
that step's entrypoint — runnable inside any emitted package as

    python3 -m runtime.plotting render \
        --stage <stage_id> --outputs runtime/outputs/<task_id> --required a,b,c

It wraps `core.generate()` via a RELATIVE import, so the identical module works
under any parent package name (`runtime.plotting` in a shipped package,
`lib.plotting` in the repo) without a path-rewrite. Unlike `_cli.py` (a
repo-only stub-render driver that hardcodes `lib.plotting` + a repo_root), this
entrypoint is portable and accepts the task's `required_figures` subset.

The result manifest is emitted as JSON on STDOUT so the agent wrapper can
capture it into `task.state.result`; human-readable progress goes to STDERR.

Exit codes:
  0 — at least one required figure was written.
  1 — no figures written (all skipped/errored, or unknown stage).
  2 — argument error (argparse).
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

from .core import generate


def _split_required(value: str | None) -> list[str] | None:
    if not value:
        return None
    figs = [f.strip() for f in value.split(",")]
    figs = [f for f in figs if f]
    return figs or None


def _render(args: argparse.Namespace) -> int:
    outputs_dir = Path(args.outputs)
    figures_dir = Path(args.figures) if args.figures else None
    mf = generate(
        stage_id=args.stage,
        outputs_dir=outputs_dir,
        figures_dir=figures_dir,
        required=_split_required(args.required),
        write_manifest=True,
    )

    report = {
        "stage_id": mf.stage_id,
        "written": {fid: str(p) for fid, p in mf.written.items()},
        "skipped": dict(mf.skipped),
        "errors": dict(mf.errors),
    }
    # Machine-readable result on stdout; the wrapper captures this verbatim.
    print(json.dumps(report))
    # Human-readable summary on stderr (does not pollute the JSON channel).
    summary = (
        f"render[{mf.stage_id}]: {len(mf.written)} written, "
        f"{len(mf.skipped)} skipped, {len(mf.errors)} errored"
    )
    print(summary, file=sys.stderr)

    return 0 if mf.written else 1


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        prog="python3 -m runtime.plotting",
        description="Fixed, non-LLM figure rendering over compute-output tables.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    render = sub.add_parser(
        "render",
        help="Render a stage's required figures from its output tables.",
    )
    render.add_argument(
        "--stage",
        required=True,
        help="Renderer stage_id (the task's plot_stage_id, else its task id).",
    )
    render.add_argument(
        "--outputs",
        required=True,
        help="Stage outputs dir holding the contract tables + manifest.json "
        "(e.g. runtime/outputs/<task_id>).",
    )
    render.add_argument(
        "--figures",
        default=None,
        help="Figures output dir (default: <outputs>/figures).",
    )
    render.add_argument(
        "--required",
        default=None,
        help="Comma-separated figure_ids to render (default: every figure the "
        "stage registers).",
    )
    render.set_defaults(func=_render)

    args = parser.parse_args(argv)
    return args.func(args)


if __name__ == "__main__":
    sys.exit(main())
