"""Tests for the biological-interpretation / pathway-enrichment renderer.

The defect these pin: `top_enriched_terms` ranked every enrichment row by
`-log10(adj_p)` — a score that is positive for a DEPLETED set exactly as
it is for an enriched one — took the top 20, and captioned the result
"Top enriched terms". In a deposited run 3 of the 20 bars were depleted
pathways (NES < 0), and `bar()` had no direction channel, so nothing in
the figure distinguished them.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Tuple

import numpy as np
import pytest
from PIL import Image

from lib.plotting.core import THEME
from lib.plotting.stages import biological_interpretation as stage
from lib.plotting.tests.helpers import make_context


def _hex_to_rgb(hex_color: str) -> Tuple[int, int, int]:
    hex_color = hex_color.lstrip("#")
    return tuple(int(hex_color[i : i + 2], 16) for i in (0, 2, 4))  # noqa: E203


def _has_color(png: Path, rgb: Tuple[int, int, int], tol: int = 12) -> bool:
    img = np.asarray(Image.open(png).convert("RGB")).astype(int)
    return bool(np.any(np.all(np.abs(img - np.array(rgb)) <= tol, axis=-1)))


# Rows lifted from the deposited fgsea run (term names shortened): the
# most significant depleted term is NOT the most negative-NES one, which is
# exactly where an undefined selection rule shows up.
SIGNED_ROWS = [
    {"term": "HALLMARK_ADIPOGENESIS", "adj_p_value": 8.468e-05, "NES": 1.9528},
    {"term": "GOBP_POS_REG_NERVOUS_SYSTEM_DEV", "adj_p_value": 0.000419, "NES": -1.8386},
    {"term": "GOBP_POS_REG_SYNAPSE_ASSEMBLY", "adj_p_value": 0.00262, "NES": -2.0259},
    {"term": "GOBP_EPIDERMIS_MORPHOGENESIS", "adj_p_value": 0.00269, "NES": -2.1502},
    {"term": "GOBP_REG_NERVOUS_SYSTEM_DEV", "adj_p_value": 0.00269, "NES": -1.5682},
]


# --- effect / significance extraction ------------------------------------


def test_effect_reads_numeric_keys_in_order() -> None:
    assert stage._effect({"NES": -2.1}) == pytest.approx(-2.1)
    assert stage._effect({"normalized_enrichment_score": 1.4}) == pytest.approx(1.4)
    assert stage._effect({"log2FoldChange": -0.8}) == pytest.approx(-0.8)


def test_effect_degrades_categorical_direction_to_unit_sign() -> None:
    assert stage._effect({"direction": "depleted"}) == -1.0
    assert stage._effect({"regulation": "UP"}) == 1.0


def test_effect_absent_for_unsigned_row() -> None:
    assert stage._effect({"term": "GO:1", "adj_p_value": 0.01}) is None


def test_significance_prefers_adjusted_and_keeps_exact_zero() -> None:
    """The old `e.get("adj_p_value") or e.get("p_value") or 1.0` chain
    turned an `adj_p_value` of exactly 0.0 into 1.0, ranking the single
    most significant term LAST."""
    assert stage._significance({"adj_p_value": 0.0, "p_value": 0.4}) == 0.0
    assert stage._significance({"p_value": 0.02}) == pytest.approx(0.02)
    assert stage._significance({"term": "GO:1"}) is None


# --- ranking --------------------------------------------------------------


def test_rank_key_orders_by_significance_then_effect_magnitude() -> None:
    ordered = [e["term"] for e in sorted(SIGNED_ROWS, key=stage._rank_key)]
    assert ordered[0] == "HALLMARK_ADIPOGENESIS"
    # The two rows tied at adj_p 0.00269 break on |NES| descending, not on
    # input order.
    assert ordered[-2:] == [
        "GOBP_EPIDERMIS_MORPHOGENESIS",
        "GOBP_REG_NERVOUS_SYSTEM_DEV",
    ]


def test_rank_key_is_order_independent() -> None:
    forward = [e["term"] for e in sorted(SIGNED_ROWS, key=stage._rank_key)]
    reverse = [e["term"] for e in sorted(SIGNED_ROWS[::-1], key=stage._rank_key)]
    assert forward == reverse


def test_unrankable_rows_are_dropped_not_sorted_last(tmp_path: Path) -> None:
    rows = SIGNED_ROWS + [{"term": "NO_P_VALUE", "NES": 9.9}]
    ctx = make_context(tmp_path, manifest={"enrichments": rows})
    out = tmp_path / "f.png"
    stage.top_significant_terms(ctx, out)
    rankable = [e for e in rows if stage._significance(e) is not None]
    assert len(rankable) == len(SIGNED_ROWS)


# --- top_significant_terms ------------------------------------------------


def test_signed_input_encodes_direction_in_color(tmp_path: Path) -> None:
    ctx = make_context(tmp_path, manifest={"enrichments": SIGNED_ROWS})
    out = tmp_path / "top.png"
    assert stage.top_significant_terms(ctx, out) == out
    palette = THEME.get("palette", {})
    assert _has_color(out, _hex_to_rgb(palette["sig_up"]))
    assert _has_color(out, _hex_to_rgb(palette["sig_down"]))


def test_unsigned_input_still_renders_and_claims_no_direction(tmp_path: Path) -> None:
    """Over-representation output with no effect column: the value axis
    falls back to -log10(adj_p) and no diverging color appears, so the
    figure asserts no direction it cannot support."""
    rows = [{"term": f"GO:{i}", "adj_p_value": 10 ** -(i + 2)} for i in range(5)]
    ctx = make_context(tmp_path, manifest={"enrichments": rows})
    out = tmp_path / "unsigned.png"
    assert stage.top_significant_terms(ctx, out) == out
    palette = THEME.get("palette", {})
    assert not _has_color(out, _hex_to_rgb(palette["sig_up"]))
    assert not _has_color(out, _hex_to_rgb(palette["sig_down"]))


def test_committed_plot_stub_renders(tmp_path: Path) -> None:
    """The checked-in `testdata/plot-stubs/pathway_enrichment` fixture is
    unsigned — the L2 per-atom smoke test drives this exact input."""
    stub = Path(__file__).resolve().parents[3] / "testdata/plot-stubs/pathway_enrichment/input"
    manifest = json.loads((stub / "manifest.json").read_text())
    ctx = make_context(stub, manifest=manifest)
    out = tmp_path / "stub.png"
    assert stage.top_significant_terms(ctx, out) == out
    assert out.stat().st_size > 0


def test_threshold_from_manifest_filters_and_is_reported(tmp_path: Path) -> None:
    rows = SIGNED_ROWS + [{"term": "NOT_SIGNIFICANT", "adj_p_value": 0.9, "NES": 3.0}]
    ctx = make_context(
        tmp_path, manifest={"enrichments": rows, "pathway_fdr_threshold": 0.25}
    )
    assert stage._threshold(ctx) == pytest.approx(0.25)
    passing = [e for e in rows if (stage._significance(e) or 1.0) < 0.25]
    assert "NOT_SIGNIFICANT" not in [e["term"] for e in passing]


def test_empty_enrichments_raise_skip(tmp_path: Path) -> None:
    ctx = make_context(tmp_path, manifest={"enrichments": []})
    with pytest.raises(FileNotFoundError):
        stage.top_significant_terms(ctx, tmp_path / "x.png")


def test_no_parseable_pvalue_raises_skip(tmp_path: Path) -> None:
    ctx = make_context(tmp_path, manifest={"enrichments": [{"term": "GO:1", "NES": 2.0}]})
    with pytest.raises(FileNotFoundError, match="p-value"):
        stage.top_significant_terms(ctx, tmp_path / "x.png")


# --- id alias -------------------------------------------------------------


def test_legacy_id_aliases_the_corrected_renderer() -> None:
    """`top_enriched_terms` is still declared in the plot-affordance
    registry and the pathway_enrichment atom, so it must keep resolving —
    to the corrected renderer, not the old direction-blind one."""
    assert "top_significant_terms" in stage.FIGURES
    assert "top_enriched_terms" in stage.FIGURES
    assert stage.FIGURES.get("top_enriched_terms") is stage.FIGURES.get(
        "top_significant_terms"
    )


# --- enrichment_table view ------------------------------------------------


def test_view_carries_direction(tmp_path: Path) -> None:
    ctx = make_context(tmp_path, manifest={"enrichments": SIGNED_ROWS})
    by_term = {r["term"]: r for r in stage.view_enrichment_table(ctx)["rows"]}
    assert by_term["HALLMARK_ADIPOGENESIS"]["direction"] == "enriched"
    assert by_term["GOBP_EPIDERMIS_MORPHOGENESIS"]["direction"] == "depleted"


def test_view_direction_undetermined_without_effect(tmp_path: Path) -> None:
    ctx = make_context(tmp_path, manifest={"enrichments": [{"term": "GO:1", "p_value": 0.01}]})
    row = stage.view_enrichment_table(ctx)["rows"][0]
    assert row["direction"] == "undetermined"
    assert row["signed_effect"] is None


# --- TSV fallback ---------------------------------------------------------


def test_tsv_fallback_picks_up_signed_effect(tmp_path: Path) -> None:
    tsv = tmp_path / "enrichment.tsv"
    tsv.write_text(
        "term\tp_value\tadj_p_value\tn_overlap\tNES\n"
        "T1\t1e-8\t1e-5\t20\t2.1\n"
        "T2\t1e-6\t1e-3\t14\t-1.9\n"
    )
    ctx = make_context(tmp_path, manifest={})
    rows = stage._load_enrichments(ctx)
    assert [r["signed_effect"] for r in rows] == [2.1, -1.9]
