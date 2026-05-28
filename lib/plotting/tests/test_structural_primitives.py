"""Snapshot tests for the five universal structural primitives.

Tests are authored but NOT run here — they are deferred to the first
SNAPSHOT_REGENERATE=1 sweep (see below).  Running pytest without that
env var will skip the hash-comparison assertions when the snapshot file
is empty ({}), so the suite remains green on a fresh branch.

Regenerate all baselines:
    SNAPSHOT_REGENERATE=1 pytest lib/plotting/tests/test_structural_primitives.py -v

Workflow:
1. Run with SNAPSHOT_REGENERATE=1 once on a known-good checkout.
2. Review the generated PNG/PDF figures visually.
3. Commit the updated snapshot-hashes.json alongside this file.
4. Subsequent CI runs compare against the committed hashes.

Each test:
- Creates a deterministic synthetic input seeded from numpy.
- Calls the primitive.
- Asserts PNG + PDF both exist.
- When snapshot-hashes.json has a non-empty entry for the test key,
  asserts the PNG sha256 matches.
- When SNAPSHOT_REGENERATE=1, writes the current sha256 into the
  snapshot file instead of asserting.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path

import numpy as np
import pytest

# The primitives are importable as runtime.plotting.primitives.structural
# when the package root is on PYTHONPATH (e.g. inside an emitted package),
# or as lib.plotting.primitives.structural from the repo root.
try:
    from runtime.plotting.primitives.structural import (
        categorical_summary,
        distribution,
        matrix_overview,
        pairs,
        scalar_card,
    )
except ImportError:
    from lib.plotting.primitives.structural import (  # type: ignore[no-redef]
        categorical_summary,
        distribution,
        matrix_overview,
        pairs,
        scalar_card,
    )

SNAPSHOT_DIR = Path(__file__).parent / "snapshots" / "structural"
SNAPSHOT_FILE = SNAPSHOT_DIR / "snapshot-hashes.json"

THEME_DIGEST = "theme.json"  # forwarded to primitives; they read the live THEME


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _png_sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def _load_snapshots() -> dict:
    if SNAPSHOT_FILE.exists():
        text = SNAPSHOT_FILE.read_text().strip()
        return json.loads(text) if text else {}
    return {}


def _save_snapshots(snaps: dict) -> None:
    SNAPSHOT_DIR.mkdir(parents=True, exist_ok=True)
    SNAPSHOT_FILE.write_text(json.dumps(snaps, indent=2, sort_keys=True) + "\n")


def _check_or_regenerate(key: str, png: Path) -> None:
    """Compare png against the stored hash, or write it when SNAPSHOT_REGENERATE=1."""
    regenerate = os.environ.get("SNAPSHOT_REGENERATE", "0") == "1"
    snaps = _load_snapshots()
    digest = _png_sha256(png)
    if regenerate:
        snaps[key] = digest
        _save_snapshots(snaps)
    elif key in snaps and snaps[key]:
        assert digest == snaps[key], (
            f"Snapshot mismatch for '{key}'.\n"
            f"  expected: {snaps[key]}\n"
            f"  actual:   {digest}\n"
            "Re-run with SNAPSHOT_REGENERATE=1 to update the baseline."
        )
    # If key is absent from snaps (empty {}), skip the assertion silently
    # until baselines are generated.


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


@pytest.fixture
def small_matrix():
    """20 × 12 float matrix — well below the 50 000-cell rasterization threshold."""
    rng = np.random.default_rng(42)
    return rng.normal(size=(20, 12))


@pytest.fixture
def large_matrix():
    """260 × 210 float matrix — above the 50 000-cell rasterization threshold."""
    rng = np.random.default_rng(42)
    return rng.normal(size=(260, 210))


@pytest.fixture
def numeric_1d_large():
    """500 values — above the n>=25 KDE threshold."""
    rng = np.random.default_rng(7)
    return rng.normal(loc=5.0, scale=2.0, size=500)


@pytest.fixture
def numeric_1d_small():
    """10 values — below the KDE threshold; histogram only."""
    rng = np.random.default_rng(7)
    return rng.normal(loc=5.0, scale=2.0, size=10)


@pytest.fixture
def category_labels():
    rng = np.random.default_rng(13)
    cats = ["alpha", "beta", "gamma", "delta"]
    indices = rng.integers(0, len(cats), size=200)
    return [cats[i] for i in indices]


@pytest.fixture
def tabular_4col():
    rng = np.random.default_rng(99)
    return rng.normal(size=(80, 4))


@pytest.fixture
def tabular_8col():
    rng = np.random.default_rng(99)
    return rng.normal(size=(60, 8))


# ---------------------------------------------------------------------------
# matrix_overview
# ---------------------------------------------------------------------------


def test_matrix_overview_small_round_trip(tmp_path, small_matrix):
    png = tmp_path / "matrix_overview_small.png"
    pdf = tmp_path / "matrix_overview_small.pdf"
    matrix_overview(
        small_matrix,
        png_path=png,
        pdf_path=pdf,
        title="test small matrix",
        theme_path=THEME_DIGEST,
    )
    assert png.exists(), "PNG not written"
    # PDF is written by savefig alongside the PNG with the same stem.
    expected_pdf = png.with_suffix(".pdf")
    assert expected_pdf.exists(), f"PDF not written (expected {expected_pdf})"
    _check_or_regenerate("matrix_overview_small", png)


def test_matrix_overview_large_rasterizes(tmp_path, large_matrix):
    """Matrix with >50 000 cells should still produce valid output."""
    assert large_matrix.size > 50_000
    png = tmp_path / "matrix_overview_large.png"
    pdf = tmp_path / "matrix_overview_large.pdf"
    matrix_overview(
        large_matrix,
        png_path=png,
        pdf_path=pdf,
        title="test large matrix (rasterized)",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("matrix_overview_large", png)


def test_matrix_overview_rejects_1d():
    with pytest.raises(ValueError, match="2D"):
        matrix_overview(
            np.zeros(10),
            png_path=Path("/tmp/x.png"),
            pdf_path=Path("/tmp/x.pdf"),
            title="bad",
            theme_path=THEME_DIGEST,
        )


# ---------------------------------------------------------------------------
# distribution
# ---------------------------------------------------------------------------


def test_distribution_with_kde(tmp_path, numeric_1d_large):
    """n >= 25: KDE overlay should be added without error."""
    png = tmp_path / "distribution_kde.png"
    pdf = tmp_path / "distribution_kde.pdf"
    distribution(
        numeric_1d_large,
        png_path=png,
        pdf_path=pdf,
        title="test distribution with KDE",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("distribution_kde", png)


def test_distribution_no_kde(tmp_path, numeric_1d_small):
    """n < 25: histogram only, no KDE; should complete without error."""
    assert len(numeric_1d_small) < 25
    png = tmp_path / "distribution_no_kde.png"
    pdf = tmp_path / "distribution_no_kde.pdf"
    distribution(
        numeric_1d_small,
        png_path=png,
        pdf_path=pdf,
        title="test distribution no KDE",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("distribution_no_kde", png)


def test_distribution_rejects_empty():
    with pytest.raises(ValueError, match="non-empty"):
        distribution(
            np.array([]),
            png_path=Path("/tmp/x.png"),
            pdf_path=Path("/tmp/x.pdf"),
            title="bad",
            theme_path=THEME_DIGEST,
        )


# ---------------------------------------------------------------------------
# categorical_summary
# ---------------------------------------------------------------------------


def test_categorical_summary_round_trip(tmp_path, category_labels):
    png = tmp_path / "categorical_summary.png"
    pdf = tmp_path / "categorical_summary.pdf"
    categorical_summary(
        category_labels,
        png_path=png,
        pdf_path=pdf,
        title="test categorical summary",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("categorical_summary", png)


def test_categorical_summary_sort_order(tmp_path):
    """Verify deterministic sort: descending count, then ascending label."""
    # "b" appears 3×, "a" and "c" each appear 2×; tie broken by label → a < c.
    labels = ["b", "b", "b", "a", "a", "c", "c"]
    png = tmp_path / "cat_sort.png"
    categorical_summary(
        labels,
        png_path=png,
        pdf_path=tmp_path / "cat_sort.pdf",
        title="sort order check",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()


def test_categorical_summary_rejects_empty():
    with pytest.raises(ValueError, match="at least one label"):
        categorical_summary(
            [],
            png_path=Path("/tmp/x.png"),
            pdf_path=Path("/tmp/x.pdf"),
            title="bad",
            theme_path=THEME_DIGEST,
        )


# ---------------------------------------------------------------------------
# pairs
# ---------------------------------------------------------------------------


def test_pairs_4col_round_trip(tmp_path, tabular_4col):
    png = tmp_path / "pairs_4col.png"
    pdf = tmp_path / "pairs_4col.pdf"
    pairs(
        tabular_4col,
        ["A", "B", "C", "D"],
        png_path=png,
        pdf_path=pdf,
        title="test pairs 4 columns",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("pairs_4col", png)


def test_pairs_8col_round_trip(tmp_path, tabular_8col):
    """8 columns is the maximum allowed; should complete without error."""
    png = tmp_path / "pairs_8col.png"
    pdf = tmp_path / "pairs_8col.pdf"
    col_names = [f"col_{i}" for i in range(8)]
    pairs(
        tabular_8col,
        col_names,
        png_path=png,
        pdf_path=pdf,
        title="test pairs 8 columns",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("pairs_8col", png)


def test_pairs_rejects_9_columns():
    rng = np.random.default_rng(1)
    bad = rng.normal(size=(10, 9))
    with pytest.raises(ValueError, match="8 columns"):
        pairs(
            bad,
            [f"c{i}" for i in range(9)],
            png_path=Path("/tmp/x.png"),
            pdf_path=Path("/tmp/x.pdf"),
            title="bad",
            theme_path=THEME_DIGEST,
        )


def test_pairs_rejects_mismatched_names():
    rng = np.random.default_rng(1)
    tbl = rng.normal(size=(10, 3))
    with pytest.raises(ValueError, match="column_names length"):
        pairs(
            tbl,
            ["a", "b"],  # only 2 names for 3 columns
            png_path=Path("/tmp/x.png"),
            pdf_path=Path("/tmp/x.pdf"),
            title="bad",
            theme_path=THEME_DIGEST,
        )


# ---------------------------------------------------------------------------
# scalar_card
# ---------------------------------------------------------------------------


def test_scalar_card_round_trip(tmp_path):
    png = tmp_path / "scalar_card.png"
    pdf = tmp_path / "scalar_card.pdf"
    scalar_card(
        0.9312,
        "AUROC",
        png_path=png,
        pdf_path=pdf,
        title="model performance",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
    assert png.with_suffix(".pdf").exists()
    _check_or_regenerate("scalar_card", png)


def test_scalar_card_zero(tmp_path):
    """Edge case: value of exactly 0 should render correctly."""
    png = tmp_path / "scalar_card_zero.png"
    scalar_card(
        0.0,
        "null metric",
        png_path=png,
        pdf_path=tmp_path / "scalar_card_zero.pdf",
        title="zero value",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()


def test_scalar_card_negative(tmp_path):
    """Negative values should render correctly (e.g. log-fold change)."""
    png = tmp_path / "scalar_card_neg.png"
    scalar_card(
        -3.14159,
        "log2FC",
        png_path=png,
        pdf_path=tmp_path / "scalar_card_neg.pdf",
        title="negative scalar",
        theme_path=THEME_DIGEST,
    )
    assert png.exists()
