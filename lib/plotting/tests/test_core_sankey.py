"""Test the sankey primitive."""

from __future__ import annotations

from pathlib import Path


from lib.plotting.core import sankey


def test_sankey_writes_png(tmp_path: Path) -> None:
    flows = [
        ("RNA_only", "paired", 800),
        ("RNA_only", "ATAC_orphan", 100),
        ("ATAC_only", "paired", 750),
        ("ATAC_only", "RNA_orphan", 50),
    ]
    out = tmp_path / "sankey.png"
    result = sankey(
        flows=flows,
        title="Barcode pairing",
        out=out,
    )
    assert result == out
    assert out.exists()
