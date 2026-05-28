"""Peak-annotation stage figures.

Manifest contract:
- ``annotation_table`` or ``peak_annotation_table`` pointing to a TSV
  with genomic feature labels and TSS-distance values.
- Fallback files: ``peak_annotation.tsv`` or ``annotation.tsv``.
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path
from typing import Optional

import numpy as np

from ..core import FigureContext, bar, register_figure, stage_registry
from ._shared import load_tsv_columns, manifest_path

FIGURES = stage_registry("peak_annotation")


def _annotation_table(ctx: FigureContext) -> Path:
    for key in ("annotation_table", "peak_annotation_table"):
        path = manifest_path(ctx.manifest, ctx.outputs_dir, key)
        if path is not None:
            return path
    for name in ("peak_annotation.tsv", "annotation.tsv"):
        path = ctx.outputs_dir / name
        if path.exists():
            return path
    raise FileNotFoundError("manifest.annotation_table or peak_annotation.tsv required")


def _columns(ctx: FigureContext) -> dict:
    path = _annotation_table(ctx)
    cols = load_tsv_columns(
        path,
        {
            "feature_class": (
                "feature_class",
                "annotation",
                "genomic_feature",
                "feature",
                "category",
            ),
            "distance_to_tss": (
                "distance_to_tss",
                "tss_distance",
                "distance",
                "dist_to_tss",
            ),
        },
    )
    if cols is None:
        raise FileNotFoundError(f"unparseable peak annotation table: {path}")
    return cols


@register_figure(FIGURES, "peak_feature_distribution")
def peak_feature_distribution(ctx: FigureContext, out: Path) -> Optional[Path]:
    cols = _columns(ctx)
    counts = Counter(str(v) for v in cols["feature_class"])
    ordered = sorted(counts.items(), key=lambda item: (-item[1], item[0]))[:20]
    if not ordered:
        raise FileNotFoundError("no feature_class rows in peak annotation table")
    return bar(
        [k for k, _v in ordered],
        [float(v) for _k, v in ordered],
        title="Peak genomic feature distribution",
        ylabel="peaks",
        out=out,
    )


@register_figure(FIGURES, "tss_distance_distribution")
def tss_distance_distribution(ctx: FigureContext, out: Path) -> Optional[Path]:
    cols = _columns(ctx)
    distances = np.asarray(cols["distance_to_tss"], dtype=float)
    abs_distance = np.abs(distances)
    bins = [
        ("<=1 kb", abs_distance <= 1_000),
        ("1-10 kb", (abs_distance > 1_000) & (abs_distance <= 10_000)),
        ("10-50 kb", (abs_distance > 10_000) & (abs_distance <= 50_000)),
        ("50-100 kb", (abs_distance > 50_000) & (abs_distance <= 100_000)),
        (">100 kb", abs_distance > 100_000),
    ]
    names = [name for name, _mask in bins]
    values = [float(mask.sum()) for _name, mask in bins]
    if sum(values) == 0:
        raise FileNotFoundError("no distance_to_tss values in peak annotation table")
    return bar(
        names,
        values,
        title="Distance to nearest TSS",
        ylabel="peaks",
        out=out,
    )
