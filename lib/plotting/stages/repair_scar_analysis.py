"""Repair-scar renderer.

track_length_hist and structural_event_bar read scar_summary.json (authoritative
keys defined by the repair_scar_analysis atom). scar_segment_map reads the per-read
repair_scar_table.tsv whose columns are the [ASSUMPTION] schema — revise here if the
real column names differ.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict, List

import matplotlib.pyplot as plt

from ..core import bar, register_figure, resolve_artifact_path, savefig, stage_registry

FIGURES = stage_registry("repair_scar_analysis")


def _summary(ctx) -> Dict[str, Any]:
    path = ctx.outputs_dir / "scar_summary.json"
    if not path.exists():
        raise FileNotFoundError(f"{path} not found")
    return json.loads(path.read_text())


@register_figure(FIGURES, "track_length_hist")
def track_length_hist(ctx, out):
    hist: List[Dict[str, Any]] = _summary(ctx).get("track_length_histogram", [])
    names = [str(b["bin"]) for b in hist]
    values = [float(b["count"]) for b in hist]
    return bar(
        names=names,
        values=values,
        title="Track length distribution",
        ylabel="Reads",
        xlabel="Track length (bp)",
        out=out,
    )


@register_figure(FIGURES, "structural_event_bar")
def structural_event_bar(ctx, out):
    s = _summary(ctx)
    names = ["inversion", "deletion", "duplication", "unresolved gap"]
    values = [
        float(s.get("n_reads_with_inversion", 0)),
        float(s.get("n_reads_with_deletion", 0)),
        float(s.get("n_reads_with_duplication", 0)),
        float(s.get("n_gaps_unresolved", 0)),
    ]
    return bar(
        names=names,
        values=values,
        title="Structural repair events",
        ylabel="Count",
        out=out,
    )


@register_figure(FIGURES, "scar_segment_map")
def scar_segment_map(ctx, out):
    # [ASSUMPTION] table columns: read_id, segment, segment_index, read_start,
    # read_end, is_inverted. Draws one horizontal track per read (capped at 40).
    table = resolve_artifact_path(ctx, "scar_table_path", "repair_scar_table.tsv")
    fig, ax = plt.subplots(figsize=(10.0, 6.0))
    if table is not None and Path(table).exists():
        import csv

        rows: List[Dict[str, str]] = []
        with open(table, newline="") as fh:
            rows = list(csv.DictReader(fh, delimiter="\t"))
        reads = []
        for r in rows:
            if r.get("read_id") not in reads:
                reads.append(r.get("read_id"))
            if len(reads) >= 40:
                break
        for r in rows:
            rid = r.get("read_id")
            if rid not in reads:
                continue
            y = reads.index(rid)
            try:
                x0 = float(r.get("read_start", 0))
                x1 = float(r.get("read_end", 0))
            except ValueError:
                continue
            inverted = str(r.get("is_inverted", "")).lower() in ("true", "1")
            ax.barh(y, x1 - x0, left=x0, height=0.6,
                    color="#D55E00" if inverted else "#0072B2")
            ax.text(x0, y, str(r.get("segment", "")), fontsize=6, va="center")
        ax.set_yticks(range(len(reads)))
        ax.set_yticklabels(reads, fontsize=5)
    ax.set_xlabel("Read coordinate (bp)")
    ax.set_title("Per-read repair-scar segment map (orange = inverted)")
    return savefig(fig, out, stage_id="repair_scar_analysis")
