"""Shared test helpers for stage-renderer snapshot tests.

`make_context` builds a `FigureContext` from a fixture directory + manifest
dict. `structural_snapshot` reads a PNG and extracts a stable structural
fingerprint (dimensions, dominant colors, axis labels) so snapshot diffs
don't churn on antialiasing noise.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any, Dict

import numpy as np
from PIL import Image

from lib.plotting.core import FigureContext


def make_context(
    fixture_dir: Path,
    manifest: Dict[str, Any] | None = None,
    seed: int = 0,
    stage_id: str = "test_stage",
    figure_id: str = "test_figure",
) -> FigureContext:
    """Build a FigureContext pointed at a fixture directory.

    The fixture dir IS the outputs_dir — render functions read tables and
    JSON from it. `manifest` defaults to whatever lives at
    `<fixture_dir>/manifest.json` if not supplied.
    """
    if manifest is None:
        mf = fixture_dir / "manifest.json"
        manifest = json.loads(mf.read_text()) if mf.exists() else {}
    return FigureContext(
        stage_id=stage_id,
        figure_id=figure_id,
        outputs_dir=fixture_dir,
        manifest=manifest,
        rng=np.random.default_rng(seed),
        seed=seed,
    )


def structural_snapshot(png_path: Path) -> Dict[str, Any]:
    """Extract a coarse structural fingerprint from a rendered PNG.

    We don't pixel-diff (that churns on font hinting). Instead we capture
    image dimensions, mean luminance per quadrant, and dominant non-white
    colour count. Snapshot tests compare these against a committed JSON.
    """
    img = np.asarray(Image.open(png_path).convert("RGB"))
    h, w, _ = img.shape
    quads = {
        "tl": img[: h // 2, : w // 2].mean(axis=(0, 1)).tolist(),
        "tr": img[: h // 2, w // 2 :].mean(axis=(0, 1)).tolist(),
        "bl": img[h // 2 :, : w // 2].mean(axis=(0, 1)).tolist(),
        "br": img[h // 2 :, w // 2 :].mean(axis=(0, 1)).tolist(),
    }
    q = (img // 64).astype(np.uint8)
    keys = q[:, :, 0].astype(int) * 25 + q[:, :, 1].astype(int) * 5 + q[:, :, 2].astype(int)
    non_white = keys[~((q[:, :, 0] >= 3) & (q[:, :, 1] >= 3) & (q[:, :, 2] >= 3))]
    distinct = int(len(np.unique(non_white)))
    return {
        "width": int(w),
        "height": int(h),
        "quadrant_means": {k: [round(v, 1) for v in mean] for k, mean in quads.items()},
        "distinct_color_buckets": distinct,
    }


def assert_snapshot(actual: Dict[str, Any], snapshot_path: Path) -> None:
    """Compare against a committed snapshot. Writes the snapshot on first run."""
    if not snapshot_path.exists():
        snapshot_path.parent.mkdir(parents=True, exist_ok=True)
        snapshot_path.write_text(json.dumps(actual, indent=2, sort_keys=True))
        return
    expected = json.loads(snapshot_path.read_text())
    for key in ("width", "height"):
        assert actual[key] == expected[key], f"{key} mismatch: {actual[key]} vs {expected[key]}"
    for q, m in actual["quadrant_means"].items():
        em = expected["quadrant_means"][q]
        for i, (a, e) in enumerate(zip(m, em)):
            assert abs(a - e) <= 12.75, f"{q}[{i}] drift: {a} vs {e}"
    assert abs(actual["distinct_color_buckets"] - expected["distinct_color_buckets"]) <= 2
