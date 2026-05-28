"""External-tool delegation registry.

Some figure types are rendered better by domain-specific tools than by
matplotlib + seaborn — pseudotime trajectories by `scvelo`, cell-cell
chord plots by `LIANA`/`CellChat`, genome tracks by `pyGenomeTracks`.
This module is the registry that lets stage modules say "for figure X,
delegate to tool Y" while still routing the final image through
`core.savefig` so DPI/format/metadata-strip/provenance-footer stay
consistent.

Determinism guarantees are tool-specific. Each registration declares
whether the underlying tool produces byte-stable output; non-
deterministic tools' outputs land outside the snapshot-test gate but
are still produced and recorded in `FigureManifest.formats`.
"""

from __future__ import annotations

import warnings
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, List, Optional


DelegateFn = Callable[..., "object"]
"""Tool-specific renderer. Receives view_data + an output Path; returns a
matplotlib Figure (preferred — savefig handles the rest), or None if it
already wrote the file at the path itself.
"""


@dataclass(frozen=True)
class DelegationSpec:
    figure_id: str
    tool: str
    impl: DelegateFn
    deterministic: bool
    requires: List[str] = field(default_factory=list)
    notes: Optional[str] = None


_REGISTRY: Dict[str, Dict[str, DelegationSpec]] = {}


def register_delegate(
    stage_id: str,
    figure_id: str,
    *,
    tool: str,
    deterministic: bool,
    requires: Optional[List[str]] = None,
    notes: Optional[str] = None,
) -> Callable[[DelegateFn], DelegateFn]:
    """Decorator: declare that a stage's figure_id delegates rendering
    to `tool`. The wrapped function receives the view_data dict + an
    output path and returns a matplotlib Figure that core.savefig can
    finalize.
    """

    def deco(fn: DelegateFn) -> DelegateFn:
        spec = DelegationSpec(
            figure_id=figure_id,
            tool=tool,
            impl=fn,
            deterministic=deterministic,
            requires=list(requires or []),
            notes=notes,
        )
        _REGISTRY.setdefault(stage_id, {})[figure_id] = spec
        return fn

    return deco


def lookup(stage_id: str, figure_id: str) -> Optional[DelegationSpec]:
    return _REGISTRY.get(stage_id, {}).get(figure_id)


def list_delegations() -> List[DelegationSpec]:
    return [s for stage in _REGISTRY.values() for s in stage.values()]


def has_required_modules(spec: DelegationSpec) -> bool:
    """True when every module name in `spec.requires` is importable in
    the current env. Used by `generate()` to fall back to the native
    matplotlib renderer when a delegated tool isn't installed.
    """
    import importlib

    for mod in spec.requires:
        try:
            importlib.import_module(mod)
        except ImportError:
            return False
    return True


def warn_non_deterministic(spec: DelegationSpec) -> None:
    """Emit a one-time warning when a non-deterministic delegate is
    invoked so byte-snapshot tests don't silently start failing.
    """
    if spec.deterministic:
        return
    warnings.warn(
        f"figure '{spec.figure_id}' delegates to '{spec.tool}' which is "
        f"NOT byte-deterministic; this figure is excluded from snapshot "
        f"tests but will still appear in the package gallery.",
        stacklevel=2,
    )
