"""Controlled-access data-acquisition figures.

This stage intentionally keeps its own id for governance and executor
policy checks, but it emits the same cohort-manifest shape as the public
data_acquisition atom. Reuse that manifest parser while registering the
figure under the controlled-access plot stage id.
"""

from __future__ import annotations

from pathlib import Path

from .data_acquisition import _per_study_totals
from ..core import (
    FigureContext,
    bar,
    register_figure,
    register_view,
    stage_registry,
    stage_view_registry,
)

FIGURES = stage_registry("controlled_access_data_acquisition")
VIEWS = stage_view_registry("controlled_access_data_acquisition")


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
        title="Controlled-access samples per study",
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
        "controlled_access": True,
    }
