"""Shared helpers for stage modules — TSV/JSON manifest readers.

Stage 12-14 introduces ~16 new stage modules whose data-loading logic
is identical: read a path from the manifest, open the TSV (gzip-aware),
return columns as numpy arrays. Centralising the boilerplate keeps each
stage module thin (<100 LOC) and tightens the contract that bad inputs
fall through to a typed FileNotFoundError that `core.generate()`
records on the per-figure `errors` map.
"""

from __future__ import annotations

import gzip
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

import numpy as np


def open_text(path: Path):
    """Open a TSV path with gzip awareness."""
    if str(path).endswith(".gz"):
        return gzip.open(path, "rt")
    return open(path, "rt")


def find_col(header: List[str], candidates: Tuple[str, ...]) -> Optional[int]:
    lower = [h.lower() for h in header]
    for cand in candidates:
        try:
            return lower.index(cand.lower())
        except ValueError:
            continue
    return None


def manifest_path(manifest: Dict[str, Any], outputs_dir: Path, *keys: str) -> Optional[Path]:
    """Walk `manifest[keys[0]][keys[1]]...` and resolve the leaf string
    against `outputs_dir`. Returns None if any segment is missing or
    not a string. Used to keep stage-module logic free of nested
    .get(...).get(...) dance.
    """
    cur: Any = manifest
    for k in keys:
        if not isinstance(cur, dict):
            return None
        cur = cur.get(k)
    if not isinstance(cur, str):
        return None
    p = (outputs_dir / cur).resolve()
    return p if p.exists() else None


def load_tsv_columns(
    path: Path, columns: Dict[str, Tuple[str, ...]]
) -> Optional[Dict[str, np.ndarray]]:
    """Load a TSV file, returning the requested columns by candidate
    header names. `columns` maps the canonical name (used by the
    primitive) to a tuple of acceptable header aliases.

    Returns None if the file can't be opened or any required column is
    missing. Required columns are those whose canonical name does not
    end in `?` (which marks an optional column).

    Example:
        load_tsv_columns(path, {
            "chrom": ("chrom", "chromosome", "#CHROM"),
            "pos": ("pos", "position", "POS"),
            "pvalue": ("pvalue", "P", "p_value"),
            "gene?": ("gene", "Gene", "symbol"),
        })
    """
    try:
        with open_text(path) as f:
            header = f.readline().rstrip("\n").split("\t")
            indices: Dict[str, Optional[int]] = {}
            for canonical, aliases in columns.items():
                indices[canonical] = find_col(header, aliases)
            for canonical, idx in indices.items():
                if not canonical.endswith("?") and idx is None:
                    return None
            data: Dict[str, List[str]] = {k: [] for k in columns}
            for line in f:
                parts = line.rstrip("\n").split("\t")
                for canonical, idx in indices.items():
                    if idx is None:
                        continue
                    val = parts[idx] if idx < len(parts) else ""
                    data[canonical].append(val)
        out: Dict[str, np.ndarray] = {}
        for canonical, vals in data.items():
            stripped = canonical.rstrip("?")
            if not vals:
                # Optional missing column — leave out.
                continue
            try:
                out[stripped] = np.asarray(vals, dtype=float)
            except ValueError:
                out[stripped] = np.asarray(vals, dtype=object)
        return out
    except OSError:
        return None
