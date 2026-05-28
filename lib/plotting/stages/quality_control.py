"""QC stage figures — applies across every modality that produces
per-sample quality metrics (bulk RNA-seq, scRNA-seq, ChIP-seq, variant
calling, proteomics, metagenomics, long-read RNA-seq).

Expected inputs (in outputs_dir, any subset works):
- manifest.json with a "per_sample_metrics" map of sample_id → metric dict
- qc_metrics.tsv / qc_metrics.tsv.gz: long-form rows with columns
  `sample`, `metric`, `value`
- summary_stats.json with aggregate numbers

Stage is tolerant: missing inputs → the figure lands in `skipped`, not
`errors`, so the stage still produces every figure it can.
"""

from __future__ import annotations

import gzip
import json
from pathlib import Path
from typing import Dict, List, Optional, Tuple

import numpy as np

from ..core import FigureContext, bar, stage_registry, violin, register_figure

FIGURES = stage_registry("quality_control")


def _load_long_metrics(
    outputs_dir: Path,
) -> Optional[Dict[str, Dict[str, List[float]]]]:
    """Parse qc_metrics.{tsv,tsv.gz} → {metric: {sample: [values]}}. Returns
    None if the file is missing or unparseable so the caller can fall
    through to the manifest-derived path.
    """
    for name in ("qc_metrics.tsv.gz", "qc_metrics.tsv"):
        p = outputs_dir / name
        if not p.exists():
            continue
        opener = gzip.open if name.endswith(".gz") else open
        try:
            with opener(p, "rt") as f:
                header = f.readline().strip().split("\t")
                try:
                    i_sample = header.index("sample")
                    i_metric = header.index("metric")
                    i_value = header.index("value")
                except ValueError:
                    return None
                out: Dict[str, Dict[str, List[float]]] = {}
                for line in f:
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) <= max(i_sample, i_metric, i_value):
                        continue
                    try:
                        val = float(parts[i_value])
                    except ValueError:
                        continue
                    metric = parts[i_metric]
                    sample = parts[i_sample]
                    out.setdefault(metric, {}).setdefault(sample, []).append(val)
                return out
        except OSError:
            return None
    return None


def _manifest_per_sample(ctx: FigureContext) -> Optional[Tuple[List[str], Dict[str, List[float]]]]:
    """Pull per-sample scalar metrics out of manifest.json. Returns
    (samples, {metric: values-in-sample-order}) or None.
    """
    per_sample = ctx.manifest.get("per_sample_metrics")
    if not isinstance(per_sample, dict) or not per_sample:
        return None
    samples = sorted(per_sample.keys())
    # Collect metric set
    metric_names: set = set()
    for s in samples:
        if isinstance(per_sample[s], dict):
            metric_names.update(
                k for k, v in per_sample[s].items() if isinstance(v, (int, float))
            )
    out: Dict[str, List[float]] = {}
    for m in sorted(metric_names):
        vals = []
        for s in samples:
            v = per_sample.get(s, {}).get(m)
            vals.append(float(v) if isinstance(v, (int, float)) else np.nan)
        out[m] = vals
    if not out:
        return None
    return samples, out


@register_figure(FIGURES, "per_sample_metric_violin")
def per_sample_metric_violin(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Violin per QC metric across samples. Falls back to a bar when we
    only have one value per sample per metric (no per-cell distribution
    in the artifact).
    """
    long_form = _load_long_metrics(ctx.outputs_dir)
    if long_form:
        # Pick the metric with the widest variation for the primary violin
        metric = next(iter(long_form))
        return violin(
            data=long_form[metric],
            title=f"QC: {metric} per sample",
            ylabel=metric,
            out=out,
            x_label="sample",
        )
    manifest_data = _manifest_per_sample(ctx)
    if manifest_data is None:
        raise FileNotFoundError("no qc_metrics.tsv[.gz] or manifest.per_sample_metrics")
    samples, metrics = manifest_data
    # Pick the first metric and render as a bar (one value per sample).
    metric_name = next(iter(metrics))
    return bar(
        names=samples,
        values=metrics[metric_name],
        title=f"QC: {metric_name} per sample",
        ylabel=metric_name,
        out=out,
        xlabel="sample",
    )


@register_figure(FIGURES, "per_sample_metric_bar")
def per_sample_metric_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Bar chart of the cardinal per-sample count (n_cells for scRNA,
    n_reads for bulk, n_variants for calling, etc.). Reads
    manifest.per_sample_metrics — the primary metric is whichever key
    contains the substring `count` or `n_`, else the first key.
    """
    manifest_data = _manifest_per_sample(ctx)
    if manifest_data is None:
        raise FileNotFoundError("manifest.per_sample_metrics required for per_sample_metric_bar")
    samples, metrics = manifest_data
    preferred = next(
        (k for k in metrics if ("count" in k.lower() or k.lower().startswith("n_"))),
        next(iter(metrics)),
    )
    return bar(
        names=samples,
        values=metrics[preferred],
        title=f"QC: {preferred} per sample",
        ylabel=preferred,
        out=out,
        xlabel="sample",
    )


@register_figure(FIGURES, "qc_summary_bar")
def qc_summary_bar(ctx: FigureContext, out: Path) -> Optional[Path]:
    """Bar of top-level QC summary numbers from summary_stats.json when
    present. Useful when the stage didn't emit per-sample data but
    produced aggregate counts.
    """
    summary_path = ctx.outputs_dir / "summary_stats.json"
    if not summary_path.exists():
        raise FileNotFoundError("summary_stats.json")
    try:
        summary = json.loads(summary_path.read_text())
    except json.JSONDecodeError as e:
        raise FileNotFoundError(f"summary_stats.json unparseable: {e}") from e
    scalar = {k: v for k, v in summary.items() if isinstance(v, (int, float))}
    if not scalar:
        raise FileNotFoundError("summary_stats.json has no scalar metrics")
    names = list(scalar.keys())
    values = [float(scalar[n]) for n in names]
    return bar(
        names=names,
        values=values,
        title="QC: aggregate summary",
        ylabel="value",
        out=out,
    )
