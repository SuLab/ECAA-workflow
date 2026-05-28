"""Data-acquisition stage figures — applies to every modality that
pulls samples down from an archive (GEO, SRA, ENA). Renders per-sample
byte counts and per-study sample totals so the SME sees what landed.

Expected inputs:
- manifest.json with `samples: [{id, size_bytes, study_id}]`
  or `studies: [{id, n_samples, total_bytes}]`
"""

from __future__ import annotations

from pathlib import Path
from typing import Dict, List, Optional

from ..core import (
    FigureContext,
    bar,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("data_acquisition")
VIEWS = stage_view_registry("data_acquisition")


def _per_study_totals(ctx: FigureContext) -> Optional[Dict[str, int]]:
    studies = ctx.manifest.get("studies")
    if isinstance(studies, list) and studies:
        out: Dict[str, int] = {}
        for s in studies:
            if not isinstance(s, dict):
                continue
            sid = str(s.get("id") or "?")
            n = s.get("n_samples") or s.get("n")
            if isinstance(n, (int, float)):
                out[sid] = int(n)
        return out if out else None
    samples = ctx.manifest.get("samples")
    if isinstance(samples, list) and samples:
        counts: Dict[str, int] = {}
        for s in samples:
            if not isinstance(s, dict):
                continue
            sid = str(s.get("study_id") or "ungrouped")
            counts[sid] = counts.get(sid, 0) + 1
        return counts if counts else None
    return None


@register_figure(FIGURES, "samples_per_study")
def samples_per_study(ctx: FigureContext, out: Path):
    totals = _per_study_totals(ctx)
    if not totals:
        raise FileNotFoundError("no studies/samples in manifest")
    names = sorted(totals.keys())
    values = [float(totals[n]) for n in names]
    return bar(
        names=names,
        values=values,
        title="Samples acquired per study",
        ylabel="n samples",
        xlabel="study",
        out=out,
    )


@register_view(VIEWS, "acquisition_summary")
def view_acquisition_summary(ctx: FigureContext) -> dict:
    totals = _per_study_totals(ctx)
    if not totals:
        raise FileNotFoundError("no studies/samples in manifest")
    return {
        "studies": [
            {"study_id": k, "n_samples": int(v)} for k, v in sorted(totals.items())
        ],
        "total_samples": sum(totals.values()),
    }
