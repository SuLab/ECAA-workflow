"""Tests for the reporting-stage figures, incl. the pathway_overlap_bar
NES/direction channel (RP-6: an enriched (NES>0) and a depleted
(NES<0) pathway used to render as visually identical bars)."""

from __future__ import annotations

from pathlib import Path
from typing import Tuple

import numpy as np
import pytest
from PIL import Image

from lib.plotting.core import THEME
from lib.plotting.stages import reporting as stage
from lib.plotting.tests.helpers import make_context


def _hex_to_rgb(hex_color: str) -> Tuple[int, int, int]:
    hex_color = hex_color.lstrip("#")
    return tuple(int(hex_color[i : i + 2], 16) for i in (0, 2, 4))  # noqa: E203


def _has_color(img: np.ndarray, rgb: Tuple[int, int, int], tol: int = 12) -> bool:
    diff = np.abs(img.astype(int) - np.array(rgb, dtype=int))
    return bool(np.any(np.all(diff <= tol, axis=-1)))


# --- _direction_sign ---------------------------------------------------


def test_direction_sign_from_numeric_nes() -> None:
    assert stage._direction_sign({"nes": 2.4}) == pytest.approx(2.4)
    assert stage._direction_sign({"NES": -2.21}) == pytest.approx(-2.21)
    assert stage._direction_sign({"normalized_enrichment_score": -1.1}) == pytest.approx(-1.1)


def test_direction_sign_from_string_direction() -> None:
    assert stage._direction_sign({"direction": "up"}) == 1.0
    assert stage._direction_sign({"direction": "Depleted"}) == -1.0
    assert stage._direction_sign({"regulation": "down"}) == -1.0


def test_direction_sign_absent_for_legacy_entry() -> None:
    assert stage._direction_sign({"label": "immune", "count": 12}) is None


# --- _overlap ------------------------------------------------------------


def test_overlap_legacy_count_only_has_no_direction(tmp_path: Path) -> None:
    ctx = make_context(
        tmp_path,
        manifest={"pathway_overlap": [{"label": "immune", "count": 12}]},
    )
    entries = stage._overlap(ctx)
    assert entries == [{"label": "immune", "count": 12.0, "direction": None}]


def test_overlap_parses_mixed_sign_nes(tmp_path: Path) -> None:
    manifest = {
        "pathway_overlap": [
            {"label": "enriched_set", "count": 40, "nes": 2.4},
            {"label": "depleted_set", "count": 18, "nes": -2.21},
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    by_label = {e["label"]: e for e in stage._overlap(ctx)}
    assert by_label["enriched_set"]["direction"] == pytest.approx(2.4)
    assert by_label["depleted_set"]["direction"] == pytest.approx(-2.21)


# --- pathway_overlap_bar --------------------------------------------------


def test_pathway_overlap_bar_legacy_input_still_renders(tmp_path: Path) -> None:
    """Backward compatibility: plain [{label, count}] (no direction
    field at all) must still render — the pre-existing contract."""
    manifest = {
        "pathway_overlap": [
            {"label": "immune", "count": 12},
            {"label": "metabolism", "count": 7},
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    out = tmp_path / "pathway_overlap_bar.png"
    result = stage.pathway_overlap_bar(ctx, out)
    assert result == out
    assert out.exists()


def test_pathway_overlap_bar_mixed_nes_renders_distinct_directions(tmp_path: Path) -> None:
    """RP-6: a mixed-sign-NES fixture must render the enriched and
    depleted pathways in visually distinct colors (the theme's
    sig_up/sig_down diverging pair), not identical bars."""
    manifest = {
        "pathway_overlap": [
            {"label": "enriched_set", "count": 40, "nes": 2.4},
            {"label": "depleted_set", "count": 18, "nes": -2.21},
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    out = tmp_path / "pathway_overlap_bar.png"
    result = stage.pathway_overlap_bar(ctx, out)
    assert result == out
    assert out.exists()

    img = np.asarray(Image.open(out).convert("RGB"))
    palette = THEME.get("palette", {})
    sig_up = _hex_to_rgb(palette.get("sig_up", "#D55E00"))
    sig_down = _hex_to_rgb(palette.get("sig_down", "#0072B2"))
    assert _has_color(img, sig_up), "enriched (direction > 0) color not found in rendered figure"
    assert _has_color(img, sig_down), "depleted (direction < 0) color not found in rendered figure"
    assert sig_up != sig_down


def test_pathway_overlap_bar_all_enriched_uses_single_up_color(tmp_path: Path) -> None:
    """A dataset with only enriched (positive) direction entries should
    not spuriously introduce the depleted color."""
    manifest = {
        "pathway_overlap": [
            {"label": "a", "count": 10, "nes": 1.5},
            {"label": "b", "count": 6, "nes": 2.0},
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    out = tmp_path / "pathway_overlap_bar.png"
    stage.pathway_overlap_bar(ctx, out)

    img = np.asarray(Image.open(out).convert("RGB"))
    palette = THEME.get("palette", {})
    sig_down = _hex_to_rgb(palette.get("sig_down", "#0072B2"))
    assert not _has_color(img, sig_down)
