"""Test the arc-diagram primitive."""

from __future__ import annotations

from pathlib import Path

import numpy as np

from lib.plotting.core import arc


def test_arc_writes_png(tmp_path: Path) -> None:
    starts = np.array([100, 200, 350, 500])
    ends = np.array([180, 290, 470, 620])
    weights = np.array([0.9, 0.3, 0.7, 0.5])
    out = tmp_path / "arc.png"
    result = arc(
        starts=starts,
        ends=ends,
        weights=weights,
        title="Test arcs",
        xlabel="Position (bp)",
        out=out,
    )
    assert result == out
    assert out.exists()
    assert out.stat().st_size > 0


def test_arc_empty_input_raises(tmp_path: Path) -> None:
    import pytest

    with pytest.raises(ValueError, match="at least one"):
        arc(
            starts=np.array([]),
            ends=np.array([]),
            weights=np.array([]),
            title="empty",
            xlabel="x",
            out=tmp_path / "empty.png",
        )
