"""Barcode-pairing renderer — sankey flow of barcodes through pairing steps
(multiome ARC demultiplex, SHARE-seq barcode match). Both atoms emit the
same swfc:multiome_paired_per_nucleus IRI; this single renderer covers
both via different figure_id entry points."""

from __future__ import annotations
import json
from pathlib import Path
from typing import List, Tuple

from ..core import register_figure, sankey, stage_registry

FIGURES = stage_registry("barcode_pairing")


def _load_flows(ctx) -> List[Tuple[str, str, float]]:
    p = ctx.manifest.get("summary_path")
    if p:
        path = Path(p) if Path(p).is_absolute() else ctx.outputs_dir / p
    else:
        path = ctx.outputs_dir / "pairing_summary.json"
    if not path.exists():
        raise FileNotFoundError(f"{path} not found")
    data = json.loads(path.read_text())
    return [(f["source"], f["target"], float(f["n"])) for f in data.get("flows", [])]


@register_figure(FIGURES, "paired_capture_summary")
def paired_capture_summary(ctx, out):
    flows = _load_flows(ctx)
    return sankey(flows=flows, title="Paired capture summary", out=out)


@register_figure(FIGURES, "barcode_match_summary")
def barcode_match_summary(ctx, out):
    flows = _load_flows(ctx)
    return sankey(flows=flows, title="Barcode match summary", out=out)
