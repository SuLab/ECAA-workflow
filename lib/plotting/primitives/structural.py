"""Universal structural primitives — operate on physical shape alone.

Every primitive honors lib/plotting/theme.json (Wong/Glasbey palette,
pdf.fonttype 42, 300 dpi, provenance footer). Outputs PNG + vector PDF
byte-deterministically given the same matplotlib + freetype combo.

These five primitives cover every PhysicalShape the affordance selector
can emit as StructuralFallback:

  matrix_overview    — PhysicalShape::Numeric2D
  distribution       — PhysicalShape::Numeric1D
  categorical_summary — PhysicalShape::Categorical1D
  pairs              — PhysicalShape::TabularNumeric { columns: 2..=8 }
  scalar_card        — PhysicalShape::Scalar

Provenance kind strings match GenericPrimitive::figure_id() in
crates/core/src/plot_affordance/primitive.rs so the StructuralFallback
path resolves end-to-end without any additional wiring.

NOTE on _core.figure() / _core.savefig() discipline:
lib/plotting/core.py exposes savefig(fig, path, *, dpi, formats,
stage_id, ...) but has no figure() constructor.  Following the
established pattern of every stage module (e.g. volcano, heatmap,
bar), we call plt.subplots() directly and thread the resulting fig
through _core.savefig().  The stage_id parameter of savefig drives the
provenance footer, so we pass the primitive id string there.  Dual
PNG + PDF output is achieved by passing formats=["png", "pdf"] and
the PNG path as the primary; savefig writes the PDF alongside it with
the same stem (path.with_suffix(".pdf")).
"""

from __future__ import annotations

from collections import Counter
from pathlib import Path
from typing import Any, Sequence

import matplotlib.pyplot as plt
import numpy as np

# Import the module-level object so we get the live THEME and all
# helpers without binding to a specific attribute at import time.
# runtime.plotting is available when executed inside an emitted package;
# fall back to lib.plotting when run from the repo root (e.g. pytest).
try:
    from runtime.plotting import core as _core
except ImportError:
    from lib.plotting import core as _core  # type: ignore[no-redef]


def matrix_overview(
    matrix: np.ndarray,
    *,
    png_path: Path,
    pdf_path: Path,
    title: str,
    theme_path: str,
) -> None:
    """Heatmap of any 2D numeric array.

    Rasterizes the imshow layer when matrix.size > 50_000 to keep the
    PDF file size manageable while preserving a vector frame and labels.
    The colorbar, title, and axis labels remain vector in both cases.

    Args:
        matrix:     2D numpy array of numeric values.
        png_path:   Destination path for the PNG output (300 dpi).
        pdf_path:   Destination path for the PDF output (vector).
        title:      Figure title shown above the heatmap.
        theme_path: Theme identifier forwarded for future preset
                    resolution; ignored by the current implementation
                    which reads the module-level THEME directly.
    """
    if np.asarray(matrix).ndim != 2:
        raise ValueError(
            f"matrix_overview expects a 2D input array; got shape {np.asarray(matrix).shape}"
        )
    mat = np.asarray(matrix, dtype=float)
    rasterize = mat.size > 50_000

    fig, ax = plt.subplots(figsize=(8.0, 6.0))
    im = ax.imshow(mat, aspect="auto", rasterized=rasterize)
    fig.colorbar(im, ax=ax)
    ax.set_title(title)
    ax.set_xlabel("column")
    ax.set_ylabel("row")

    # Savefig writes PNG + PDF via formats=["png","pdf"]. The primary
    # path carries the PNG suffix; the companion PDF is written alongside
    # With the same stem. stage_id drives the provenance footer.
    _core.savefig(
        fig,
        Path(png_path),
        stage_id="__structural_matrix_overview",
        formats=["png", "pdf"],
    )


def distribution(
    values: np.ndarray,
    *,
    png_path: Path,
    pdf_path: Path,
    title: str,
    theme_path: str,
    bins: int = 50,
) -> None:
    """Histogram of any 1D numeric array with an optional KDE overlay.

    The KDE overlay is added only when n >= 25 — below that threshold
    the kernel estimate is too unstable to be informative.

    Args:
        values:     1D (or ravelled) numeric array.
        png_path:   Destination path for the PNG output.
        pdf_path:   Destination path for the PDF output.
        title:      Figure title.
        theme_path: Theme identifier (forwarded; not used directly).
        bins:       Number of histogram bins (default 50).
    """
    arr = np.asarray(values, dtype=float).ravel()
    if arr.size == 0:
        raise ValueError("distribution requires a non-empty input array")

    fig, ax = plt.subplots(figsize=(8.0, 5.5))
    ax.hist(arr, bins=bins, density=True, alpha=0.6)

    if arr.size >= 25:
        from scipy.stats import gaussian_kde  # lazy import — optional dep path

        kde = gaussian_kde(arr)
        xs = np.linspace(float(arr.min()), float(arr.max()), 200)
        ax.plot(xs, kde(xs))

    ax.set_title(title)
    ax.set_xlabel("value")
    ax.set_ylabel("density")

    _core.savefig(
        fig,
        Path(png_path),
        stage_id="__structural_distribution",
        formats=["png", "pdf"],
    )


def categorical_summary(
    labels: Sequence[Any],
    *,
    png_path: Path,
    pdf_path: Path,
    title: str,
    theme_path: str,
) -> None:
    """Bar plot showing per-category counts of a 1D categorical array.

    Categories are sorted by descending count; ties are broken by
    ascending string representation so the order is fully deterministic
    regardless of insertion order.

    Args:
        labels:     Sequence of hashable category labels.
        png_path:   Destination path for the PNG output.
        pdf_path:   Destination path for the PDF output.
        title:      Figure title.
        theme_path: Theme identifier (forwarded; not used directly).
    """
    counts = Counter(labels)
    if not counts:
        raise ValueError("categorical_summary requires at least one label")

    # Sort: descending count, then ascending string label as tie-breaker.
    sorted_items = sorted(counts.items(), key=lambda kv: (-kv[1], str(kv[0])))
    keys = [str(kv[0]) for kv in sorted_items]
    vals = [kv[1] for kv in sorted_items]

    n_cats = len(keys)
    fig_width = max(6.0, min(16.0, n_cats * 0.6 + 2.0))
    fig, ax = plt.subplots(figsize=(fig_width, 5.5))

    palette = _core.categorical_palette(n_cats, name="categorical_summary")
    ax.bar(range(n_cats), vals, color=palette)
    ax.set_xticks(range(n_cats))
    ax.set_xticklabels(keys, rotation=45, ha="right", fontsize=7)
    ax.set_title(title)
    ax.set_ylabel("count")

    _core.savefig(
        fig,
        Path(png_path),
        stage_id="__structural_categorical_summary",
        formats=["png", "pdf"],
    )


def pairs(
    table: np.ndarray,
    column_names: Sequence[str],
    *,
    png_path: Path,
    pdf_path: Path,
    title: str,
    theme_path: str,
) -> None:
    """Small-multiples scatter matrix for 2D tabular numeric input.

    Diagonal cells show per-column histograms; off-diagonal cells show
    pairwise scatter plots.  Scatter points are rasterized to keep PDF
    file size bounded.

    The column count is validated to be <= 8.  At higher cardinality
    the individual panels become too small to be legible and the caller
    should aggregate columns before plotting.

    Args:
        table:        2D array of shape (n_rows, n_cols) with n_cols <= 8.
        column_names: Sequence of n_cols human-readable column labels.
        png_path:     Destination path for the PNG output.
        pdf_path:     Destination path for the PDF output.
        title:        Figure suptitle.
        theme_path:   Theme identifier (forwarded; not used directly).
    """
    arr = np.asarray(table, dtype=float)
    if arr.ndim != 2:
        raise ValueError(
            f"pairs expects a 2D input array; got ndim={arr.ndim}"
        )
    n_cols = arr.shape[1]
    if n_cols > 8:
        raise ValueError(
            f"pairs accepts at most 8 columns; got {n_cols}. "
            "Reduce the number of columns before calling pairs()."
        )
    if len(column_names) != n_cols:
        raise ValueError(
            f"column_names length ({len(column_names)}) must match "
            f"table column count ({n_cols})"
        )

    cell_size = 2.0
    fig_size = cell_size * n_cols
    fig, axes = plt.subplots(n_cols, n_cols, figsize=(fig_size, fig_size))

    # Normalise axes to always be a 2D array for uniform indexing.
    if n_cols == 1:
        axes = np.array([[axes]])
    else:
        axes = np.asarray(axes)

    scatter_color = _core.categorical_palette(1, name="pairs")[0]

    for i in range(n_cols):
        for j in range(n_cols):
            ax = axes[i, j]
            if i == j:
                ax.hist(arr[:, i], bins=30, color=scatter_color, alpha=0.7)
            else:
                ax.scatter(
                    arr[:, j],
                    arr[:, i],
                    s=4,
                    alpha=0.5,
                    color=scatter_color,
                    rasterized=True,
                )
            # Only show tick labels on the outer edges to reduce clutter.
            if i < n_cols - 1:
                ax.set_xticklabels([])
            else:
                ax.set_xlabel(column_names[j], fontsize=7)
            if j > 0:
                ax.set_yticklabels([])
            else:
                ax.set_ylabel(column_names[i], fontsize=7)

    fig.suptitle(title, y=1.01)
    fig.tight_layout()

    _core.savefig(
        fig,
        Path(png_path),
        stage_id="__structural_pairs",
        formats=["png", "pdf"],
    )


def scalar_card(
    value: float,
    label: str,
    *,
    png_path: Path,
    pdf_path: Path,
    title: str,
    theme_path: str,
) -> None:
    """Plain-text display card for a single scalar metric.

    Renders the numeric value in large type with a descriptive label
    below it, suitable for surfacing aggregate statistics (e.g. AUROC,
    R-squared, p-value) in the result-review pane.

    Args:
        value:      The scalar value to display.
        label:      Short descriptive label for the value.
        png_path:   Destination path for the PNG output.
        pdf_path:   Destination path for the PDF output.
        title:      Figure title shown above the card.
        theme_path: Theme identifier (forwarded; not used directly).
    """
    fig, ax = plt.subplots(figsize=(4.0, 2.5))
    ax.axis("off")
    ax.text(
        0.5,
        0.60,
        f"{value:.4g}",
        ha="center",
        va="center",
        fontsize=36,
        transform=ax.transAxes,
    )
    ax.text(
        0.5,
        0.25,
        label,
        ha="center",
        va="center",
        fontsize=14,
        transform=ax.transAxes,
    )
    ax.set_title(title)

    _core.savefig(
        fig,
        Path(png_path),
        stage_id="__structural_scalar_card",
        formats=["png", "pdf"],
    )
