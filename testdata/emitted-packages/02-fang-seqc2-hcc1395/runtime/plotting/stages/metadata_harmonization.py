"""Metadata-harmonization stage figures (Phase 1 of the IVD compliance
remediation). Reads the canonical per-sample TSV produced by this
stage and renders a sample × attribute tile heatmap.

Expected inputs:
- runtime/outputs/metadata_harmonization/sample_metadata.tsv
  with columns: study, sample_key, condition, age_years,
  pfirrmann_grade, sex, donor_id, compartment, ivd_score
"""

from __future__ import annotations

from pathlib import Path
from typing import List, Optional

import numpy as np

from ..core import (
    FigureContext,
    register_figure,
    register_view,
    savefig,
    stage_registry,
    stage_view_registry,
)
import matplotlib.pyplot as plt

FIGURES = stage_registry("metadata_harmonization")
VIEWS = stage_view_registry("metadata_harmonization")


def _load_metadata_rows(ctx: FigureContext) -> Optional[List[dict]]:
    p = ctx.outputs_dir / "sample_metadata.tsv"
    if not p.exists():
        return None
    try:
        with p.open() as f:
            header = f.readline().rstrip("\n").split("\t")
            rows: List[dict] = []
            for line in f:
                parts = line.rstrip("\n").split("\t")
                if len(parts) < len(header):
                    parts.extend([""] * (len(header) - len(parts)))
                row = {h: v for h, v in zip(header, parts)}
                rows.append(row)
            return rows if rows else None
    except OSError:
        return None


@register_figure(FIGURES, "metadata_attribute_heatmap")
def metadata_attribute_heatmap(ctx: FigureContext, out: Path) -> Optional[Path]:
    rows = _load_metadata_rows(ctx)
    if not rows:
        raise FileNotFoundError("sample_metadata.tsv not present")
    # Attributes to surface (all spec-canonical; missing → unknown)
    attrs = [
        "condition",
        "compartment",
        "age_years",
        "pfirrmann_grade",
        "sex",
        "ivd_score",
    ]
    samples = [r.get("sample_key") or r.get("study") or "?" for r in rows]
    # Build a categorical matrix: map each value to a color index per
    # attribute so the figure shows which samples share which levels.
    matrix = np.zeros((len(rows), len(attrs)))
    legends = []
    for j, attr in enumerate(attrs):
        values = [r.get(attr, "") or "unknown" for r in rows]
        uniq = sorted(set(values))
        idx_map = {v: i for i, v in enumerate(uniq)}
        for i, v in enumerate(values):
            matrix[i, j] = idx_map[v]
        legends.append((attr, uniq))

    # Per-column categorical encoding rendered as a sequential colormap:
    # each cell's value is an index into that column's unique values, so
    # within a column color → category, but across columns the index
    # space is independent. viridis is perceptually uniform and reads
    # cleanly even when the index spaces overlap.
    fig, ax = plt.subplots(
        figsize=(max(6.0, 0.6 * len(attrs)), max(4.0, 0.15 * len(samples))),
    )
    im = ax.imshow(matrix, cmap="viridis", aspect="auto", interpolation="nearest")
    ax.set_xticks(range(len(attrs)))
    ax.set_xticklabels(attrs, rotation=30, ha="right")
    ax.set_yticks(range(len(samples)))
    ax.set_yticklabels(samples)
    ax.set_title("Sample × metadata attribute (categorical)")
    fig.colorbar(im, ax=ax, shrink=0.6, label="category index (per column)")
    return savefig(fig, out)


@register_view(VIEWS, "metadata_table")
def view_metadata_table(ctx: FigureContext) -> dict:
    rows = _load_metadata_rows(ctx)
    if not rows:
        raise FileNotFoundError("sample_metadata.tsv not present")
    # Truncate to avoid massive payloads for huge cohorts.
    return {
        "rows": rows[:500],
        "n_total": len(rows),
        "columns": list(rows[0].keys()) if rows else [],
    }
