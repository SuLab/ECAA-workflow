"""Tests for the reporting-stage figures, incl. the pathway_overlap_bar
NES/direction channel (RP-6: an enriched (NES>0) and a depleted
(NES<0) pathway used to render as visually identical bars) and the
overlap-magnitude basis (a deposited run supplied twenty entries at
`count: 1`, so every bar had the same height and the axis measured
nothing)."""

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
    entries, basis = stage._overlap(ctx)
    assert entries == [{"label": "immune", "count": 12.0, "direction": None}]
    assert basis == "reported_count"


def test_overlap_parses_mixed_sign_nes(tmp_path: Path) -> None:
    manifest = {
        "pathway_overlap": [
            {"label": "enriched_set", "count": 40, "nes": 2.4},
            {"label": "depleted_set", "count": 18, "nes": -2.21},
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    entries, _basis = stage._overlap(ctx)
    by_label = {e["label"]: e for e in entries}
    assert by_label["enriched_set"]["direction"] == pytest.approx(2.4)
    assert by_label["depleted_set"]["direction"] == pytest.approx(-2.21)


# --- overlap magnitude basis ---------------------------------------------


def test_overlap_intersects_members_with_significant_set(tmp_path: Path) -> None:
    """The real overlap measure: |members ∩ significant set|, not a row
    marker. A pipe-separated leading edge (fgsea's TSV shape) and a list
    both parse."""
    manifest = {
        "pathway_overlap": [
            {"label": "A", "leadingEdge": "G1|G2|G3|G4", "nes": 2.0},
            {"label": "B", "leading_edge": ["G3", "G9", "G10"], "nes": -1.7},
        ],
        "significant_genes": ["G1", "G2", "G3", "G9"],
    }
    entries, basis = stage._overlap(make_context(tmp_path, manifest=manifest))
    assert basis == "leading_edge_intersect_significant"
    assert {e["label"]: e["count"] for e in entries} == {"A": 3.0, "B": 2.0}


def test_overlap_falls_back_to_member_set_size(tmp_path: Path) -> None:
    """Member sets but no significant set to intersect against: the bar is
    the member-set size, and the basis says so."""
    manifest = {
        "pathway_overlap": [
            {"label": "A", "members": "G1|G2|G3"},
            {"label": "B", "members": ["G4"]},
        ]
    }
    entries, basis = stage._overlap(make_context(tmp_path, manifest=manifest))
    assert basis == "member_set_size"
    assert {e["label"]: e["count"] for e in entries} == {"A": 3.0, "B": 1.0}


def test_overlap_basis_is_uniform_when_members_are_partial(tmp_path: Path) -> None:
    """One entry without a member set forces the whole figure onto the
    reported-count basis — mixing bases would put incomparable units on a
    shared axis."""
    manifest = {
        "pathway_overlap": [
            {"label": "A", "members": ["G1", "G2"], "n_overlap": 9},
            {"label": "B", "n_overlap": 4},
        ]
    }
    entries, basis = stage._overlap(make_context(tmp_path, manifest=manifest))
    assert basis == "reported_count"
    assert {e["label"]: e["count"] for e in entries} == {"A": 9.0, "B": 4.0}


def test_degenerate_detects_constant_magnitudes() -> None:
    assert stage._degenerate([1.0, 1.0, 1.0])
    assert not stage._degenerate([1.0, 2.0])
    # A single bar has nothing to be degenerate against.
    assert not stage._degenerate([1.0])
    assert not stage._degenerate([])


def test_pathway_overlap_bar_drops_constant_count_input(tmp_path: Path) -> None:
    """The deposited defect: twenty entries all at `count: 1`. The bar
    height measures nothing, so the figure is skipped (FileNotFoundError
    is the channel `core.generate()` records as a skip) rather than
    shipped as a chart of identical bars."""
    manifest = {
        "pathway_overlap": [
            {"label": f"TERM_{i}", "count": 1, "nes": 1.5 if i % 2 else -1.5}
            for i in range(20)
        ]
    }
    ctx = make_context(tmp_path, manifest=manifest)
    with pytest.raises(FileNotFoundError, match="degenerate"):
        stage.pathway_overlap_bar(ctx, tmp_path / "pathway_overlap_bar.png")
    assert not (tmp_path / "pathway_overlap_bar.png").exists()


def test_pathway_overlap_bar_axis_names_the_resolved_basis(tmp_path: Path) -> None:
    """Three different measures must never share an unqualified 'count'
    axis — the label is part of the honesty contract."""
    assert set(stage.BASIS_AXIS_LABEL) == {
        "leading_edge_intersect_significant",
        "member_set_size",
        "reported_count",
    }
    assert len(set(stage.BASIS_AXIS_LABEL.values())) == 3


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
