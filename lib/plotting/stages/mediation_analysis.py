"""Mediation-analysis figures.

Manifest contract:
- ``mediation_results``: TSV with one row per candidate mediator.

The renderer accepts the common column names emitted by causal-mediation
implementations while keeping the plotted estimand explicit: the point and
interval are the indirect effect, not a generic benchmarking score.
"""

from __future__ import annotations

from pathlib import Path
from typing import Optional

from ..core import FigureContext, forest, register_figure, stage_registry
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("mediation_analysis")


def _results_path(ctx: FigureContext) -> Optional[Path]:
    path = manifest_path(ctx.manifest, ctx.outputs_dir, "mediation_results")
    if path is not None:
        return path
    fallback = ctx.outputs_dir / "mediation_results.tsv"
    return fallback if fallback.is_file() else None


@register_figure(FIGURES, "forest")
def forest_fig(ctx: FigureContext, out: Path) -> Optional[Path]:
    path = _results_path(ctx)
    if path is None:
        raise FileNotFoundError(
            "manifest.mediation_results or mediation_results.tsv required"
        )
    columns = load_tsv_columns(
        path,
        {
            "label": (
                "mediator",
                "mediator_variable",
                "candidate_mediator",
                "feature",
                "label",
            ),
            "effect": (
                "indirect_effect",
                "indirect_effect_estimate",
                "average_causal_mediation_effect",
                "acme",
                "estimate",
            ),
            "ci_lo": (
                "indirect_ci_lower",
                "indirect_effect_ci_lower",
                "acme_ci_lower",
                "ci_lower",
                "lower",
                "lcl",
            ),
            "ci_hi": (
                "indirect_ci_upper",
                "indirect_effect_ci_upper",
                "acme_ci_upper",
                "ci_upper",
                "upper",
                "ucl",
            ),
        },
    )
    if columns is None:
        raise FileNotFoundError(f"unparseable mediation result table: {path}")
    return forest(
        frame=columns,
        title="Indirect effects by mediator",
        out=out,
        xlabel="indirect effect (95% CI)",
        row_unit="mediators",
    )
