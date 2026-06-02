"""Preprocessing stage — retention and filtering summary plots. Works
across bulk + single-cell + proteomics + variant-calling since every
preprocessing stage produces per-sample retention counts.

Expected inputs:
- manifest.json with `samples: [{id, n_in, n_out}]`
  (retention counts before + after filtering)
- per-sample retention.tsv fallback not implemented — stick to the
  manifest-driven path to keep this module modality-agnostic.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

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

FIGURES = stage_registry("preprocessing")
VIEWS = stage_view_registry("preprocessing")


def _retention_rows(ctx: FigureContext) -> Optional[list]:
    samples = ctx.manifest.get("samples")
    if not isinstance(samples, list) or not samples:
        return None
    rows = []
    for s in samples:
        if not isinstance(s, dict):
            continue
        sid = str(s.get("id") or "?")
        n_in = s.get("n_in") or s.get("n_before")
        n_out = s.get("n_out") or s.get("n_after")
        if isinstance(n_in, (int, float)) and isinstance(n_out, (int, float)):
            rows.append((sid, int(n_in), int(n_out)))
    return rows if rows else None


@register_figure(FIGURES, "retention_bar")
def retention_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    rows = _retention_rows(ctx)
    if not rows:
        raise FileNotFoundError("manifest.samples[].n_in/n_out required")
    names = [r[0] for r in rows]
    n_in = [r[1] for r in rows]
    n_out = [r[2] for r in rows]
    x = np.arange(len(names))
    width = 0.38
    fig, ax = plt.subplots(figsize=(max(6.0, 0.4 * len(names)), 5.0))
    ax.bar(x - width / 2, n_in, width, label="before", color="lightsteelblue")
    ax.bar(x + width / 2, n_out, width, label="after", color="steelblue")
    ax.set_xticks(x)
    ax.set_xticklabels(names, rotation=45, ha="right", fontsize=7)
    ax.set_ylabel("count")
    ax.set_title("Retention per sample")
    ax.legend()
    fig.tight_layout()
    return savefig(fig, out)


@register_view(VIEWS, "retention_table")
def view_retention_table(ctx: FigureContext) -> dict:
    rows = _retention_rows(ctx)
    if not rows:
        raise FileNotFoundError("manifest.samples[].n_in/n_out required")
    return {
        "rows": [
            {
                "sample_id": sid,
                "n_in": n_in,
                "n_out": n_out,
                "pct_retained": (n_out / n_in * 100.0) if n_in > 0 else 0.0,
            }
            for sid, n_in, n_out in rows
        ]
    }
