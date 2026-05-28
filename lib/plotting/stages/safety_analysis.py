"""Safety analysis stage — clinical-trial taxonomy. Renders the SAP's
adverse-event summary bar (top-N preferred terms by count, optionally
severity-stratified) and a competing-risks cumulative-incidence plot
for SAE / discontinuation timing.

Plan reference: §S12 (clinical-trial-analysis stage `safety_analysis`).

Manifest contract:
- ``ae_table``: TSV with `term`, `count`, optional `severity`.
- ``cumulative_incidence_table``: TSV with `time`, `event` (cause code),
  optional `group`.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import (
    FigureContext,
    adverse_event_bar,
    cumulative_incidence,
    register_figure,
    stage_registry,
)
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("safety_analysis")


@register_figure(FIGURES, "adverse_event_bar")
def adverse_event_bar_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "ae_table")
    if p is None:
        raise FileNotFoundError("manifest.ae_table required")
    cols = load_tsv_columns(
        p,
        {
            "term": ("term", "AETERM", "preferred_term", "PT"),
            "count": ("count", "n", "AECOUNT", "events"),
            "severity?": ("severity", "AESEV", "grade", "AETOXGR"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable ae_table: {p}")
    severity_col = "severity" if "severity" in cols else None
    return adverse_event_bar(
        frame=cols,
        title="Adverse events",
        out=out,
        severity_col=severity_col,
    )


@register_figure(FIGURES, "cumulative_incidence")
def cumulative_incidence_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    p = manifest_path(ctx.manifest, ctx.outputs_dir, "cumulative_incidence_table")
    if p is None:
        raise FileNotFoundError("manifest.cumulative_incidence_table required")
    cols = load_tsv_columns(
        p,
        {
            "time": ("time", "follow_up", "AESTDY"),
            "event": ("event", "cause", "status"),
            "group?": ("group", "arm", "treatment"),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable cumulative-incidence table: {p}")
    group_col = "group" if "group" in cols else None
    return cumulative_incidence(
        frame=cols,
        title="Cumulative incidence",
        out=out,
        group_col=group_col,
    )
