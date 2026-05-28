"""Core plotting primitives — determinism, dispatch, registry.

Every emitted package carries this module under runtime/plotting/core.py.
Stage modules import from here rather than from matplotlib directly so
the Agg backend + metadata stripping + seed plumbing are always applied.

Any figure produced through savefig() in this module is byte-reproducible
for the same (stage, figure_id, inputs) on the same pinned matplotlib
version — a requirement of the package-level RO-Crate guarantee.
"""

from __future__ import annotations

import hashlib
import importlib
import json
import os
import random
import warnings
from contextlib import contextmanager
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Iterator, List, Mapping, Optional, Sequence, Tuple

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt
import numpy as np

def _read_shared_version() -> str:
    """F20 — single source of truth for the plotting library version. Both
    Python and R sides read this file so a version bump touches one place
    instead of drifting silently across two.
    """
    return (Path(__file__).resolve().parent / "VERSION").read_text().strip()


__version__ = _read_shared_version()

_THEME_PATH = Path(__file__).resolve().parent / "theme.json"

# Wong (Okabe-Ito) 8-color colorblind-safe categorical palette.
# Wong, B. Points of view: Color blindness. Nat Methods 8, 441 (2011).
_WONG_PALETTE: Tuple[str, ...] = (
    "#000000",  # black
    "#E69F00",  # orange
    "#56B4E9",  # sky blue
    "#009E73",  # bluish green
    "#F0E442",  # yellow
    "#0072B2",  # blue
    "#D55E00",  # vermillion
    "#CC79A7",  # reddish purple
)

# Glasbey-style 20-color extension. Wong's 8 followed by 12 perceptually
# distinct additions chosen for category counts the agent commonly hits
# (clusters, cell types, study cohorts). Not strictly colorblind-safe at
# n>8 — at high n the figure-level fix is shape/label, not just color.
_GLASBEY20_PALETTE: Tuple[str, ...] = _WONG_PALETTE + (
    "#999999",  # grey
    "#882E72",  # purple
    "#1965B0",  # navy
    "#7BAFDE",  # light blue
    "#4EB265",  # green
    "#CAE0AB",  # pale green
    "#F7F056",  # pale yellow
    "#EE8026",  # pumpkin
    "#DC050C",  # bright red
    "#B17BA6",  # mauve
    "#5289C7",  # steel blue
    "#882027",  # dark red
)


FigureFn = Callable[["FigureContext", Path], Optional[Path]]
ViewFn = Callable[["FigureContext"], Dict[str, Any]]


@dataclass
class FigureContext:
    """Passed into every FigureFn — carries the stage's outputs dir, the
    agent-written manifest, and a deterministic RNG keyed by (stage, fig).
    """

    stage_id: str
    figure_id: str
    outputs_dir: Path
    manifest: Dict[str, Any]
    rng: np.random.Generator
    seed: int

    def load_artifact(self, relative: str) -> Path:
        """Return an absolute path inside the stage's outputs dir; the
        caller owns the open/read. Raises FileNotFoundError when missing
        so the dispatcher can surface it as a per-figure warning without
        killing the whole stage.
        """
        p = (self.outputs_dir / relative).resolve()
        if not p.exists():
            raise FileNotFoundError(f"stage artifact not found: {relative}")
        return p


@dataclass
class FigureManifest:
    """Returned from generate(). `written` maps figure_id → primary path
    (the format named in `task.spec.required_figures`). `formats` is
    an optional sibling map of figure_id → list of all written paths
    (PNG + PDF/SVG when dual-format is on). `skipped` + `errors` carry
    per-figure diagnostic text so the validator has something to report.
    """

    stage_id: str
    written: Dict[str, Path] = field(default_factory=dict)
    formats: Dict[str, List[Path]] = field(default_factory=dict)
    skipped: Dict[str, str] = field(default_factory=dict)
    errors: Dict[str, str] = field(default_factory=dict)

    def to_json(self) -> Dict[str, Any]:
        return {
            "stage_id": self.stage_id,
            "written": {fid: str(p) for fid, p in self.written.items()},
            "formats": {
                fid: [str(p) for p in paths] for fid, paths in self.formats.items()
            },
            "skipped": self.skipped,
            "errors": self.errors,
        }


class FigureRegistry:
    """Per-stage registry of figure_id → FigureFn. Stage modules attach
    to a module-level registry; `generate()` resolves by importing the
    stage module and reading its `FIGURES` attribute.
    """

    def __init__(self, stage_id: str) -> None:
        self.stage_id = stage_id
        self.figures: Dict[str, FigureFn] = {}

    def register(self, figure_id: str, fn: FigureFn) -> FigureFn:
        self.figures[figure_id] = fn
        return fn

    def __iter__(self) -> Iterator[str]:
        return iter(self.figures)

    def __contains__(self, figure_id: str) -> bool:
        return figure_id in self.figures

    def get(self, figure_id: str) -> Optional[FigureFn]:
        return self.figures.get(figure_id)


class ViewRegistry:
    """Per-stage registry of view_id → ViewFn. View functions return
    JSON-serializable dicts that the interactive dashboard renders
    client-side. Functions share FigureContext with the figure
    registry so stage modules can co-locate a static PNG and an
    interactive view that draws from the same inputs.
    """

    def __init__(self, stage_id: str) -> None:
        self.stage_id = stage_id
        self.views: Dict[str, ViewFn] = {}

    def register(self, view_id: str, fn: ViewFn) -> ViewFn:
        self.views[view_id] = fn
        return fn

    def __iter__(self) -> Iterator[str]:
        return iter(self.views)

    def __contains__(self, view_id: str) -> bool:
        return view_id in self.views

    def get(self, view_id: str) -> Optional[ViewFn]:
        return self.views.get(view_id)


def register_figure(registry: FigureRegistry, figure_id: str) -> Callable[[FigureFn], FigureFn]:
    """Decorator: @register_figure(FIGURES, "umap_clusters"). Keeps stage
    modules declarative without a huge dict literal at the bottom.
    """

    def deco(fn: FigureFn) -> FigureFn:
        registry.register(figure_id, fn)
        return fn

    return deco


def register_alias(registry: FigureRegistry, alias_id: str, source_id: str) -> None:
    """Plan §S14.5 — register `alias_id` as an alias of `source_id` in
    the same stage's FigureRegistry. Both ids resolve to the same
    figure function so taxonomies that reuse a figure id under a
    different SME-facing name don't need a duplicate decorator.

    Example: in lib/plotting/stages/cell_type_annotation.py, after
    registering `umap_by_celltype`, also alias `class_annotation` so
    the histopath taxonomy's `required_figures` list can name either
    id and resolve to the same renderer:

    ```python
    register_alias(FIGURES, "class_annotation", "umap_by_celltype")
    ```

    Raises ValueError if the source id isn't registered yet —
    aliases must point at concrete renderers, not other aliases
    (one indirection level keeps the resolution graph trivially
    deterministic).
    """
    fn = registry.get(source_id)
    if fn is None:
        raise ValueError(
            f"register_alias: source id '{source_id}' not yet registered "
            f"in stage '{registry.stage_id}' — register the renderer "
            "before calling register_alias."
        )
    registry.register(alias_id, fn)


def register_view(registry: ViewRegistry, view_id: str) -> Callable[[ViewFn], ViewFn]:
    """Decorator: @register_view(VIEWS, "embedding_scatter")."""

    def deco(fn: ViewFn) -> ViewFn:
        registry.register(view_id, fn)
        return fn

    return deco


def stage_registry(stage_id: str) -> FigureRegistry:
    """Helper for stage modules: `FIGURES = stage_registry("clustering")`."""
    return FigureRegistry(stage_id)


def stage_view_registry(stage_id: str) -> ViewRegistry:
    """Helper for stage modules: `VIEWS = stage_view_registry("clustering")`."""
    return ViewRegistry(stage_id)


def _load_theme() -> Dict[str, Any]:
    """Read theme.json next to this module. Returns a hardcoded fallback
    when the file is absent so the library still works in degraded
    environments — the fallback matches the shipped theme.json so byte-
    output is identical with or without it.
    """
    if _THEME_PATH.exists():
        try:
            return json.loads(_THEME_PATH.read_text())
        except (OSError, json.JSONDecodeError) as e:  # pragma: no cover
            warnings.warn(f"theme.json unreadable, falling back to defaults: {e}")
    return {
        "schema_version": 1,
        "fonts": {
            "family": "sans-serif",
            "stack": ["Arial", "Helvetica", "DejaVu Sans"],
            "body_pt": 8,
            "title_pt": 9,
            "tick_pt": 7,
            "legend_pt": 7,
            "footer_pt": 5,
        },
        "axes": {
            "linewidth": 0.5,
            "spines_top": False,
            "spines_right": False,
            "constrained_layout": True,
        },
        "palette": {
            "categorical_8": "wong",
            "categorical_extended": "glasbey20",
            "sequential": "viridis",
            "diverging": "RdBu_r",
            "sig_up": "#D55E00",
            "sig_down": "#0072B2",
            "non_sig": "#999999",
        },
        "output": {
            "png_dpi": 300,
            "formats": ["png", "pdf"],
            "pdf_fonttype": 42,
            "svg_fonttype": "none",
            "rasterize_threshold_n": 50000,
        },
        "provenance_footer": True,
    }


THEME: Dict[str, Any] = _load_theme()

_PRESETS_DIR = Path(__file__).resolve().parent / "presets"


def _deep_merge(base: Dict[str, Any], overlay: Dict[str, Any]) -> Dict[str, Any]:
    """Recursive merge: overlay values win, except `_*` annotation keys
    in the overlay get dropped from the result so the merged theme stays
    machine-readable.
    """
    out: Dict[str, Any] = dict(base)
    for k, v in overlay.items():
        if k.startswith("_"):
            continue
        if isinstance(v, dict) and isinstance(out.get(k), dict):
            out[k] = _deep_merge(out[k], v)
        else:
            out[k] = v
    return out


def available_presets() -> List[str]:
    """Names of every preset shipped under runtime/plotting/presets/.
    Used by the chat UI to populate the journal-style selector and by
    tests to round-trip every preset.
    """
    if not _PRESETS_DIR.exists():
        return []
    return sorted(p.stem for p in _PRESETS_DIR.glob("*.json"))


def load_preset(name: str) -> Dict[str, Any]:
    """Read a journal-style preset and return a theme dict that overlays
    the shipped theme.json. Raises FileNotFoundError when the preset
    name doesn't match a shipped JSON. Pass the result to apply_theme()
    or assign it to module-level THEME to switch styles.
    """
    path = _PRESETS_DIR / f"{name}.json"
    if not path.exists():
        raise FileNotFoundError(f"unknown preset '{name}'; available: {available_presets()}")
    overlay = json.loads(path.read_text())
    return _deep_merge(THEME, overlay)


def use_preset(name: str) -> None:
    """Switch the module-level THEME to the named preset and re-apply
    rcParams. Affects every subsequent generate()/savefig() call. Tests
    use this to verify presets round-trip without mutating their own
    theme files.
    """
    global THEME
    THEME = load_preset(name)
    apply_theme(THEME)


def apply_theme(theme: Optional[Dict[str, Any]] = None) -> None:
    """Apply the loaded theme to matplotlib's rcParams. Idempotent — safe
    to call multiple times. Called automatically on module import; tests
    can re-apply after monkeypatching rcParams.
    """
    t = theme if theme is not None else THEME
    fonts = t.get("fonts", {})
    axes = t.get("axes", {})
    output = t.get("output", {})
    plt.rcParams.update(
        {
            "font.family": fonts.get("family", "sans-serif"),
            "font.sans-serif": fonts.get("stack", ["DejaVu Sans"]),
            "font.size": fonts.get("body_pt", 8),
            "axes.titlesize": fonts.get("title_pt", 9),
            "axes.labelsize": fonts.get("body_pt", 8),
            "axes.linewidth": axes.get("linewidth", 0.5),
            "axes.spines.top": axes.get("spines_top", False),
            "axes.spines.right": axes.get("spines_right", False),
            "xtick.labelsize": fonts.get("tick_pt", 7),
            "ytick.labelsize": fonts.get("tick_pt", 7),
            "legend.fontsize": fonts.get("legend_pt", 7),
            "legend.frameon": False,
            "pdf.fonttype": output.get("pdf_fonttype", 42),
            "ps.fonttype": output.get("pdf_fonttype", 42),
            "svg.fonttype": output.get("svg_fonttype", "none"),
            "savefig.bbox": "tight",
        }
    )


# Apply on import so any caller using `import lib.plotting.core` (or the
# emitted `runtime.plotting.core`) inherits the theme without needing to
# remember to call apply_theme().
apply_theme()


def wong_palette() -> Tuple[str, ...]:
    """Wong (Okabe-Ito) 8-color colorblind-safe categorical palette.

    Use for categorical encodings up to 8 levels. Beyond 8, prefer
    `categorical_palette(n)` which falls back to the 20-color glasbey
    extension and warns once at high n.
    """
    return _WONG_PALETTE


def glasbey20_palette() -> Tuple[str, ...]:
    """20-color extended palette. Wong's 8 + 12 perceptually-distinct
    additions. Not strictly colorblind-safe past 8 — pair with shape or
    label encodings when n > 8.
    """
    return _GLASBEY20_PALETTE


_HIGH_CARD_WARNED: set = set()


def categorical_palette(n: int, *, name: Optional[str] = None) -> List[str]:
    """Return n colors for a categorical encoding using the theme palette.

    n <= 8 → Wong (colorblind-safe).
    n <= 20 → glasbey20 (Wong + 12 distinct extensions).
    n > 20 → glasbey20 cycled with a one-time warning per stage; at this
    cardinality color alone is insufficient and the figure should add
    shape/label encoding.
    """
    if n <= 0:
        return []
    base = _WONG_PALETTE if n <= 8 else _GLASBEY20_PALETTE
    if n > 20:
        key = name or "global"
        if key not in _HIGH_CARD_WARNED:
            _HIGH_CARD_WARNED.add(key)
            warnings.warn(
                f"categorical_palette({n}) exceeds 20 colors; cycling glasbey20. "
                "At this cardinality, encode category by shape or label as well — "
                "color alone is not perceptually distinguishable.",
                stacklevel=2,
            )
    return [base[i % len(base)] for i in range(n)]


def _provenance_text(stage_id: str) -> str:
    """Build the per-figure footer text. Reads ECAA_PACKAGE_ID and
    ECAA_GIT_SHA from env when present; falls back to "unknown" so the
    footer text is byte-stable across runs in environments that do not
    set them (e.g. the unit test suite).
    """
    package_id = os.environ.get("ECAA_PACKAGE_ID", "unknown")
    git_sha = os.environ.get("ECAA_GIT_SHA", "unknown")
    sha_short = git_sha[:7] if git_sha and git_sha != "unknown" else "unknown"
    return f"{package_id} · {stage_id} · plotting v{__version__} · git@{sha_short}"


def _add_provenance_footer(fig: plt.Figure, stage_id: str) -> None:
    """Stamp the package + stage + library version + git sha at the
    bottom-right of the figure. No-op when the theme has the footer
    disabled.
    """
    if not THEME.get("provenance_footer", True):
        return
    text = _provenance_text(stage_id)
    fig.text(
        0.995,
        0.005,
        text,
        ha="right",
        va="bottom",
        fontsize=THEME.get("fonts", {}).get("footer_pt", 5),
        color="#888888",
        family=THEME.get("fonts", {}).get("family", "sans-serif"),
    )


def _deterministic_seed(stage_id: str, figure_id: str, extra: Optional[str] = None) -> int:
    h = hashlib.sha256()
    h.update(stage_id.encode())
    h.update(b"|")
    h.update(figure_id.encode())
    if extra:
        h.update(b"|")
        h.update(extra.encode())
    return int.from_bytes(h.digest()[:8], "big", signed=False) & 0x7FFFFFFF


@contextmanager
def seeded(seed: int) -> Iterator[None]:
    """Freeze every RNG source a figure function might touch so the
    resulting PNG bytes match across re-runs.
    """
    np_state = np.random.get_state()
    py_state = random.getstate()
    try:
        np.random.seed(seed)
        random.seed(seed)
        yield
    finally:
        np.random.set_state(np_state)
        random.setstate(py_state)


# Thread-safe-enough context for the active stage_id during generate().
# Helpers like `bar`/`scatter`/`heatmap` can pull it via `_active_stage()`
# without each stage module having to thread it through every call.
# A list is used as a simple stack so nested generate() calls (rare)
# unwind correctly.
_ACTIVE_STAGE: List[str] = []


@contextmanager
def _stage_context(stage_id: str) -> Iterator[None]:
    _ACTIVE_STAGE.append(stage_id)
    try:
        yield
    finally:
        _ACTIVE_STAGE.pop()


def _active_stage() -> str:
    return _ACTIVE_STAGE[-1] if _ACTIVE_STAGE else "unknown"


def _stripped_metadata() -> Dict[str, Optional[str]]:
    """matplotlib metadata keys that drift across runs/hosts. Suppressing
    all of them is what makes PNG and PDF byte-identical for repeated
    invocations on a pinned matplotlib + freetype.
    """
    return {
        "Software": None,
        "Source": None,
        "Creator": None,
        "CreationDate": None,
        "ModDate": None,
        "Producer": None,
        "Title": None,
        "Author": None,
        "Subject": None,
        "Keywords": None,
    }


def savefig(
    fig: plt.Figure,
    path: Path,
    *,
    dpi: Optional[int] = None,
    formats: Optional[Sequence[str]] = None,
    stage_id: Optional[str] = None,
    extra_metadata_strip: bool = True,
) -> Path:
    """Write a figure to disk in one or more formats with stripped
    metadata so repeated runs produce identical bytes.

    `path` is the *primary* output path; its suffix selects the primary
    format. When `formats` is None the theme's `output.formats` list is
    consulted and any format not matching `path.suffix` is written
    alongside it (same parent + stem, suffix swapped). The primary
    format's path is returned for backwards compatibility with older
    stage modules; the full set is reachable via the FigureManifest.

    `stage_id`, when provided, drives the provenance footer. Stage
    callers from inside `generate()` get this for free; ad-hoc callers
    (tests, helpers) that don't pass it produce a footer with a
    "unknown" stage segment, which is fine for byte-stability.
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    output_cfg = THEME.get("output", {})
    if dpi is None:
        dpi = output_cfg.get("png_dpi", 300)
    if formats is None:
        formats = output_cfg.get("formats", ["png"])
    metadata = _stripped_metadata() if extra_metadata_strip else {}

    # Provenance footer is overlaid once on the figure before any
    # savefig — both PNG and PDF inherit the same stamp so the two
    # formats stay visually paired.
    _add_provenance_footer(fig, stage_id or _active_stage())

    primary_suffix = path.suffix.lstrip(".").lower() or "png"
    desired = [primary_suffix] + [
        f.lower() for f in formats if f.lower() != primary_suffix
    ]
    written: List[Path] = []
    for fmt in desired:
        out = path.with_suffix(f".{fmt}")
        # PDF/SVG/PS use vector pipelines and ignore dpi for raster
        # quality — but matplotlib still accepts the kwarg, so passing
        # it uniformly keeps the call site simple.
        fig.savefig(out, dpi=dpi, metadata=metadata)
        written.append(out)
    plt.close(fig)
    # Primary path returned for back-compat; the FigureManifest
    # captures the rest via the `written` dict in `generate()`.
    return written[0]


def _load_manifest(outputs_dir: Path) -> Dict[str, Any]:
    """Best-effort load of the stage's manifest.json; returns an empty
    dict on miss so figure functions can fall back to directory scans.
    """
    p = outputs_dir / "manifest.json"
    if not p.exists():
        return {}
    try:
        return json.loads(p.read_text())
    except (OSError, json.JSONDecodeError) as e:
        warnings.warn(f"manifest.json unreadable in {outputs_dir}: {e}")
        return {}


def resolve_artifact_path(
    ctx: "FigureContext", manifest_key: str, default_name: str
) -> Optional[Path]:
    """R6-C3 — shared helper for the ``_resolve(ctx, key, default)`` pattern
    that 13 stage renderers used to duplicate locally.

    Resolves an artifact path by consulting the agent-written
    ``manifest.json`` first (under ``manifest_key``) and falling back to
    ``outputs_dir / default_name``. Returns ``None`` if neither exists.
    Absolute paths in the manifest are honored; relative paths are
    anchored to ``ctx.outputs_dir``.
    """
    manifest_value = ctx.manifest.get(manifest_key)
    if manifest_value:
        candidate = (
            Path(manifest_value)
            if Path(manifest_value).is_absolute()
            else ctx.outputs_dir / manifest_value
        )
        if candidate.exists():
            return candidate
    fallback = ctx.outputs_dir / default_name
    return fallback if fallback.exists() else None


def load_tsv_columns(path: Optional[Path]) -> Optional[Dict[str, List[str]]]:
    """R6-C3 — shared helper for the ``_load(path)`` pattern that 11 stage
    renderers used to duplicate locally.

    Reads a TSV (header row + tab-separated body) into a column-major
    ``{header: [string values]}`` dict. Rows whose split length does not
    match the header width are silently skipped. Returns ``None`` when
    the path is missing or ``None`` so the caller can short-circuit
    with ``cols = load_tsv_columns(p) or {}``.

    String-typed by design — callers cast numeric columns at use site
    so missing/empty cells stay distinguishable from numeric zeros.
    """
    if path is None or not path.exists():
        return None
    with open(path) as f:
        header = f.readline().rstrip("\n").split("\t")
        cols: Dict[str, List[str]] = {h: [] for h in header}
        for line in f:
            parts = line.rstrip("\n").split("\t")
            if len(parts) == len(header):
                for h, v in zip(header, parts):
                    cols[h].append(v)
    return cols


def iter_runs(ctx: "FigureContext") -> List[Dict[str, Any]]:
    """R6-C3 — shared helper for the ``_iter_runs(ctx)`` pattern.

    Returns a non-empty list of run dicts taken from the manifest's
    ``compartments`` key (preferred) or ``runs`` key (fallback). When
    neither is present, returns a single placeholder run ``{"id": "run"}``
    so callers can keep a uniform loop shape against flat-layout outputs.

    Two stage renderers use this exact compartments-first ordering and
    migrate to it: ``batch_correction`` and ``clustering``.

    Three other renderers (``cell_type_annotation``, ``normalization``,
    ``trajectory_analysis``) keep local copies that prefer ``runs`` over
    ``compartments`` — to preserve byte-equivalent behavior when both
    keys are present, those callers are not migrated to this helper.

    ``dimensionality_reduction`` keeps a local copy that injects extra
    keys (``variance_explained``, ``embedding_path``) into the
    placeholder — also not migrated.
    """
    runs = ctx.manifest.get("compartments") or ctx.manifest.get("runs")
    if isinstance(runs, list) and runs:
        return runs
    return [{"id": "run"}]


def _import_stage(stage_id: str) -> Optional[Any]:
    """Import runtime.plotting.stages.<stage_id> — returns None when the
    module does not exist so callers can decide to warn vs raise.
    """
    # Accept composed stage ids like "wave1.clustering" by splitting on
    # the dot and trying the final segment. Keeps cross-taxonomy
    # composition compatible with a flat stage module layout.
    names: List[str] = []
    if "." in stage_id:
        names.append(stage_id)
        names.append(stage_id.rsplit(".", 1)[-1])
    else:
        names.append(stage_id)
    for name in names:
        module_path = f"runtime.plotting.stages.{name.replace('.', '_')}"
        try:
            return importlib.import_module(module_path)
        except ImportError:
            continue
    # Also allow invocation from outside an emitted package — useful
    # from the repo's lib/plotting/tests/.
    for name in names:
        module_path = f"lib.plotting.stages.{name.replace('.', '_')}"
        try:
            return importlib.import_module(module_path)
        except ImportError:
            continue
    return None


def generate(
    stage_id: str,
    outputs_dir: Path,
    figures_dir: Optional[Path] = None,
    required: Optional[Iterable[str]] = None,
    write_manifest: bool = True,
    manifest_override: Optional[Mapping[str, Any]] = None,
) -> FigureManifest:
    """Dispatch entry point. Imports the stage module, resolves its
    registry, and produces every figure in `required`. Writes
    `figures/manifest.json` with the result when `write_manifest` is
    True so the validator has something structured to check.

    `required=None` renders every figure the module exposes.
    """
    outputs_dir = Path(outputs_dir)
    figures_dir = Path(figures_dir) if figures_dir else outputs_dir / "figures"
    figures_dir.mkdir(parents=True, exist_ok=True)

    mf = FigureManifest(stage_id=stage_id)

    module = _import_stage(stage_id)
    if module is None:
        mf.skipped["*"] = f"no plotting stage module registered for '{stage_id}'"
        if write_manifest:
            _write_figures_manifest(figures_dir, mf)
        return mf

    registry: Optional[FigureRegistry] = getattr(module, "FIGURES", None)
    if registry is None:
        mf.skipped["*"] = (
            f"module runtime.plotting.stages.{stage_id} imported but exposes no FIGURES registry"
        )
        if write_manifest:
            _write_figures_manifest(figures_dir, mf)
        return mf

    manifest = (
        dict(manifest_override)
        if manifest_override is not None
        else _load_manifest(outputs_dir)
    )
    requested = list(required) if required is not None else list(registry)

    desired_formats = THEME.get("output", {}).get("formats", ["png"])

    # Lazy import — delegation is optional. The dispatcher only needs
    # to know about it when a stage's figure isn't in the FigureRegistry
    # but IS in the delegation registry.
    try:
        from . import delegation as _delegation
    except ImportError:  # pragma: no cover
        _delegation = None  # type: ignore[assignment]

    for fig_id in requested:
        fn = registry.get(fig_id)
        delegate_spec = (
            _delegation.lookup(stage_id, fig_id) if _delegation is not None else None
        )
        if fn is None and delegate_spec is None:
            mf.skipped[fig_id] = "not registered in stage module"
            continue
        if fn is None and delegate_spec is not None:
            if not _delegation.has_required_modules(delegate_spec):
                mf.skipped[fig_id] = (
                    f"delegate '{delegate_spec.tool}' requires "
                    f"{delegate_spec.requires} — not installed"
                )
                continue
            _delegation.warn_non_deterministic(delegate_spec)
            fn = _delegate_to_figure_fn(delegate_spec)
        seed = _deterministic_seed(stage_id, fig_id)
        rng = np.random.default_rng(seed)
        ctx = FigureContext(
            stage_id=stage_id,
            figure_id=fig_id,
            outputs_dir=outputs_dir,
            manifest=manifest,
            rng=rng,
            seed=seed,
        )
        target = figures_dir / f"{fig_id}.png"
        try:
            with seeded(seed), _stage_context(stage_id):
                produced = fn(ctx, target)
            primary = Path(produced if produced is not None else target)
            if primary.exists() and primary.stat().st_size > 0:
                mf.written[fig_id] = primary
                # Record the sibling formats savefig() may have written
                # alongside the primary (e.g. PDF next to the PNG).
                siblings: List[Path] = []
                for fmt in desired_formats:
                    sib = primary.with_suffix(f".{fmt.lower()}")
                    if sib.exists() and sib.stat().st_size > 0:
                        siblings.append(sib)
                if siblings:
                    mf.formats[fig_id] = siblings
            else:
                mf.errors[fig_id] = "figure function returned without writing"
        except FileNotFoundError as e:
            mf.skipped[fig_id] = f"input artifact missing: {e}"
        except Exception as e:
            mf.errors[fig_id] = f"{type(e).__name__}: {e}"
            # Don't let one bad figure kill the whole stage — the
            # validator reports the miss, SME sees a partial gallery.
            plt.close("all")

    if write_manifest:
        _write_figures_manifest(figures_dir, mf)

    # When the same stage registers interactive VIEWS, extract them too
    # so the dashboard surface has data to render without a separate
    # agent call. Silent no-op when the module exposes no VIEWS.
    if hasattr(module, "VIEWS") and getattr(module, "VIEWS") is not None:
        extract(stage_id, outputs_dir, write_manifest=write_manifest)

    return mf


def _delegate_to_figure_fn(spec: "Any") -> FigureFn:
    """Wrap a DelegationSpec.impl into the FigureFn signature `generate()`
    expects. The delegate may return either a matplotlib Figure (then
    savefig finalizes), or None (if it wrote the file itself).
    """

    def _wrapper(ctx: "FigureContext", out: Path) -> Optional[Path]:
        result = spec.impl(ctx, out)
        if result is None:
            return out if Path(out).exists() else None
        # Delegates that return a matplotlib Figure go through savefig
        # so DPI/format/metadata-strip/footer all apply uniformly.
        if hasattr(result, "savefig"):
            return savefig(result, out, stage_id=ctx.stage_id)
        return Path(result) if isinstance(result, (str, Path)) else None

    return _wrapper


def _write_figures_manifest(figures_dir: Path, mf: FigureManifest) -> None:
    path = figures_dir / "manifest.json"
    path.write_text(json.dumps(mf.to_json(), indent=2, sort_keys=True))


@dataclass
class ViewManifest:
    """Output of extract(): which views were written + reasons for any
    that failed. Mirrors FigureManifest so the dashboard route can
    check coverage with the same shape.
    """

    stage_id: str
    written: Dict[str, Path] = field(default_factory=dict)
    skipped: Dict[str, str] = field(default_factory=dict)
    errors: Dict[str, str] = field(default_factory=dict)

    def to_json(self) -> Dict[str, Any]:
        return {
            "stage_id": self.stage_id,
            "written": {vid: str(p) for vid, p in self.written.items()},
            "skipped": self.skipped,
            "errors": self.errors,
        }


def extract(
    stage_id: str,
    outputs_dir: Path,
    view_data_dir: Optional[Path] = None,
    required: Optional[Iterable[str]] = None,
    write_manifest: bool = True,
) -> ViewManifest:
    """Parallel to generate() but for interactive JSON views. Writes
    `view_data/<view_id>.json` per registered view; the dashboard
    surface streams those files straight to the browser. Deterministic
    RNG keyed by (stage, view) — same seed plumbing as figures.
    """
    outputs_dir = Path(outputs_dir)
    view_data_dir = Path(view_data_dir) if view_data_dir else outputs_dir / "view_data"
    view_data_dir.mkdir(parents=True, exist_ok=True)

    mf = ViewManifest(stage_id=stage_id)

    module = _import_stage(stage_id)
    if module is None:
        mf.skipped["*"] = f"no plotting stage module registered for '{stage_id}'"
        if write_manifest:
            _write_view_manifest(view_data_dir, mf)
        return mf

    registry: Optional[ViewRegistry] = getattr(module, "VIEWS", None)
    if registry is None:
        mf.skipped["*"] = (
            f"stage '{stage_id}' does not expose a VIEWS registry"
        )
        if write_manifest:
            _write_view_manifest(view_data_dir, mf)
        return mf

    manifest = _load_manifest(outputs_dir)
    requested = list(required) if required is not None else list(registry)

    for view_id in requested:
        fn = registry.get(view_id)
        if fn is None:
            mf.skipped[view_id] = "not registered in stage module"
            continue
        seed = _deterministic_seed(stage_id, view_id, "view")
        rng = np.random.default_rng(seed)
        ctx = FigureContext(
            stage_id=stage_id,
            figure_id=view_id,
            outputs_dir=outputs_dir,
            manifest=manifest,
            rng=rng,
            seed=seed,
        )
        target = view_data_dir / f"{view_id}.json"
        try:
            with seeded(seed):
                payload = fn(ctx)
            if not isinstance(payload, dict):
                mf.errors[view_id] = f"view returned {type(payload).__name__}, expected dict"
                continue
            wrapped = {
                "stage_id": stage_id,
                "view_id": view_id,
                "schema_version": 1,
                "data": payload,
            }
            target.write_text(json.dumps(wrapped, sort_keys=True))
            mf.written[view_id] = target
        except FileNotFoundError as e:
            mf.skipped[view_id] = f"input artifact missing: {e}"
        except Exception as e:
            mf.errors[view_id] = f"{type(e).__name__}: {e}"

    if write_manifest:
        _write_view_manifest(view_data_dir, mf)
    return mf


def _write_view_manifest(view_data_dir: Path, mf: ViewManifest) -> None:
    path = view_data_dir / "manifest.json"
    path.write_text(json.dumps(mf.to_json(), indent=2, sort_keys=True))


def _significance_marker(p_value: float) -> str:
    """APA-style significance bracket label."""
    if not np.isfinite(p_value):
        return "ns"
    if p_value < 0.001:
        return "***"
    if p_value < 0.01:
        return "**"
    if p_value < 0.05:
        return "*"
    return "ns"


def _pairwise_significance(
    values: List[np.ndarray],
    *,
    max_groups: int = 5,
) -> List[Tuple[int, int, float, str]]:
    """Compute pairwise Mann-Whitney U for groups[i] vs groups[j] with
    Bonferroni correction. Returns (i, j, p_corrected, marker) per
    pair. Skips pairs where either side has fewer than 3 observations.
    Returns empty list when more than `max_groups` groups (annotation
    becomes unreadable past that).
    """
    if len(values) > max_groups:
        return []
    try:
        from scipy.stats import mannwhitneyu
    except ImportError:  # pragma: no cover
        return []
    pairs: List[Tuple[int, int, float, str]] = []
    indices = [i for i, v in enumerate(values) if len(v) >= 3]
    n_pairs = len(indices) * (len(indices) - 1) // 2
    if n_pairs == 0:
        return []
    for ii, i in enumerate(indices):
        for j in indices[ii + 1 :]:
            try:
                p = mannwhitneyu(values[i], values[j], alternative="two-sided").pvalue
            except ValueError:
                continue
            p_corrected = min(1.0, p * n_pairs)
            pairs.append((i, j, float(p_corrected), _significance_marker(p_corrected)))
    return pairs


def violin(
    data: Dict[str, Iterable[float]],
    *,
    title: str,
    ylabel: str,
    out: Path,
    x_label: str = "group",
    figsize: tuple = (8.0, 6.0),
    show_points: Optional[bool] = None,
    show_significance: bool = True,
    sig_max_groups: int = 5,
) -> Path:
    """Generic violin plot — accepts a dict of group_name → values and
    writes one violin per group.

    Phase B contract:
    - Jitter overlay drawn for groups with n ≤ 200 observations
      (auto-decided when `show_points` is None) so individual data is
      visible.
    - Per-group `n=` annotated below x-axis.
    - Pairwise Mann-Whitney U significance markers (Bonferroni-
      corrected) drawn as brackets when group count ≤ `sig_max_groups`.
    """
    groups = list(data.keys())
    values = [np.asarray(list(data[g]), dtype=float) for g in groups]
    fig, ax = plt.subplots(figsize=figsize)
    if not any(len(v) for v in values):
        ax.set_xticks(range(len(groups)))
        ax.set_xticklabels(groups, rotation=45, ha="right")
        ax.set_xlabel(x_label)
        ax.set_ylabel(ylabel)
        ax.set_title(title)
        return savefig(fig, out)

    non_empty = [(i, v) for i, v in enumerate(values) if len(v) > 0]
    palette = categorical_palette(len(non_empty), name="violin")
    parts = ax.violinplot(
        [v for _, v in non_empty],
        positions=[i for i, _ in non_empty],
        showmeans=False,
        showmedians=True,
    )
    # Color violins from the theme palette so multi-violin figures
    # stay visually consistent with bar/scatter.
    for body, color in zip(parts.get("bodies", []), palette):
        body.set_facecolor(color)
        body.set_edgecolor("#333333")
        body.set_alpha(0.7)
    for key in ("cmedians", "cbars", "cmins", "cmaxes"):
        coll = parts.get(key)
        if coll is not None:
            coll.set_edgecolor("#333333")
            coll.set_linewidth(0.6)

    # Jitter overlay for small n. Decision auto when show_points=None.
    auto_show_points = show_points
    if auto_show_points is None:
        auto_show_points = max((len(v) for _, v in non_empty), default=0) <= 200
    if auto_show_points:
        for (i, v), color in zip(non_empty, palette):
            jitter = (np.random.default_rng(_deterministic_seed(
                _active_stage(), f"violin.jitter.{i}"
            )).uniform(-0.12, 0.12, size=len(v)))
            ax.scatter(
                np.full(len(v), i) + jitter,
                v,
                s=4,
                color=color,
                edgecolor="#333333",
                linewidth=0.2,
                alpha=0.6,
                zorder=3,
            )

    # n= annotation under each tick label.
    tick_labels = []
    for g, v in zip(groups, values):
        tick_labels.append(f"{g}\n(n={len(v)})")
    ax.set_xticks(range(len(groups)))
    ax.set_xticklabels(tick_labels, rotation=45, ha="right")
    ax.set_xlabel(x_label)
    ax.set_ylabel(ylabel)
    ax.set_title(title)

    # Pairwise significance brackets when group count is small.
    if show_significance and len(values) <= sig_max_groups:
        pairs = _pairwise_significance(values, max_groups=sig_max_groups)
        if pairs:
            ymax = max((float(np.nanmax(v)) for _, v in non_empty), default=1.0)
            ymin = min((float(np.nanmin(v)) for _, v in non_empty), default=0.0)
            yrange = (ymax - ymin) or 1.0
            step = yrange * 0.08
            cur = ymax + step
            for i, j, _, marker in pairs:
                ax.plot([i, i, j, j], [cur, cur + step * 0.3, cur + step * 0.3, cur],
                        color="#444444", linewidth=0.6)
                ax.text((i + j) / 2.0, cur + step * 0.4, marker,
                        ha="center", va="bottom", color="#222222")
                cur += step
    return savefig(fig, out)


def bar(
    names: List[str],
    values: List[float],
    *,
    title: str,
    ylabel: str,
    out: Path,
    xlabel: str = "",
    figsize: tuple = (8.0, 5.0),
    ci_lo: Optional[List[float]] = None,
    ci_hi: Optional[List[float]] = None,
    annotate_counts: Optional[bool] = None,
    horizontal: Optional[bool] = None,
    error_label: str = "95% CI",
) -> Path:
    """Bar chart with optional 95% CI error bars.

    Phase B contract:
    - When `ci_lo` + `ci_hi` are both provided, draw asymmetric error
      bars relative to each value and label them in the legend.
    - When n_bars > 12, auto-flip to horizontal so labels stay legible
      (override with `horizontal=False`).
    - Annotate raw counts above each bar when n_bars ≤ 20 (or set
      `annotate_counts` explicitly).
    """
    n = len(names)
    if horizontal is None:
        horizontal = n > 12
    if annotate_counts is None:
        annotate_counts = n <= 20
    fig, ax = plt.subplots(figsize=figsize)
    palette = categorical_palette(n, name="bar")
    positions = np.arange(n)

    err = None
    if ci_lo is not None and ci_hi is not None and n > 0:
        lo = np.asarray(ci_lo, dtype=float)
        hi = np.asarray(ci_hi, dtype=float)
        vals = np.asarray(values, dtype=float)
        # matplotlib expects (lower_distance, upper_distance) relative
        # to the bar value, both non-negative.
        lower = np.clip(vals - lo, a_min=0.0, a_max=None)
        upper = np.clip(hi - vals, a_min=0.0, a_max=None)
        err = np.array([lower, upper])

    if horizontal:
        ax.barh(positions, values, color=palette,
                xerr=err, error_kw=dict(ecolor="#333333", linewidth=0.6, capsize=2.5))
        ax.set_yticks(positions)
        ax.set_yticklabels(names)
        ax.invert_yaxis()
        ax.set_ylabel(xlabel)
        ax.set_xlabel(ylabel)
        if annotate_counts and not isinstance(values, np.ndarray):
            for i, v in enumerate(values):
                ax.text(v, i, f" {v:g}", va="center", ha="left")
    else:
        ax.bar(positions, values, color=palette,
               yerr=err, error_kw=dict(ecolor="#333333", linewidth=0.6, capsize=2.5))
        ax.set_xticks(positions)
        ax.set_xticklabels(names, rotation=45, ha="right")
        ax.set_xlabel(xlabel)
        ax.set_ylabel(ylabel)
        if annotate_counts:
            for i, v in enumerate(values):
                ax.text(i, v, f"{v:g}", ha="center", va="bottom")
    ax.set_title(title)
    if err is not None:
        # Tiny in-axes label so the reader knows what the bars represent
        # without forcing a legend on a 1-series figure.
        ax.text(
            0.99, 0.01, f"error bars: {error_label}",
            transform=ax.transAxes, ha="right", va="bottom",
            color="#666666",
        )
    return savefig(fig, out)


def scatter(
    x: np.ndarray,
    y: np.ndarray,
    *,
    title: str,
    xlabel: str,
    ylabel: str,
    out: Path,
    color: Optional[np.ndarray] = None,
    cmap: Optional[str] = None,
    point_size: float = 3.0,
    figsize: tuple = (7.0, 6.0),
) -> Path:
    """Scatter plot for continuous-color encodings. For categorical
    encodings, build the scatter call yourself with `categorical_palette`
    so legend ordering and color assignment are explicit.

    Auto-rasterizes the points layer when len(x) exceeds
    `theme.output.rasterize_threshold_n` so vector PDFs stay reasonably
    sized while keeping axes/text vector.
    """
    fig, ax = plt.subplots(figsize=figsize)
    if cmap is None:
        cmap = THEME.get("palette", {}).get("sequential", "viridis")
    rasterize = (
        len(x) > THEME.get("output", {}).get("rasterize_threshold_n", 50000)
    )
    sc = ax.scatter(
        x,
        y,
        c=color,
        s=point_size,
        cmap=cmap,
        alpha=0.7,
        linewidths=0,
        rasterized=rasterize,
    )
    if color is not None and np.issubdtype(np.asarray(color).dtype, np.number):
        fig.colorbar(sc, ax=ax, shrink=0.7)
    ax.set_xlabel(xlabel)
    ax.set_ylabel(ylabel)
    ax.set_title(title)
    return savefig(fig, out)


def _greedy_label_placement(
    coords: List[Tuple[float, float]],
    width: float,
    height: float,
    min_dist: float,
) -> List[Optional[Tuple[float, float]]]:
    """Greedy collision-avoidance for top-N labels on a volcano. Walks
    candidates in input order, accepts placements that don't fall
    within min_dist of any already-accepted placement (in data
    coordinates), nudges colliders upward by min_dist until they fit
    or until we've tried 4 times. Returns offsets per input point;
    None means "skip this label".
    """
    placed: List[Tuple[float, float]] = []
    out: List[Optional[Tuple[float, float]]] = []
    for x, y in coords:
        target = (x, y)
        for _ in range(4):
            if all(
                ((target[0] - px) / width) ** 2 + ((target[1] - py) / height) ** 2
                >= (min_dist) ** 2
                for px, py in placed
            ):
                placed.append(target)
                out.append(target)
                break
            target = (target[0], target[1] + height * min_dist * 0.6)
        else:
            out.append(None)
    return out


def volcano(
    log_fc: np.ndarray,
    neg_log10_p: np.ndarray,
    *,
    title: str,
    out: Path,
    fc_threshold: float = 1.0,
    p_threshold: float = 1.3,
    figsize: tuple = (7.0, 6.5),
    labels: Optional[List[str]] = None,
    label_top_n: int = 10,
) -> Path:
    """Volcano plot with the publication-quality contract:

    - Points colored by direction × significance using the theme palette
      (sig-up = vermillion, sig-down = blue, NS = grey).
    - Threshold lines at `±fc_threshold` and `p_threshold` (drawn even
      when no points exceed them so the reader sees the gate).
    - Up/down significant-gene counts annotated in the upper corners.
    - n_total annotated under the title.
    - Top-N gene labels chosen by |log2FC| × −log10(p) with greedy
      collision avoidance so labels don't overlap each other.
    """
    log_fc = np.asarray(log_fc, dtype=float)
    neg_log10_p = np.asarray(neg_log10_p, dtype=float)
    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    sig_down = palette.get("sig_down", "#0072B2")
    non_sig = palette.get("non_sig", "#999999")

    sig_mask = (np.abs(log_fc) >= fc_threshold) & (neg_log10_p >= p_threshold)
    up_mask = sig_mask & (log_fc > 0)
    down_mask = sig_mask & (log_fc < 0)

    colors = np.where(up_mask, sig_up, np.where(down_mask, sig_down, non_sig))

    rasterize = (
        len(log_fc) > THEME.get("output", {}).get("rasterize_threshold_n", 50000)
    )
    fig, ax = plt.subplots(figsize=figsize)
    ax.scatter(
        log_fc,
        neg_log10_p,
        c=colors,
        s=6,
        alpha=0.75,
        linewidths=0,
        rasterized=rasterize,
    )
    ax.axhline(p_threshold, color="#444444", linestyle="--", linewidth=0.5)
    ax.axvline(fc_threshold, color="#444444", linestyle="--", linewidth=0.5)
    ax.axvline(-fc_threshold, color="#444444", linestyle="--", linewidth=0.5)
    ax.set_xlabel("log₂ fold change")
    ax.set_ylabel("−log₁₀ p-value")
    n_up = int(up_mask.sum())
    n_down = int(down_mask.sum())
    n_total = int(len(log_fc))
    ax.set_title(f"{title}\nn = {n_total} features")

    # Up/down counts in the upper corners, axis-coords so they don't
    # collide with points regardless of data range.
    ax.text(
        0.99, 0.97, f"↑ {n_up}",
        transform=ax.transAxes, ha="right", va="top",
        color=sig_up, fontweight="bold",
    )
    ax.text(
        0.01, 0.97, f"↓ {n_down}",
        transform=ax.transAxes, ha="left", va="top",
        color=sig_down, fontweight="bold",
    )

    if labels is not None and label_top_n > 0:
        score = np.abs(log_fc) * np.where(np.isfinite(neg_log10_p), neg_log10_p, 0.0)
        # Restrict labelling to significant features when any are
        # significant — labelling NS noise wastes ink.
        if sig_mask.any():
            score = np.where(sig_mask, score, -np.inf)
        order = np.argsort(score)[::-1][:label_top_n]
        order = [int(i) for i in order if np.isfinite(score[i])]
        coords = [(float(log_fc[i]), float(neg_log10_p[i])) for i in order]
        x_range = float(np.nanmax(log_fc) - np.nanmin(log_fc)) or 1.0
        y_range = float(np.nanmax(neg_log10_p) - np.nanmin(neg_log10_p)) or 1.0
        placements = _greedy_label_placement(
            coords, width=x_range, height=y_range, min_dist=0.04
        )
        for idx, place in zip(order, placements):
            if place is None:
                continue
            ax.annotate(
                labels[idx],
                xy=(log_fc[idx], neg_log10_p[idx]),
                xytext=place,
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#222222",
                arrowprops=dict(
                    arrowstyle="-", color="#888888", linewidth=0.3,
                ) if place != (log_fc[idx], neg_log10_p[idx]) else None,
            )
    return savefig(fig, out)


def _hierarchical_order(
    matrix: np.ndarray, axis: int = 0, method: str = "average"
) -> Tuple[Optional[np.ndarray], Optional[np.ndarray]]:
    """Compute hierarchical clustering ordering along an axis. Returns
    (order, linkage_matrix) or (None, None) when scipy isn't available
    or matrix is too small / has NaNs that block distance computation.
    """
    try:
        from scipy.cluster.hierarchy import linkage, leaves_list
        from scipy.spatial.distance import pdist
    except ImportError:  # pragma: no cover
        return None, None
    arr = matrix if axis == 0 else matrix.T
    if arr.shape[0] < 2:
        return None, None
    if not np.all(np.isfinite(arr)):
        # Replace NaN with row/col mean so distance is computable; the
        # ordering is best-effort.
        arr = np.where(np.isfinite(arr), arr, np.nanmean(arr))
        if not np.all(np.isfinite(arr)):
            arr = np.nan_to_num(arr, nan=0.0)
    try:
        d = pdist(arr, metric="euclidean")
        Z = linkage(d, method=method)  # noqa: N806  # Z is scipy linkage matrix convention
    except (ValueError, FloatingPointError):  # pragma: no cover
        return None, None
    return np.asarray(leaves_list(Z)), np.asarray(Z)


def _draw_dendrogram(ax: "plt.Axes", Z: np.ndarray, *, orientation: str) -> None:  # noqa: N803  # Z is scipy linkage matrix convention
    """Lightweight matplotlib dendrogram from a linkage matrix. We do
    this manually instead of using scipy.cluster.hierarchy.dendrogram
    so colors/linewidths follow the theme.
    """
    try:
        from scipy.cluster.hierarchy import dendrogram
    except ImportError:  # pragma: no cover
        return
    dendrogram(
        Z,
        ax=ax,
        orientation=orientation,
        no_labels=True,
        link_color_func=lambda _k: "#444444",
        above_threshold_color="#444444",
    )
    for spine in ax.spines.values():
        spine.set_visible(False)
    ax.tick_params(axis="both", which="both", length=0, labelbottom=False, labelleft=False)


def heatmap(
    matrix: np.ndarray,
    *,
    row_labels: List[str],
    col_labels: List[str],
    title: str,
    out: Path,
    cmap: Optional[str] = None,
    center: Optional[float] = 0.0,
    figsize: tuple = (9.0, 7.0),
    cbar_label: Optional[str] = None,
    cluster_rows: Optional[bool] = None,
    cluster_cols: Optional[bool] = None,
    z_score_rows: bool = False,
) -> Path:
    """Heatmap with optional hierarchical clustering on rows/columns.

    Phase B contract:
    - Auto-cluster rows when n_rows ≤ 200 and `cluster_rows` is None.
    - Auto-cluster columns when n_cols ≤ 200 and `cluster_cols` is None.
    - Row Z-scaling (`z_score_rows=True`) for matrices where rows have
      different units (e.g. gene-expression top-features heatmap).
    - Centered diverging palette by default; falls back to sequential
      when `center` is None.
    - Dendrograms drawn alongside the heatmap when clustering is on.
    """
    matrix = np.asarray(matrix, dtype=float)
    if cmap is None:
        cmap = (
            THEME.get("palette", {}).get("diverging", "RdBu_r")
            if center is not None
            else THEME.get("palette", {}).get("sequential", "viridis")
        )
    n_rows, n_cols = matrix.shape if matrix.ndim == 2 else (0, 0)
    if cluster_rows is None:
        cluster_rows = 2 <= n_rows <= 200
    if cluster_cols is None:
        cluster_cols = 2 <= n_cols <= 200

    if z_score_rows and matrix.size:
        means = np.nanmean(matrix, axis=1, keepdims=True)
        stds = np.nanstd(matrix, axis=1, keepdims=True)
        with np.errstate(invalid="ignore", divide="ignore"):
            matrix = (matrix - means) / np.where(stds > 0, stds, 1.0)

    row_order = np.arange(n_rows)
    col_order = np.arange(n_cols)
    Z_rows = Z_cols = None  # noqa: N806  # Z_rows/Z_cols are scipy linkage matrix convention
    if cluster_rows:
        ro, Z_rows = _hierarchical_order(matrix, axis=0)  # noqa: N806
        if ro is not None:
            row_order = ro
    if cluster_cols:
        co, Z_cols = _hierarchical_order(matrix, axis=1)  # noqa: N806
        if co is not None:
            col_order = co

    ordered = matrix[np.ix_(row_order, col_order)]
    ordered_rows = [row_labels[i] for i in row_order]
    ordered_cols = [col_labels[i] for i in col_order]

    use_grid = (Z_rows is not None) or (Z_cols is not None)
    if use_grid:
        # GridSpec layout: row dendrogram on left, col dendrogram on top,
        # heatmap center, colorbar on right.
        from matplotlib.gridspec import GridSpec
        fig = plt.figure(figsize=figsize)
        col_dendro_h = 0.18 if Z_cols is not None else 0.0
        row_dendro_w = 0.18 if Z_rows is not None else 0.0
        gs = GridSpec(
            2,
            3,
            figure=fig,
            width_ratios=[row_dendro_w if Z_rows is not None else 0.001, 1.0, 0.05],
            height_ratios=[col_dendro_h if Z_cols is not None else 0.001, 1.0],
            wspace=0.02,
            hspace=0.02,
        )
        ax_heat = fig.add_subplot(gs[1, 1])
        if Z_cols is not None:
            ax_top = fig.add_subplot(gs[0, 1], sharex=ax_heat)
            _draw_dendrogram(ax_top, Z_cols, orientation="top")
        if Z_rows is not None:
            ax_left = fig.add_subplot(gs[1, 0], sharey=ax_heat)
            _draw_dendrogram(ax_left, Z_rows, orientation="left")
        ax_cbar = fig.add_subplot(gs[1, 2])
    else:
        fig, ax_heat = plt.subplots(figsize=figsize)
        ax_cbar = None

    vmax = float(np.nanmax(np.abs(ordered))) if ordered.size else 1.0
    vmin = -vmax if center is not None else float(np.nanmin(ordered))
    if use_grid:
        # scipy dendrogram leaf centers live at 5, 15, 25, ... .  Use the
        # same coordinate system for the shared heatmap axes; otherwise the
        # dendrogram autoscale squeezes imshow's default 0, 1, 2, ... cells
        # into a thin strip.
        x_ticks = (
            np.arange(len(ordered_cols), dtype=float) * 10.0 + 5.0
            if Z_cols is not None
            else np.arange(len(ordered_cols), dtype=float)
        )
        y_ticks = (
            np.arange(len(ordered_rows), dtype=float) * 10.0 + 5.0
            if Z_rows is not None
            else np.arange(len(ordered_rows), dtype=float)
        )
        x_left, x_right = (
            (0.0, 10.0 * len(ordered_cols))
            if Z_cols is not None
            else (-0.5, len(ordered_cols) - 0.5)
        )
        y_bottom, y_top = (
            (10.0 * len(ordered_rows), 0.0)
            if Z_rows is not None
            else (len(ordered_rows) - 0.5, -0.5)
        )
        im = ax_heat.imshow(
            ordered,
            cmap=cmap,
            aspect="auto",
            vmin=vmin,
            vmax=vmax,
            extent=(x_left, x_right, y_bottom, y_top),
            origin="upper",
        )
        ax_heat.set_xlim(x_left, x_right)
        ax_heat.set_ylim(y_bottom, y_top)
    else:
        im = ax_heat.imshow(ordered, cmap=cmap, aspect="auto", vmin=vmin, vmax=vmax)
        x_ticks = np.arange(len(ordered_cols), dtype=float)
        y_ticks = np.arange(len(ordered_rows), dtype=float)
    ax_heat.set_xticks(x_ticks)
    ax_heat.set_xticklabels(ordered_cols, rotation=45, ha="right")
    ax_heat.set_yticks(y_ticks)
    ax_heat.set_yticklabels(ordered_rows)
    if use_grid and Z_rows is not None:
        dendro_label_pad = max(
            8.0,
            figsize[0] * row_dendro_w / (row_dendro_w + 1.0 + 0.05) * 72.0 + 4.0,
        )
        ax_heat.tick_params(axis="y", pad=dendro_label_pad)
    if not use_grid:
        ax_heat.set_title(title)
    cbar = fig.colorbar(im, cax=ax_cbar, ax=None if ax_cbar else ax_heat,
                        shrink=0.7 if ax_cbar is None else 1.0)
    if cbar_label:
        cbar.set_label(cbar_label)
    elif z_score_rows:
        cbar.set_label("row Z-score")
    if use_grid:
        fig.suptitle(title)
    return savefig(fig, out)


def arc(
    starts: "np.ndarray",
    ends: "np.ndarray",
    weights: "np.ndarray",
    *,
    title: str,
    xlabel: str,
    out: Path,
    figsize: Tuple[float, float] = (10.0, 4.0),
    color_above: str = "#D55E00",
    color_below: str = "#0072B2",
) -> Optional[Path]:
    """Arc diagram: each (start, end) pair drawn as a semicircle on a 1D axis.

    Arc height ∝ |end - start|; line width ∝ weight. Positive weights drawn
    above the axis (`color_above`), negatives below. Used for genomic loops,
    peak-to-gene links, regulatory edges.
    """
    starts = np.asarray(starts, dtype=float)
    ends = np.asarray(ends, dtype=float)
    weights = np.asarray(weights, dtype=float)
    if starts.size == 0:
        raise ValueError("arc requires at least one segment")
    if not (starts.size == ends.size == weights.size):
        raise ValueError("starts/ends/weights must have equal length")

    fig, ax = plt.subplots(figsize=figsize)
    abs_w = np.abs(weights)
    max_w = float(abs_w.max()) or 1.0
    for s, e, w in zip(starts, ends, weights):
        mid = (s + e) / 2.0
        radius = abs(e - s) / 2.0
        theta = np.linspace(0.0, np.pi, 64)
        x = mid + radius * np.cos(theta)
        y = radius * np.sin(theta)
        if w < 0:
            y = -y
        lw = 0.5 + 2.5 * (abs(w) / max_w)
        color = color_above if w >= 0 else color_below
        ax.plot(x, y, color=color, linewidth=lw, alpha=0.7)
    span = max(float(np.max(ends)), 1.0) - min(float(np.min(starts)), 0.0)
    ax.set_xlim(float(np.min(starts)) - 0.02 * span, float(np.max(ends)) + 0.02 * span)
    y_lim = float(np.max(np.abs(ends - starts))) / 2.0 * 1.1
    ax.set_ylim(-y_lim if np.any(weights < 0) else 0.0, y_lim)
    ax.axhline(0.0, color="#000000", linewidth=0.8)
    ax.set_yticks([])
    ax.set_xlabel(xlabel)
    ax.set_title(title)
    ax.spines["top"].set_visible(False)
    ax.spines["right"].set_visible(False)
    ax.spines["left"].set_visible(False)
    return savefig(fig, out)


def sankey(
    flows: "Sequence[Tuple[str, str, float]]",
    *,
    title: str,
    out: Path,
    figsize: Tuple[float, float] = (8.0, 5.0),
) -> Optional[Path]:
    """Two-column sankey/flow diagram.

    `flows` is a list of (source, target, magnitude) triples. Sources are
    drawn as bars on the left, targets on the right, with curved ribbons
    between them whose width is proportional to magnitude.

    Deterministic ordering: sources in input-encounter order, targets in
    input-encounter order. Use this for QC views (barcode pairing,
    cell-type retention, etc.).
    """
    sources: List[str] = []
    targets: List[str] = []
    for s, t, _ in flows:
        if s not in sources:
            sources.append(s)
        if t not in targets:
            targets.append(t)
    if not sources or not targets:
        raise ValueError("sankey requires at least one flow")

    src_totals = {s: 0.0 for s in sources}
    tgt_totals = {t: 0.0 for t in targets}
    for s, t, m in flows:
        src_totals[s] += float(m)
        tgt_totals[t] += float(m)
    total = max(sum(src_totals.values()), 1e-9)

    src_y = {}
    cum = 0.0
    for s in sources:
        h = src_totals[s] / total
        src_y[s] = (cum, cum + h)
        cum += h + 0.02
    tgt_y = {}
    cum = 0.0
    for t in targets:
        h = tgt_totals[t] / total
        tgt_y[t] = (cum, cum + h)
        cum += h + 0.02

    fig, ax = plt.subplots(figsize=figsize)
    palette = categorical_palette(len(sources), name="sankey.sources")
    src_color = dict(zip(sources, palette))
    src_offset = {s: src_y[s][0] for s in sources}
    tgt_offset = {t: tgt_y[t][0] for t in targets}
    for s, t, m in flows:
        h = float(m) / total
        y0a = src_offset[s]
        y0b = y0a + h
        src_offset[s] = y0b + 0.0001
        y1a = tgt_offset[t]
        y1b = y1a + h
        tgt_offset[t] = y1b + 0.0001
        xs = np.linspace(0.1, 0.9, 32)
        u = (xs - 0.1) / 0.8
        s_curve = u * u * (3 - 2 * u)
        ys_a = y0a + (y1a - y0a) * s_curve
        ys_b = y0b + (y1b - y0b) * s_curve
        ax.fill_between(xs, ys_a, ys_b, color=src_color[s], alpha=0.4, linewidth=0)
    for s in sources:
        y0, y1 = src_y[s]
        ax.add_patch(plt.Rectangle((0.05, y0), 0.04, y1 - y0, color=src_color[s]))
        ax.text(0.03, (y0 + y1) / 2, s, ha="right", va="center", fontsize=9)
    for t in targets:
        y0, y1 = tgt_y[t]
        ax.add_patch(plt.Rectangle((0.91, y0), 0.04, y1 - y0, color="#666666"))
        ax.text(0.97, (y0 + y1) / 2, t, ha="left", va="center", fontsize=9)
    ax.set_xlim(0.0, 1.0)
    ax.set_ylim(-0.05, 1.05)
    ax.set_aspect("auto")
    ax.set_axis_off()
    ax.set_title(title)
    return savefig(fig, out)


# ---------------------------------------------------------------------------
# Phase F (plan §S12.1) — variant + GWAS primitives.
#
# Every helper here takes a pandas DataFrame as the primary input (or any
# dict-like with the same column keys, so callers without pandas still
# work). Columns are extracted by name via `_col`, which raises a
# KeyError-derived ValueError naming the missing column so the dispatcher
# surfaces it as a per-figure skip rather than killing the whole stage.
# ---------------------------------------------------------------------------


def _col(frame: Any, name: str, *, optional: bool = False) -> Optional[np.ndarray]:
    """Pull a column from a DataFrame / dict / structured-array as a 1-D
    numpy array. Raises ValueError naming the missing column so the
    `generate()` dispatcher's exception handler surfaces a useful message
    instead of an opaque KeyError trace. With `optional=True`, missing
    columns return None.
    """
    try:
        return np.asarray(frame[name])
    except (KeyError, ValueError, TypeError, IndexError):
        if optional:
            return None
        raise ValueError(f"required column '{name}' not present in input frame")


def _optional_float_col(values: Optional[np.ndarray]) -> Optional[np.ndarray]:
    """Convert an optional numeric column, treating blanks as missing.

    Optional overlays such as forecast actuals often contain future rows
    where the observed value is intentionally absent. Those blanks should
    not prevent rendering the forecast interval and point estimate.
    """
    if values is None:
        return None
    converted: List[float] = []
    for value in np.asarray(values, dtype=object):
        if value is None:
            converted.append(float("nan"))
            continue
        text = str(value).strip()
        if not text or text.lower() in {"nan", "na", "none", "null"}:
            converted.append(float("nan"))
            continue
        try:
            converted.append(float(value))
        except (TypeError, ValueError):
            converted.append(float("nan"))
    return np.asarray(converted, dtype=float)


_CHROM_NUMERIC_ORDER: Tuple[str, ...] = tuple(
    [str(i) for i in range(1, 23)] + ["X", "Y", "MT", "M"]
)


def _chrom_sort_key(chrom: str) -> Tuple[int, str]:
    """Numeric chromosomes sort first (1..22), then X, Y, MT, then any
    leftover named scaffolds in lexicographic order. Returns a tuple so
    sorted()'s stability does the rest.
    """
    s = str(chrom)
    if s.isdigit():
        return (int(s), s)
    upper = s.upper()
    fixed = {"X": (23, "X"), "Y": (24, "Y"), "MT": (25, "MT"), "M": (25, "MT")}
    if upper in fixed:
        return fixed[upper]
    return (1000, s)


def manhattan(
    frame: Any,
    *,
    title: str,
    out: Path,
    sig_threshold: Optional[float] = None,
    suggestive_threshold: Optional[float] = None,
    label_top_n: int = 5,
    chrom_order: Optional[Sequence[str]] = None,
    figsize: Tuple[float, float] = (10.0, 4.0),
) -> Path:
    """Manhattan plot of GWAS / variant-calling −log10(p) by genomic
    position. One line per chromosome; alternating Wong-derived shading
    so chromosome boundaries read at a glance.

    Required columns (DataFrame or dict-like):
        - ``chrom``    chromosome label (str or int; 1..22 / X / Y / MT)
        - ``pos``      base-pair position (int)
        - ``pvalue``   raw p-value, OR
        - ``neg_log10_p`` if you've already transformed
    Optional columns:
        - ``gene``     label drawn next to the top-N most-significant
                       variants (greedy collision-avoidance)

    Plan reference: §S12.1 (Phase F variant + GWAS primitives).
    """
    chrom = _col(frame, "chrom").astype(object)
    pos = _col(frame, "pos").astype(np.int64)
    nlogp = _col(frame, "neg_log10_p", optional=True)
    if nlogp is None:
        pvalue = _col(frame, "pvalue").astype(float)
        with np.errstate(divide="ignore", invalid="ignore"):
            nlogp = -np.log10(np.clip(pvalue, 1e-300, 1.0))
    else:
        nlogp = nlogp.astype(float)
    labels = _col(frame, "gene", optional=True)
    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")
    if sig_threshold is None:
        sig_threshold = -float(np.log10(5e-8))
    if suggestive_threshold is None:
        suggestive_threshold = -float(np.log10(1e-5))

    # Order chromosomes: explicit override wins; otherwise numeric-first.
    chroms_present = list(dict.fromkeys(str(c) for c in chrom))
    if chrom_order is None:
        ordered_chroms = sorted(chroms_present, key=_chrom_sort_key)
    else:
        ordered_chroms = [str(c) for c in chrom_order if str(c) in chroms_present]

    # Virtualize positions onto a continuous axis so the plot doesn't
    # leave gaps between chromosomes.
    chrom_str = np.asarray([str(c) for c in chrom])
    chrom_offsets: Dict[str, float] = {}
    chrom_midpoints: List[float] = []
    cursor = 0.0
    virtual_x = np.zeros(len(pos), dtype=float)
    palette_alt = (palette.get("non_sig", "#999999"), "#444444")
    point_colors = np.full(len(pos), palette_alt[0], dtype=object)
    spacing = 0.0
    for idx, c in enumerate(ordered_chroms):
        mask = chrom_str == c
        if not mask.any():
            continue
        chrom_pos = pos[mask].astype(float)
        chrom_offsets[c] = cursor
        virtual_x[mask] = cursor + chrom_pos
        # Alternate per-chrom shading.
        point_colors[mask] = palette_alt[idx % 2]
        chrom_midpoints.append(cursor + (float(chrom_pos.max()) + float(chrom_pos.min())) / 2.0)
        cursor += float(chrom_pos.max()) + 1.0
        # Cosmetic spacer between chromosomes for visual separation.
        spacing = max(spacing, float(chrom_pos.max()) * 0.005)

    rasterize = (
        len(pos) > THEME.get("output", {}).get("rasterize_threshold_n", 50000)
    )
    fig, ax = plt.subplots(figsize=figsize)
    ax.scatter(
        virtual_x, nlogp,
        c=point_colors, s=4, alpha=0.75, linewidths=0,
        rasterized=rasterize,
    )
    ax.axhline(suggestive_threshold, color="#444444", linestyle="--", linewidth=0.5)
    ax.axhline(sig_threshold, color=sig_color, linestyle="-", linewidth=0.6)
    ax.set_xticks(chrom_midpoints)
    ax.set_xticklabels(ordered_chroms)
    ax.set_xlabel("chromosome")
    ax.set_ylabel("−log₁₀ p-value")
    ax.set_title(f"{title}\nn = {len(pos)} variants")
    ax.set_xlim(-spacing, cursor + spacing)
    ymax = float(np.nanmax(nlogp)) if len(nlogp) else 1.0
    ax.set_ylim(0, max(ymax * 1.05, sig_threshold * 1.1))

    # Top-N labels via the volcano helper so collision avoidance is shared.
    if labels is not None and label_top_n > 0:
        order = np.argsort(nlogp)[::-1][:label_top_n]
        coords = [(float(virtual_x[i]), float(nlogp[i])) for i in order]
        x_range = (cursor + spacing) or 1.0
        y_range = (ymax * 1.05) or 1.0
        placements = _greedy_label_placement(
            coords, width=x_range, height=y_range, min_dist=0.04
        )
        for i, place in zip(order, placements):
            if place is None:
                continue
            ax.annotate(
                str(labels[i]),
                xy=(virtual_x[i], nlogp[i]),
                xytext=place,
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#222222",
                arrowprops=(
                    dict(arrowstyle="-", color="#888888", linewidth=0.3)
                    if place != (float(virtual_x[i]), float(nlogp[i]))
                    else None
                ),
            )
    return savefig(fig, out)


def qq(
    frame: Any,
    *,
    title: str,
    out: Path,
    ci_level: float = 0.95,
    annotate_lambda_gc: bool = True,
    figsize: Tuple[float, float] = (5.0, 5.0),
) -> Path:
    """QQ plot of observed −log10(p) vs expected uniform quantiles, with
    a theoretical confidence band and the genomic-control inflation
    factor (λ_GC) annotated when ``annotate_lambda_gc`` is set.

    Required columns:
        - ``pvalue``  raw p-value, OR ``neg_log10_p`` if pre-transformed.

    Plan reference: §S12.1.
    """
    nlogp = _col(frame, "neg_log10_p", optional=True)
    if nlogp is None:
        pvalue = _col(frame, "pvalue").astype(float)
        observed = np.sort(np.clip(pvalue, 1e-300, 1.0))
        nlogp_sorted = -np.log10(observed)
    else:
        observed = 10.0 ** -np.asarray(nlogp).astype(float)
        observed = np.sort(np.clip(observed, 1e-300, 1.0))
        nlogp_sorted = -np.log10(observed)
    n = len(observed)
    if n == 0:
        fig, ax = plt.subplots(figsize=figsize)
        ax.set_title(title)
        return savefig(fig, out)
    expected = (np.arange(1, n + 1) - 0.5) / n
    expected_log = -np.log10(expected)

    # Theoretical CI band — Beta(k, n-k+1) quantile bounds per Stephens 2010.
    band_lo = band_hi = None
    try:
        from scipy.stats import beta
        alpha = 1.0 - ci_level
        k = np.arange(1, n + 1)
        band_lo = -np.log10(beta.ppf(1.0 - alpha / 2.0, k, n - k + 1))
        band_hi = -np.log10(beta.ppf(alpha / 2.0, k, n - k + 1))
    except ImportError:  # pragma: no cover
        pass

    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")

    fig, ax = plt.subplots(figsize=figsize)
    if band_lo is not None and band_hi is not None:
        ax.fill_between(expected_log, band_lo, band_hi,
                        color="#cccccc", alpha=0.5, linewidth=0,
                        label=f"{int(ci_level * 100)}% CI")
    ax.plot([0, expected_log.max()], [0, expected_log.max()],
            color="#444444", linestyle="--", linewidth=0.5)
    ax.scatter(expected_log, nlogp_sorted, s=4, color=sig_color,
               alpha=0.75, linewidths=0)

    if annotate_lambda_gc:
        try:
            from scipy.stats import chi2
            chi_sq = chi2.isf(observed, df=1)
            lam = float(np.median(chi_sq) / chi2.ppf(0.5, df=1))
            ax.text(
                0.02, 0.97, f"λ_GC = {lam:.3f}",
                transform=ax.transAxes, ha="left", va="top",
                color="#222222",
            )
        except ImportError:  # pragma: no cover
            pass

    ax.set_xlabel("expected −log₁₀ p")
    ax.set_ylabel("observed −log₁₀ p")
    ax.set_title(f"{title}\nn = {n}")
    return savefig(fig, out)


def miami(
    top: Any,
    bottom: Any,
    *,
    title: str,
    out: Path,
    top_label: str = "top",
    bottom_label: str = "bottom",
    sig_threshold: Optional[float] = None,
    figsize: Tuple[float, float] = (10.0, 6.0),
) -> Path:
    """Miami plot — two mirrored Manhattan panels for paired phenotypes
    (e.g. case-vs-control, trait-A vs trait-B). Uses the same per-chrom
    virtualization + shading as ``manhattan``; the bottom panel inverts
    its y-axis so significant signals from both traits surface together.

    Required columns (in both ``top`` and ``bottom``):
        - ``chrom``, ``pos``, ``pvalue`` (or ``neg_log10_p``)
    Optional columns: ``gene``.

    Plan reference: §S12.1.
    """
    if sig_threshold is None:
        sig_threshold = -float(np.log10(5e-8))

    def _prep(f: Any) -> Tuple[np.ndarray, np.ndarray, np.ndarray]:
        chrom = _col(f, "chrom").astype(object)
        pos = _col(f, "pos").astype(np.int64)
        nlogp = _col(f, "neg_log10_p", optional=True)
        if nlogp is None:
            pv = _col(f, "pvalue").astype(float)
            nlogp = -np.log10(np.clip(pv, 1e-300, 1.0))
        return chrom, pos, np.asarray(nlogp).astype(float)

    top_c, top_p, top_n = _prep(top)
    bot_c, bot_p, bot_n = _prep(bottom)

    chroms_present = list(dict.fromkeys(
        [str(c) for c in top_c] + [str(c) for c in bot_c]
    ))
    ordered = sorted(chroms_present, key=_chrom_sort_key)

    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")
    palette_alt = (palette.get("non_sig", "#999999"), "#444444")

    def _virtualize(
        c: np.ndarray, p: np.ndarray
    ) -> Tuple[np.ndarray, np.ndarray, List[float], float]:
        cs = np.asarray([str(x) for x in c])
        cursor = 0.0
        virt = np.zeros(len(p), dtype=float)
        colors = np.full(len(p), palette_alt[0], dtype=object)
        midpoints: List[float] = []
        for idx, ch in enumerate(ordered):
            mask = cs == ch
            if not mask.any():
                cursor += 1.0
                midpoints.append(cursor)
                continue
            cp = p[mask].astype(float)
            virt[mask] = cursor + cp
            colors[mask] = palette_alt[idx % 2]
            midpoints.append(cursor + (float(cp.max()) + float(cp.min())) / 2.0)
            cursor += float(cp.max()) + 1.0
        return virt, colors, midpoints, cursor

    vt, ct, mt, cursor = _virtualize(top_c, top_p)
    vb, cb, _mb, _cb = _virtualize(bot_c, bot_p)

    fig, (ax_top, ax_bot) = plt.subplots(
        2, 1, figsize=figsize, sharex=True, gridspec_kw={"hspace": 0.05}
    )
    ax_top.scatter(vt, top_n, c=ct, s=4, alpha=0.75, linewidths=0)
    ax_top.axhline(sig_threshold, color=sig_color, linestyle="-", linewidth=0.6)
    ax_top.set_ylabel(f"{top_label}\n−log₁₀ p")
    ax_top.set_ylim(0, max(float(np.nanmax(top_n)) * 1.05 if len(top_n) else 1.0,
                           sig_threshold * 1.1))

    ax_bot.scatter(vb, bot_n, c=cb, s=4, alpha=0.75, linewidths=0)
    ax_bot.axhline(sig_threshold, color=sig_color, linestyle="-", linewidth=0.6)
    ax_bot.set_ylabel(f"{bottom_label}\n−log₁₀ p")
    ax_bot.invert_yaxis()
    ax_bot.set_ylim(max(float(np.nanmax(bot_n)) * 1.05 if len(bot_n) else 1.0,
                        sig_threshold * 1.1), 0)
    ax_bot.set_xticks(mt)
    ax_bot.set_xticklabels(ordered)
    ax_bot.set_xlabel("chromosome")
    ax_top.set_title(title)
    return savefig(fig, out)


def locus_zoom(
    frame: Any,
    *,
    title: str,
    out: Path,
    lead_index: Optional[int] = None,
    figsize: Tuple[float, float] = (8.0, 6.0),
) -> Path:
    """Locus-zoom regional plot — points colored by LD-r² to a lead
    variant, with an optional gene-track strip below. Lead variant is
    drawn as a diamond. LD bins are fixed at [0, 0.2, 0.4, 0.6, 0.8, 1.0]
    so two locus-zooms in a paper read against the same scale.

    Required columns:
        - ``pos``           base-pair position (int)
        - ``neg_log10_p``   transformed p-value (float)
    Optional columns:
        - ``ld``            LD r² to the lead variant (0..1)
        - ``rsid``          variant id; used to label the lead point

    ``lead_index`` defaults to the row with the highest ``neg_log10_p``.

    Plan reference: §S12.1.
    """
    pos = _col(frame, "pos").astype(np.int64)
    nlogp = _col(frame, "neg_log10_p").astype(float)
    ld = _col(frame, "ld", optional=True)
    rsid = _col(frame, "rsid", optional=True)
    if lead_index is None:
        if len(nlogp) == 0:
            lead_index = 0
        else:
            lead_index = int(np.argmax(nlogp))
    fig, ax = plt.subplots(figsize=figsize)
    if ld is not None:
        ld_arr = np.clip(np.asarray(ld, dtype=float), 0.0, 1.0)
        bins = [0.0, 0.2, 0.4, 0.6, 0.8, 1.0001]
        # Five-color LD bins drawn from the wong palette extension.
        bin_colors = ["#0072B2", "#56B4E9", "#009E73", "#E69F00", "#D55E00"]
        bin_labels = ["r² < 0.2", "0.2 ≤ r² < 0.4", "0.4 ≤ r² < 0.6",
                      "0.6 ≤ r² < 0.8", "r² ≥ 0.8"]
        idx = np.digitize(ld_arr, bins) - 1
        idx = np.clip(idx, 0, len(bin_colors) - 1)
        for bi, (color, label) in enumerate(zip(bin_colors, bin_labels)):
            mask = idx == bi
            if mask.any():
                ax.scatter(pos[mask], nlogp[mask], s=10,
                           color=color, alpha=0.85, linewidths=0,
                           label=label)
        ax.legend(loc="upper left", frameon=False, fontsize=6)
    else:
        ax.scatter(pos, nlogp, s=8, color=THEME.get("palette", {}).get("non_sig", "#999999"),
                   alpha=0.75, linewidths=0)
    if 0 <= int(lead_index) < len(pos):
        ax.scatter([pos[int(lead_index)]], [nlogp[int(lead_index)]],
                   marker="D", s=60, color="#882E72", edgecolor="white",
                   linewidth=0.6, zorder=5)
        if rsid is not None:
            ax.annotate(
                str(rsid[int(lead_index)]),
                xy=(pos[int(lead_index)], nlogp[int(lead_index)]),
                xytext=(8, 8),
                textcoords="offset points",
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#222222",
            )
    ax.set_xlabel("position (bp)")
    ax.set_ylabel("−log₁₀ p")
    ax.set_title(title)
    return savefig(fig, out)


def credible_set_track(
    frame: Any,
    *,
    title: str,
    out: Path,
    pp_threshold: float = 0.95,
    figsize: Tuple[float, float] = (8.0, 4.0),
) -> Path:
    """Stem plot of per-variant posterior inclusion probability (PIP /
    posterior) with credible-set membership shaded.

    Required columns:
        - ``pos``        base-pair position (int)
        - ``posterior``  posterior probability of being a causal variant (0..1)
    Optional columns:
        - ``credible_set``  bool — whether the variant is in the
                            cumulative ``pp_threshold`` credible set
                            (computed if absent: cumulative sort).

    Plan reference: §S12.1.
    """
    pos = _col(frame, "pos").astype(np.int64)
    post = _col(frame, "posterior").astype(float)
    cs_col = _col(frame, "credible_set", optional=True)
    if cs_col is None:
        order = np.argsort(post)[::-1]
        cumulative = np.cumsum(post[order])
        cutoff = int(np.searchsorted(cumulative, pp_threshold)) + 1
        in_cs = np.zeros(len(post), dtype=bool)
        in_cs[order[:cutoff]] = True
    else:
        in_cs = np.asarray(cs_col, dtype=bool)

    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")
    fig, ax = plt.subplots(figsize=figsize)
    if in_cs.any():
        # Shade the convex hull of credible-set positions for visual
        # grouping; use a translucent version of the sig color.
        cs_pos = pos[in_cs]
        ax.axvspan(float(cs_pos.min()), float(cs_pos.max()),
                   color=sig_color, alpha=0.12, linewidth=0,
                   label=f"{int(pp_threshold * 100)}% credible set")
        ax.legend(loc="upper right", frameon=False, fontsize=6)
    ax.vlines(pos, 0, post,
              colors=np.where(in_cs, sig_color, "#999999"),
              linewidth=0.6)
    ax.scatter(pos, post, s=10,
               color=np.where(in_cs, sig_color, "#999999"),
               edgecolor="white", linewidth=0.3, zorder=3)
    ax.set_xlabel("position (bp)")
    ax.set_ylabel("posterior probability")
    ax.set_ylim(0, max(float(np.nanmax(post)) * 1.1 if len(post) else 1.0, 0.05))
    ax.set_title(f"{title}\n{int(in_cs.sum())} / {len(post)} in credible set")
    return savefig(fig, out)


def coloc_pp_panel(
    frame: Any,
    *,
    title: str,
    out: Path,
    figsize: Tuple[float, float] = (10.0, 5.0),
) -> Path:
    """Five-panel summary of colocalization posterior probabilities
    (PP.H0 .. PP.H4) per locus / region, drawn as horizontal bars so
    region labels stay legible. Bars sum to ≤ 1 per region; the H4
    panel uses the theme ``sig_up`` color since high H4 = strong
    colocalization (the headline result).

    Required columns:
        - ``region``  locus / gene-region label
        - ``pp_h0``, ``pp_h1``, ``pp_h2``, ``pp_h3``, ``pp_h4``
          posterior probabilities for each of the five coloc hypotheses

    Plan reference: §S12.1.
    """
    region = _col(frame, "region").astype(object)
    cols = ["pp_h0", "pp_h1", "pp_h2", "pp_h3", "pp_h4"]
    arrs = [_col(frame, c).astype(float) for c in cols]
    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    non_sig = palette.get("non_sig", "#999999")
    # Five-panel colors: greys for H0..H3, sig color for H4.
    colors = [non_sig, "#bbbbbb", "#999999", "#666666", sig_up]
    titles = ["PP.H0\nneither", "PP.H1\ntrait1 only",
              "PP.H2\ntrait2 only", "PP.H3\nboth, distinct",
              "PP.H4\ncoloc"]

    fig, axes = plt.subplots(1, 5, figsize=figsize, sharey=True,
                             gridspec_kw={"wspace": 0.05})
    y = np.arange(len(region))
    for ax, vals, color, sub in zip(axes, arrs, colors, titles):
        ax.barh(y, vals, color=color, edgecolor="#333333", linewidth=0.2)
        ax.set_xlim(0, 1)
        ax.set_xlabel("PP")
        ax.set_title(sub, fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
        ax.axvline(0.5, color="#444444", linestyle="--", linewidth=0.4)
    axes[0].set_yticks(y)
    axes[0].set_yticklabels([str(r) for r in region])
    axes[0].invert_yaxis()
    fig.suptitle(f"{title}\n{len(region)} regions")
    return savefig(fig, out)


def forest(
    frame: Any,
    *,
    title: str,
    out: Path,
    null_value: float = 0.0,
    xlabel: str = "effect size (95% CI)",
    figsize: Tuple[float, float] = (7.0, 6.0),
) -> Path:
    """Forest plot of point estimate ± 95% CI per study / contrast.
    Each row is one study; the dashed vertical line marks the null
    effect (defaults to 0 — pass ``null_value=1.0`` for odds-ratio
    style x-axes).

    Required columns:
        - ``label``     study or contrast label
        - ``effect``    point estimate (β / log-OR / log-HR)
        - ``ci_lo``     lower 95% CI bound
        - ``ci_hi``     upper 95% CI bound
    Optional columns:
        - ``weight``    inverse-variance weight; controls marker size
                        (proportional). Falls back to constant size.

    Plan reference: §S12.1.
    """
    label = _col(frame, "label").astype(object)
    effect = _col(frame, "effect").astype(float)
    lo = _col(frame, "ci_lo").astype(float)
    hi = _col(frame, "ci_hi").astype(float)
    weight = _col(frame, "weight", optional=True)
    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    sig_down = palette.get("sig_down", "#0072B2")
    non_sig = palette.get("non_sig", "#999999")

    # Significance: CI excludes the null on the same side as the effect.
    sig_pos = (lo > null_value) & (effect >= null_value)
    sig_neg = (hi < null_value) & (effect <= null_value)
    colors = np.where(sig_pos, sig_up, np.where(sig_neg, sig_down, non_sig))

    if weight is not None:
        w = np.asarray(weight, dtype=float)
        wmax = float(np.nanmax(w)) if len(w) else 1.0
        sizes = 30.0 + 80.0 * (w / (wmax or 1.0))
    else:
        sizes = np.full(len(effect), 50.0)

    fig, ax = plt.subplots(figsize=figsize)
    y = np.arange(len(label))
    # Horizontal CI lines first so the marker sits on top.
    for i in range(len(label)):
        ax.plot([lo[i], hi[i]], [y[i], y[i]],
                color=colors[i], linewidth=1.2, alpha=0.8)
    ax.scatter(effect, y, s=sizes, c=colors,
               edgecolor="#333333", linewidth=0.4, zorder=3)
    ax.axvline(null_value, color="#444444", linestyle="--", linewidth=0.5)
    ax.set_yticks(y)
    ax.set_yticklabels([str(s) for s in label])
    ax.invert_yaxis()
    ax.set_xlabel(xlabel)
    ax.set_title(f"{title}\nn = {len(label)} studies")
    n_sig = int(sig_pos.sum() + sig_neg.sum())
    ax.text(0.99, 0.01, f"{n_sig} / {len(label)} CI excludes null",
            transform=ax.transAxes, ha="right", va="bottom",
            color="#666666")
    return savefig(fig, out)


# ---------------------------------------------------------------------------
# Phase G (plan §S12.4-S12.6): clinical statistical figures.
#
# Five primitives cover the SAP-driven figure roster shared across the
# clinical-trial-analysis taxonomy: Kaplan-Meier survival curves,
# CONSORT participant-flow diagrams, competing-risks cumulative-incidence
# functions, per-subject longitudinal spaghetti, and adverse-event
# frequency stacked bars. Conventions match the Phase F primitives above:
# DataFrame primary input that degrades to dict-like via `_col`, theme
# applied through `apply_theme()` (already at module load), dual-format
# write through `savefig()` which stamps the provenance footer.
#
# Statistical helpers (Kaplan-Meier estimator, Aalen-Johansen cumulative
# incidence) are implemented inline rather than pulling lifelines /
# survival-package wrappers — these ship in matplotlib + numpy only.
# ---------------------------------------------------------------------------


def _km_estimator(
    time: np.ndarray, event: np.ndarray
) -> Tuple[np.ndarray, np.ndarray, np.ndarray]:
    """Kaplan-Meier survival estimator. Returns (unique event times,
    survival probability after each event time, number-at-risk before
    each event time). Censoring observations (event == 0) reduce the
    risk set without producing a step. Implemented inline so the
    primitive doesn't pull lifelines.
    """
    t = np.asarray(time, dtype=float)
    e = np.asarray(event, dtype=int)
    if len(t) == 0:
        return np.array([0.0]), np.array([1.0]), np.array([0])
    order = np.argsort(t, kind="stable")
    t = t[order]
    e = e[order]
    unique_event_times = np.unique(t[e == 1])
    if len(unique_event_times) == 0:
        return np.array([0.0]), np.array([1.0]), np.array([len(t)])
    surv = 1.0
    surv_curve: List[float] = []
    at_risk: List[int] = []
    for ti in unique_event_times:
        n_at_risk = int(np.sum(t >= ti))
        n_events = int(np.sum((t == ti) & (e == 1)))
        if n_at_risk > 0:
            surv *= 1.0 - n_events / n_at_risk
        surv_curve.append(surv)
        at_risk.append(n_at_risk)
    return unique_event_times, np.asarray(surv_curve), np.asarray(at_risk)


def kaplan_meier(
    frame: Any,
    *,
    title: str,
    out: Path,
    time_col: str = "time",
    event_col: str = "event",
    group_col: Optional[str] = None,
    show_at_risk_table: bool = True,
    figsize: Tuple[float, float] = (7.0, 5.0),
) -> Path:
    """Kaplan-Meier survival curve. Step plot per group with optional
    number-at-risk table beneath the axis. Uses an inline KM estimator
    so the primitive ships without lifelines / survival.

    Required columns:
        - ``time_col``   follow-up time (float; days, months, etc.)
        - ``event_col``  event indicator (0 = censored, 1 = event)
    Optional:
        - ``group_col``  stratifying group; one curve per level

    Plan reference: §S12.4 (clinical primary-endpoint survival).
    """
    time = _col(frame, time_col).astype(float)
    event = _col(frame, event_col).astype(int)
    group = _col(frame, group_col, optional=True) if group_col else None

    if group is None:
        groups: List[Tuple[Optional[str], np.ndarray]] = [(None, np.ones(len(time), dtype=bool))]
    else:
        group_arr = np.asarray(group, dtype=object)
        levels = list(dict.fromkeys(str(g) for g in group_arr))
        groups = [(lvl, np.asarray([str(g) == lvl for g in group_arr])) for lvl in levels]

    colors = categorical_palette(max(len(groups), 1))

    if show_at_risk_table and len(groups) > 0:
        fig, (ax, ax_table) = plt.subplots(
            2, 1, figsize=figsize, sharex=True,
            gridspec_kw={"height_ratios": [4.0, max(1.0, 0.4 * len(groups) + 0.5)],
                         "hspace": 0.05},
        )
    else:
        fig, ax = plt.subplots(figsize=figsize)
        ax_table = None

    tmax = float(np.nanmax(time)) if len(time) else 1.0
    grid = np.linspace(0.0, tmax, num=6)
    table_rows: List[Tuple[str, List[int]]] = []
    for (label, mask), color in zip(groups, colors):
        t_g = time[mask]
        e_g = event[mask]
        ev_times, surv, _at_risk = _km_estimator(t_g, e_g)
        # Step plot starting at S(0) = 1.
        x = np.concatenate(([0.0], ev_times))
        y = np.concatenate(([1.0], surv))
        ax.step(x, y, where="post", color=color, linewidth=1.2,
                label=str(label) if label is not None else None)
        # Tick marks at censoring times.
        cens = t_g[e_g == 0]
        if len(cens):
            cens_y = np.interp(cens, x, y, left=1.0, right=y[-1])
            ax.scatter(cens, cens_y, marker="|", color=color,
                       s=18, linewidths=0.6, zorder=3)
        # At-risk row at the table's grid points.
        row_counts = [int(np.sum(t_g >= g)) for g in grid]
        table_rows.append((str(label) if label is not None else "all", row_counts))

    ax.set_ylim(0, 1.05)
    ax.set_ylabel("survival probability S(t)")
    if group_col is not None:
        ax.legend(loc="upper right", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_title(f"{title}\nn = {len(time)}")

    if ax_table is not None and len(table_rows) > 0:
        ax_table.set_xlim(ax.get_xlim())
        ax_table.set_ylim(-0.5, len(table_rows) - 0.5)
        ax_table.set_yticks(range(len(table_rows)))
        ax_table.set_yticklabels([r[0] for r in table_rows])
        ax_table.invert_yaxis()
        for ri, (_lab, counts) in enumerate(table_rows):
            for ci, val in enumerate(counts):
                ax_table.text(grid[ci], ri, str(val),
                              ha="center", va="center",
                              fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
        ax_table.set_xlabel("time (at risk)")
        # Table grid has no spine clutter.
        for spine in ("top", "right", "left"):
            ax_table.spines[spine].set_visible(False)
        ax_table.tick_params(axis="y", length=0)
    else:
        ax.set_xlabel("time")
    return savefig(fig, out)


def consort_diagram(
    flow: Any,
    *,
    title: str,
    out: Path,
    figsize: Tuple[float, float] = (8.0, 8.0),
) -> Path:
    """CONSORT participant-flow diagram. Nodes are stages (Enrolled →
    Randomized → Allocated → Followed-up → Analyzed) with side branches
    for exclusions / withdrawals. ``flow`` is either a dict mapping
    stage label → count (with optional ``_excluded`` siblings keyed
    ``"<stage>_excluded"`` whose value is "<n> reason text") or any
    object exposing the same keys.

    Required keys (dict-like):
        - ``enrolled``      assessed for eligibility
        - ``randomized``    randomized to a treatment arm
        - ``allocated``     received allocated intervention
        - ``followed_up``   completed follow-up
        - ``analyzed``      included in primary analysis

    Optional sibling exclusions (each an "<n> <reason>" string):
        - ``enrolled_excluded``
        - ``randomized_excluded``
        - ``allocated_excluded``
        - ``followed_up_excluded``
        - ``analyzed_excluded``

    Plan reference: §S12.4 (clinical primary-endpoint flow).
    """
    # Coerce dict-like into a real dict for predictable key access.
    if hasattr(flow, "to_dict"):
        d = flow.to_dict()
    elif isinstance(flow, dict):
        d = dict(flow)
    else:
        try:
            d = dict(flow)
        except TypeError as exc:
            raise ValueError("consort_diagram: flow must be dict-like") from exc

    required = ["enrolled", "randomized", "allocated", "followed_up", "analyzed"]
    missing = [k for k in required if k not in d]
    if missing:
        raise ValueError(
            f"consort_diagram missing required key(s): {', '.join(missing)}"
        )

    palette = THEME.get("palette", {})
    box_color = "#ffffff"
    edge_color = "#333333"
    sig_color = palette.get("sig_up", "#D55E00")

    fig, ax = plt.subplots(figsize=figsize)
    ax.set_xlim(0, 10)
    ax.set_ylim(0, len(required) * 2 + 1)
    ax.invert_yaxis()
    ax.axis("off")

    box_w = 4.0
    box_h = 1.2
    cx = 3.0
    excl_x = cx + box_w + 0.5
    excl_w = 4.0

    for i, key in enumerate(required):
        cy = 1.0 + i * 2.0
        n = int(d[key])
        # Main box.
        ax.add_patch(plt.Rectangle(
            (cx, cy - box_h / 2.0), box_w, box_h,
            facecolor=box_color, edgecolor=edge_color, linewidth=0.8,
        ))
        ax.text(
            cx + box_w / 2.0, cy,
            f"{key.replace('_', ' ').title()}\n(n = {n})",
            ha="center", va="center",
            fontsize=THEME.get("fonts", {}).get("body_pt", 8),
        )
        # Down-arrow to next stage.
        if i + 1 < len(required):
            ax.annotate(
                "",
                xy=(cx + box_w / 2.0, cy + box_h / 2.0 + 0.7),
                xytext=(cx + box_w / 2.0, cy + box_h / 2.0),
                arrowprops=dict(arrowstyle="->", color=edge_color, linewidth=0.6),
            )
        # Side exclusion branch when provided.
        excl_key = f"{key}_excluded"
        if excl_key in d and d[excl_key]:
            text = str(d[excl_key])
            ax.add_patch(plt.Rectangle(
                (excl_x, cy - box_h / 2.0), excl_w, box_h,
                facecolor=box_color, edgecolor=sig_color, linewidth=0.6,
            ))
            ax.text(
                excl_x + excl_w / 2.0, cy,
                f"Excluded\n{text}",
                ha="center", va="center",
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#444444",
            )
            ax.annotate(
                "",
                xy=(excl_x, cy),
                xytext=(cx + box_w, cy),
                arrowprops=dict(arrowstyle="->", color=sig_color, linewidth=0.5),
            )
    ax.set_title(title)
    return savefig(fig, out)


def _aalen_johansen(
    time: np.ndarray, event: np.ndarray, cause: int
) -> Tuple[np.ndarray, np.ndarray]:
    """Aalen-Johansen estimator of the cumulative-incidence function for
    a single cause in the presence of competing risks. Returns (event
    times, CIF at each event time). ``event`` codes: 0 censored,
    >= 1 cause id; ``cause`` is the cause we're estimating. Implemented
    inline so the primitive ships without cmprsk / lifelines.
    """
    t = np.asarray(time, dtype=float)
    e = np.asarray(event, dtype=int)
    if len(t) == 0:
        return np.array([0.0]), np.array([0.0])
    order = np.argsort(t, kind="stable")
    t = t[order]
    e = e[order]
    # Overall survival estimator (any-cause failure decrements the risk pool).
    any_event_times = np.unique(t[e > 0])
    if len(any_event_times) == 0:
        return np.array([0.0]), np.array([0.0])
    surv = 1.0
    cif = 0.0
    cif_times: List[float] = []
    cif_vals: List[float] = []
    for ti in any_event_times:
        n_at_risk = int(np.sum(t >= ti))
        n_cause = int(np.sum((t == ti) & (e == cause)))
        n_any = int(np.sum((t == ti) & (e > 0)))
        if n_at_risk > 0:
            cif += surv * (n_cause / n_at_risk)
            surv *= 1.0 - n_any / n_at_risk
        cif_times.append(float(ti))
        cif_vals.append(cif)
    return np.asarray(cif_times), np.asarray(cif_vals)


def cumulative_incidence(
    frame: Any,
    *,
    title: str,
    out: Path,
    time_col: str = "time",
    event_col: str = "event",
    competing_col: Optional[str] = None,
    group_col: Optional[str] = None,
    figsize: Tuple[float, float] = (7.0, 5.0),
) -> Path:
    """Competing-risks cumulative-incidence function (CIF). Aalen-Johansen
    estimator drawn as a step plot per cause × group. Cause codes are
    pulled from ``event_col`` directly (1, 2, ...) — pass
    ``competing_col`` only when the cause code lives in a separate
    column from the binary event flag.

    Required columns:
        - ``time_col``   follow-up time (float)
        - ``event_col``  cause code (0 = censored; 1, 2, ... = cause id)
    Optional:
        - ``competing_col``  separate cause-id column when ``event_col``
                             is a binary 0/1 flag and the cause id lives
                             elsewhere; merged as ``cause = event * id``
        - ``group_col``      stratifying group; one curve per cause × group

    Plan reference: §S12.5 (clinical safety + competing-risks).
    """
    time = _col(frame, time_col).astype(float)
    event = _col(frame, event_col).astype(int)
    if competing_col is not None:
        comp = _col(frame, competing_col).astype(int)
        event = event * comp
    group = _col(frame, group_col, optional=True) if group_col else None

    causes = sorted(int(c) for c in np.unique(event) if int(c) > 0)
    if not causes:
        # Nothing to plot — emit an empty figure rather than crashing.
        fig, ax = plt.subplots(figsize=figsize)
        ax.set_xlim(0, 1)
        ax.set_ylim(0, 1)
        ax.set_title(f"{title}\nno events observed")
        ax.set_xlabel("time")
        ax.set_ylabel("cumulative incidence")
        return savefig(fig, out)

    if group is None:
        group_levels: List[Optional[str]] = [None]
        group_masks: List[np.ndarray] = [np.ones(len(time), dtype=bool)]
    else:
        group_arr = np.asarray(group, dtype=object)
        group_levels = list(dict.fromkeys(str(g) for g in group_arr))
        group_masks = [np.asarray([str(g) == lvl for g in group_arr])
                       for lvl in group_levels]

    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    sig_down = palette.get("sig_down", "#0072B2")
    cause_colors = [sig_up, sig_down] + list(categorical_palette(max(0, len(causes) - 2)))

    fig, ax = plt.subplots(figsize=figsize)
    ymax = 0.0
    linestyles = ["-", "--", ":", "-."]
    for ci, cause in enumerate(causes):
        color = cause_colors[ci % len(cause_colors)]
        for gi, (level, mask) in enumerate(zip(group_levels, group_masks)):
            ev_times, cif = _aalen_johansen(time[mask], event[mask], cause=cause)
            x = np.concatenate(([0.0], ev_times))
            y = np.concatenate(([0.0], cif))
            label_parts: List[str] = [f"cause {cause}"]
            if level is not None:
                label_parts.append(str(level))
            ax.step(
                x, y, where="post",
                color=color,
                linestyle=linestyles[gi % len(linestyles)],
                linewidth=1.2,
                label=" · ".join(label_parts),
            )
            ymax = max(ymax, float(np.nanmax(y)) if len(y) else 0.0)
    ax.set_xlabel("time")
    ax.set_ylabel("cumulative incidence")
    ax.set_ylim(0, max(0.05, ymax * 1.1))
    ax.legend(loc="upper left", frameon=False,
              fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_title(f"{title}\n{len(causes)} cause(s), n = {len(time)}")
    return savefig(fig, out)


def spaghetti(
    frame: Any,
    *,
    title: str,
    out: Path,
    id_col: str = "id",
    time_col: str = "time",
    value_col: str = "value",
    group_col: Optional[str] = None,
    show_mean: bool = True,
    figsize: Tuple[float, float] = (8.0, 5.0),
) -> Path:
    """Per-subject longitudinal trajectories ("spaghetti plot"). One thin
    line per subject, optionally colored by group, with the per-group
    mean overlaid as a thick line when ``show_mean`` is set.

    Required columns:
        - ``id_col``     subject id (str / int)
        - ``time_col``   measurement time (float)
        - ``value_col``  measured value (float)
    Optional:
        - ``group_col``  stratifying group; subjects share line color

    Plan reference: §S12.5 (clinical secondary-endpoint longitudinal).
    """
    ids = _col(frame, id_col).astype(object)
    time = _col(frame, time_col).astype(float)
    value = _col(frame, value_col).astype(float)
    group = _col(frame, group_col, optional=True) if group_col else None

    palette = THEME.get("palette", {})
    if group is None:
        group_levels: List[Optional[str]] = [None]
        group_arr = np.full(len(ids), None, dtype=object)
        group_masks = [np.ones(len(ids), dtype=bool)]
        line_color = palette.get("non_sig", "#999999")
        mean_colors = [palette.get("sig_up", "#D55E00")]
        line_colors = [line_color]
    else:
        group_arr = np.asarray(group, dtype=object)
        group_levels = list(dict.fromkeys(str(g) for g in group_arr))
        group_masks = [np.asarray([str(g) == lvl for g in group_arr])
                       for lvl in group_levels]
        line_colors = categorical_palette(max(len(group_levels), 1))
        mean_colors = line_colors

    fig, ax = plt.subplots(figsize=figsize)
    # Per-subject lines first so means draw on top.
    unique_ids = list(dict.fromkeys(str(i) for i in ids))
    for sid in unique_ids:
        mask = np.asarray([str(i) == sid for i in ids])
        if not mask.any():
            continue
        order = np.argsort(time[mask], kind="stable")
        # Color the subject by its (first) group assignment.
        if group_col is not None:
            level = str(group_arr[mask][0])
            gi = group_levels.index(level) if level in group_levels else 0
            color = line_colors[gi]
        else:
            color = line_colors[0]
        ax.plot(time[mask][order], value[mask][order],
                color=color, linewidth=0.4, alpha=0.45, zorder=2)

    if show_mean:
        for level, mask, color in zip(group_levels, group_masks, mean_colors):
            t_g = time[mask]
            v_g = value[mask]
            if len(t_g) == 0:
                continue
            # Mean trajectory at the unique observed times.
            unique_t = np.sort(np.unique(t_g))
            mean_v = np.array([float(np.nanmean(v_g[t_g == t])) for t in unique_t])
            ax.plot(
                unique_t, mean_v,
                color=color, linewidth=1.6, alpha=0.95,
                label=f"mean · {level}" if level is not None else "mean",
                zorder=3,
            )
        if group_col is not None:
            ax.legend(loc="best", frameon=False,
                      fontsize=THEME.get("fonts", {}).get("legend_pt", 7))

    ax.set_xlabel("time")
    ax.set_ylabel(value_col)
    ax.set_title(f"{title}\n{len(unique_ids)} subjects, n = {len(value)}")
    return savefig(fig, out)


def adverse_event_bar(
    frame: Any,
    *,
    title: str,
    out: Path,
    term_col: str = "term",
    count_col: str = "count",
    severity_col: Optional[str] = None,
    top_n: int = 20,
    horizontal: Optional[bool] = None,
    figsize: Tuple[float, float] = (8.0, 6.0),
) -> Path:
    """Adverse-event frequency bar — horizontal stacked bar of AE terms
    by count, optionally split by severity (Grade 1/2/3+) using a
    sequential greys-to-sig_up palette so severe events visually
    dominate. Top-``top_n`` terms by total count are kept; the rest
    are dropped silently.

    Required columns:
        - ``term_col``   AE term (MedDRA preferred term, etc.)
        - ``count_col``  count per term (or per term × severity row)
    Optional:
        - ``severity_col``  ordinal severity column; rows with the same
                            term + severity combine via sum

    Plan reference: §S12.6 (clinical safety summary).
    """
    term = _col(frame, term_col).astype(object)
    count = _col(frame, count_col).astype(float)
    severity = _col(frame, severity_col, optional=True) if severity_col else None

    # Aggregate by term (and severity when present).
    if severity is None:
        # term -> total count
        agg: Dict[str, float] = {}
        for tname, c in zip(term, count):
            agg[str(tname)] = agg.get(str(tname), 0.0) + float(c)
        terms_sorted = sorted(agg.items(), key=lambda kv: kv[1], reverse=True)[:top_n]
        labels = [k for k, _ in terms_sorted]
        totals = np.array([v for _, v in terms_sorted], dtype=float)
        sev_levels: List[str] = []
        seg_matrix = totals.reshape(-1, 1)
    else:
        sev_arr = np.asarray(severity, dtype=object)
        sev_levels = list(dict.fromkeys(str(s) for s in sev_arr))
        agg2: Dict[Tuple[str, str], float] = {}
        for tname, sname, c in zip(term, sev_arr, count):
            key = (str(tname), str(sname))
            agg2[key] = agg2.get(key, 0.0) + float(c)
        # Total per term for ranking + top-N.
        term_totals: Dict[str, float] = {}
        for (tname, _sname), v in agg2.items():
            term_totals[tname] = term_totals.get(tname, 0.0) + v
        terms_sorted = sorted(term_totals.items(), key=lambda kv: kv[1], reverse=True)[:top_n]
        labels = [k for k, _ in terms_sorted]
        seg_matrix = np.array([
            [agg2.get((label, lvl), 0.0) for lvl in sev_levels]
            for label in labels
        ])
        totals = seg_matrix.sum(axis=1)

    # Default to horizontal when many terms — readability over n>10.
    if horizontal is None:
        horizontal = len(labels) > 10

    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    if sev_levels:
        # Greys → sig color for severity ramp, last bucket = severe.
        n_sev = len(sev_levels)
        if n_sev == 1:
            sev_colors = [sig_up]
        else:
            ramp = ["#cccccc", "#888888", "#555555", sig_up]
            if n_sev <= len(ramp):
                sev_colors = ramp[-n_sev:]
            else:
                # Fall back to a categorical palette for unusually many levels.
                sev_colors = categorical_palette(n_sev)
    else:
        sev_colors = [sig_up]

    fig, ax = plt.subplots(figsize=figsize)
    if horizontal:
        y = np.arange(len(labels))
        offsets = np.zeros(len(labels))
        if sev_levels:
            for si, lvl in enumerate(sev_levels):
                vals = seg_matrix[:, si]
                ax.barh(y, vals, left=offsets,
                        color=sev_colors[si], edgecolor="#333333",
                        linewidth=0.2, label=str(lvl))
                offsets += vals
        else:
            ax.barh(y, totals, color=sev_colors[0],
                    edgecolor="#333333", linewidth=0.2)
        ax.set_yticks(y)
        ax.set_yticklabels([str(lbl) for lbl in labels])
        ax.invert_yaxis()
        ax.set_xlabel("count")
    else:
        x = np.arange(len(labels))
        offsets = np.zeros(len(labels))
        if sev_levels:
            for si, lvl in enumerate(sev_levels):
                vals = seg_matrix[:, si]
                ax.bar(x, vals, bottom=offsets,
                       color=sev_colors[si], edgecolor="#333333",
                       linewidth=0.2, label=str(lvl))
                offsets += vals
        else:
            ax.bar(x, totals, color=sev_colors[0],
                   edgecolor="#333333", linewidth=0.2)
        ax.set_xticks(x)
        ax.set_xticklabels([str(lbl) for lbl in labels], rotation=45, ha="right")
        ax.set_ylabel("count")
    if sev_levels:
        ax.legend(title="severity", loc="best", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_title(f"{title}\n{len(labels)} term(s), {int(totals.sum())} events")
    return savefig(fig, out)


# ---------------------------------------------------------------------------
# Phase H (plan §S13.1-§S13.3) — sequencing/ChIP/ATAC, long-read RNA-seq,
# proteomics primitives.
#
# Conventions match Phase F/G above: DataFrame primary input that degrades
# to dict-like via `_col`, theme already applied at module load, dual-format
# write through `savefig()` which stamps the provenance footer. No new
# runtime deps — every helper renders with matplotlib + numpy only.
# ---------------------------------------------------------------------------


def profile_pileup(
    frame: Any,
    *,
    title: str,
    out: Path,
    position_col: str = "position",
    signal_col: str = "signal",
    group_col: Optional[str] = None,
    figsize: Tuple[float, float] = (7.0, 4.0),
) -> Path:
    """ChIP/ATAC signal pileup around peak centers — mean signal as a
    function of distance from peak summit (or any reference position).
    Multiple groups (e.g. different antibodies, conditions) draw as
    separate lines on a shared axis with a vertical reference at zero.

    Required columns:
        - ``position_col``  signed distance from peak center (bp; int/float)
        - ``signal_col``    coverage / fold-enrichment / RPKM signal
    Optional:
        - ``group_col``     stratifying group; one curve per group

    Plan reference: §S13.1.
    """
    pos = _col(frame, position_col).astype(float)
    sig = _col(frame, signal_col).astype(float)
    group = _col(frame, group_col, optional=True) if group_col else None

    palette = THEME.get("palette", {})
    fig, ax = plt.subplots(figsize=figsize)
    if group is None:
        # Mean per unique position; falls through cleanly with one row per pos.
        unique_pos = np.sort(np.unique(pos))
        mean_sig = np.array(
            [float(np.nanmean(sig[pos == p])) for p in unique_pos]
        )
        ax.plot(
            unique_pos, mean_sig,
            color=palette.get("sig_up", "#D55E00"),
            linewidth=1.2,
        )
    else:
        group_arr = np.asarray(group, dtype=object)
        levels = list(dict.fromkeys(str(g) for g in group_arr))
        colors = categorical_palette(max(len(levels), 1))
        for level, color in zip(levels, colors):
            mask = np.asarray([str(g) == level for g in group_arr])
            if not mask.any():
                continue
            unique_pos = np.sort(np.unique(pos[mask]))
            mean_sig = np.array([
                float(np.nanmean(sig[mask & (pos == p)])) for p in unique_pos
            ])
            ax.plot(unique_pos, mean_sig,
                    color=color, linewidth=1.2, label=str(level))
        ax.legend(loc="best", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.axvline(0.0, color="#444444", linestyle="--", linewidth=0.4)
    ax.set_xlabel("distance from peak center (bp)")
    ax.set_ylabel("mean signal")
    n_unique = int(len(np.unique(pos)))
    ax.set_title(f"{title}\nn = {len(pos)} rows, {n_unique} positions")
    return savefig(fig, out)


def coverage_track(
    frame: Any,
    *,
    title: str,
    out: Path,
    chrom_col: str = "chrom",
    pos_col: str = "pos",
    depth_col: str = "depth",
    region: Optional[Tuple[str, int, int]] = None,
    figsize: Tuple[float, float] = (10.0, 3.0),
) -> Path:
    """IGV-style coverage track — a shaded depth-of-coverage line over
    genomic position. ``region``, when supplied, is ``(chrom, start, end)``
    and clips the displayed window; otherwise the full input is shown.

    Required columns:
        - ``chrom_col``  chromosome label
        - ``pos_col``    base-pair position (int)
        - ``depth_col``  coverage depth (int/float)

    Plan reference: §S13.1.
    """
    chrom = _col(frame, chrom_col).astype(object)
    pos = _col(frame, pos_col).astype(np.int64)
    depth = _col(frame, depth_col).astype(float)

    chrom_str = np.asarray([str(c) for c in chrom])
    if region is not None:
        rchrom, rstart, rend = region
        mask = (chrom_str == str(rchrom)) & (pos >= int(rstart)) & (pos <= int(rend))
        pos = pos[mask]
        depth = depth[mask]
        chrom_str = chrom_str[mask]
        region_label = f"{rchrom}:{int(rstart)}-{int(rend)}"
    else:
        # Default: show whichever chromosome carries the most rows.
        if len(chrom_str) > 0:
            unique, counts = np.unique(chrom_str, return_counts=True)
            top_chrom = str(unique[int(np.argmax(counts))])
            mask = chrom_str == top_chrom
            pos = pos[mask]
            depth = depth[mask]
            region_label = top_chrom
        else:
            region_label = "empty"

    palette = THEME.get("palette", {})
    fill_color = palette.get("non_sig", "#999999")
    line_color = palette.get("sig_up", "#D55E00")
    fig, ax = plt.subplots(figsize=figsize)
    if len(pos) > 0:
        order = np.argsort(pos, kind="stable")
        x = pos[order]
        y = depth[order]
        ax.fill_between(x, 0, y, color=fill_color, alpha=0.35,
                        linewidth=0, step="pre")
        ax.plot(x, y, color=line_color, linewidth=0.6)
        ax.set_xlim(float(x.min()), float(x.max()))
    ax.set_xlabel(f"position ({region_label})")
    ax.set_ylabel("depth")
    ymax = float(np.nanmax(depth)) if len(depth) else 1.0
    ax.set_ylim(0, max(ymax * 1.05, 1.0))
    ax.set_title(f"{title}\nn = {len(pos)} positions")
    return savefig(fig, out)


def peak_saturation(
    frame: Any,
    *,
    title: str,
    out: Path,
    depth_col: str = "depth",
    peaks_called_col: str = "peaks_called",
    group_col: Optional[str] = None,
    figsize: Tuple[float, float] = (6.0, 4.5),
) -> Path:
    """Peak-calling saturation curve — number of peaks called as a
    function of subsampled read depth. Plateau indicates saturation;
    ascending curve at the rightmost depth indicates undersampling.

    Required columns:
        - ``depth_col``         subsampled read count (int)
        - ``peaks_called_col``  peaks called at that depth (int)
    Optional:
        - ``group_col``         per-sample / per-replicate stratification

    Plan reference: §S13.1.
    """
    depth = _col(frame, depth_col).astype(float)
    peaks = _col(frame, peaks_called_col).astype(float)
    group = _col(frame, group_col, optional=True) if group_col else None

    palette = THEME.get("palette", {})
    fig, ax = plt.subplots(figsize=figsize)
    if group is None:
        order = np.argsort(depth, kind="stable")
        ax.plot(depth[order], peaks[order],
                color=palette.get("sig_up", "#D55E00"),
                marker="o", markersize=3, linewidth=1.0)
    else:
        group_arr = np.asarray(group, dtype=object)
        levels = list(dict.fromkeys(str(g) for g in group_arr))
        colors = categorical_palette(max(len(levels), 1))
        for level, color in zip(levels, colors):
            mask = np.asarray([str(g) == level for g in group_arr])
            if not mask.any():
                continue
            order = np.argsort(depth[mask], kind="stable")
            ax.plot(depth[mask][order], peaks[mask][order],
                    color=color, marker="o", markersize=3,
                    linewidth=1.0, label=str(level))
        ax.legend(loc="best", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_xlabel("read depth (subsampled)")
    ax.set_ylabel("peaks called")
    ax.set_title(f"{title}\nn = {len(depth)} subsamples")
    return savefig(fig, out)


def isoform_structure(
    frame: Any,
    *,
    title: str,
    out: Path,
    transcript_col: str = "transcript",
    exon_starts_col: str = "exon_starts",
    exon_ends_col: str = "exon_ends",
    strand_col: Optional[str] = None,
    figsize: Tuple[float, float] = (9.0, 4.0),
) -> Path:
    """Transcript / isoform model — one row per transcript, exons drawn
    as filled rectangles, introns as thin connecting lines. Exon
    coordinate columns may hold either a list/array of starts (and
    matching ends) per row, or a single integer per row when the input
    is exploded one-exon-per-row by ``transcript`` (we detect this by
    type-sniffing the first non-empty cell).

    Required columns:
        - ``transcript_col``   transcript id (str)
        - ``exon_starts_col``  exon start coords (list/array per row OR int)
        - ``exon_ends_col``    exon end coords (list/array per row OR int)
    Optional:
        - ``strand_col``       "+" / "-" — controls arrowhead direction

    Plan reference: §S13.2.
    """
    tid = _col(frame, transcript_col).astype(object)
    # exon_starts/exon_ends may hold ragged list-of-lists (packed form);
    # numpy 1.20+ refuses to build an ndarray from those, so pull raw and
    # let the caller iterate. Required-column check still happens here.
    try:
        starts_raw = frame[exon_starts_col]
        ends_raw = frame[exon_ends_col]
    except (KeyError, ValueError, TypeError, IndexError) as exc:
        missing = exon_ends_col if exon_starts_col in (
            getattr(frame, "columns", None) or getattr(frame, "keys", lambda: [])() or []
        ) else exon_starts_col
        raise ValueError(
            f"required column '{missing}' not present in input frame"
        ) from exc
    # Coerce to a plain Python list so `len(...)` and indexing both work
    # whether the input was a list, tuple, numpy array, or pandas Series.
    starts_raw = list(starts_raw)
    ends_raw = list(ends_raw)
    strand = _col(frame, strand_col, optional=True) if strand_col else None

    # Detect packed (list/array per row) vs exploded (one row per exon).
    def _is_listlike(v: Any) -> bool:
        return isinstance(v, (list, tuple, np.ndarray))

    transcripts: Dict[str, Tuple[List[int], List[int], Optional[str]]] = {}
    if len(starts_raw) > 0 and _is_listlike(starts_raw[0]):
        for i in range(len(tid)):
            key = str(tid[i])
            s = [int(x) for x in starts_raw[i]]
            e = [int(x) for x in ends_raw[i]]
            sd = str(strand[i]) if strand is not None else None
            transcripts[key] = (s, e, sd)
    else:
        for i in range(len(tid)):
            key = str(tid[i])
            s = int(starts_raw[i])
            e = int(ends_raw[i])
            sd = str(strand[i]) if strand is not None else None
            cur = transcripts.get(key, ([], [], sd))
            cur[0].append(s)
            cur[1].append(e)
            transcripts[key] = (cur[0], cur[1], sd)

    palette = THEME.get("palette", {})
    exon_color = palette.get("sig_up", "#D55E00")
    intron_color = "#666666"
    fig, ax = plt.subplots(figsize=figsize)

    keys = list(transcripts.keys())
    box_h = 0.45
    for row, key in enumerate(keys):
        s, e, sd = transcripts[key]
        if not s:
            continue
        order = np.argsort(s)
        s_sorted = [s[i] for i in order]
        e_sorted = [e[i] for i in order]
        # Intron line first.
        ax.plot([s_sorted[0], e_sorted[-1]], [row, row],
                color=intron_color, linewidth=0.5, zorder=2)
        # Exon rectangles on top.
        for sx, ex in zip(s_sorted, e_sorted):
            ax.add_patch(plt.Rectangle(
                (sx, row - box_h / 2.0), ex - sx, box_h,
                facecolor=exon_color, edgecolor="#333333",
                linewidth=0.3, zorder=3,
            ))
        # Strand arrow at the 3' end.
        if sd == "+":
            ax.annotate("", xy=(e_sorted[-1] + (e_sorted[-1] - s_sorted[0]) * 0.04, row),
                        xytext=(e_sorted[-1], row),
                        arrowprops=dict(arrowstyle="->", color=intron_color,
                                        linewidth=0.5))
        elif sd == "-":
            ax.annotate("", xy=(s_sorted[0] - (e_sorted[-1] - s_sorted[0]) * 0.04, row),
                        xytext=(s_sorted[0], row),
                        arrowprops=dict(arrowstyle="->", color=intron_color,
                                        linewidth=0.5))

    ax.set_yticks(np.arange(len(keys)))
    ax.set_yticklabels([str(k) for k in keys])
    ax.invert_yaxis()
    ax.set_xlabel("genomic position (bp)")
    ax.set_title(f"{title}\n{len(keys)} transcript(s)")
    return savefig(fig, out)


def sashimi(
    frame: Any,
    *,
    title: str,
    out: Path,
    junction_col: str = "junction",
    count_col: str = "count",
    figsize: Tuple[float, float] = (9.0, 4.5),
) -> Path:
    """Splice-junction sashimi plot — junction read counts drawn as
    arcs over the genomic axis with arc thickness proportional to
    junction count. ``junction_col`` is parsed as ``"start-end"`` (string)
    so callers can pass standard bedpe/leafcutter junction ids without
    pre-splitting.

    Required columns:
        - ``junction_col``  junction id formatted ``"<start>-<end>"`` (str)
        - ``count_col``     supporting read count (int/float)

    Plan reference: §S13.2.
    """
    junctions = _col(frame, junction_col).astype(object)
    counts = _col(frame, count_col).astype(float)

    starts: List[float] = []
    ends: List[float] = []
    for j in junctions:
        s = str(j)
        if "-" in s:
            a, b = s.split("-", 1)
            try:
                starts.append(float(a))
                ends.append(float(b))
            except ValueError:
                starts.append(np.nan)
                ends.append(np.nan)
        else:
            starts.append(np.nan)
            ends.append(np.nan)
    starts_arr = np.asarray(starts, dtype=float)
    ends_arr = np.asarray(ends, dtype=float)
    valid = ~(np.isnan(starts_arr) | np.isnan(ends_arr))

    palette = THEME.get("palette", {})
    arc_color = palette.get("sig_up", "#D55E00")
    fig, ax = plt.subplots(figsize=figsize)

    if valid.any():
        max_count = float(np.nanmax(counts[valid])) if valid.any() else 1.0
        for i in np.where(valid)[0]:
            x0 = float(starts_arr[i])
            x1 = float(ends_arr[i])
            c = float(counts[i])
            if c <= 0 or x1 <= x0:
                continue
            # Arc as a half-ellipse via parametric points.
            xc = (x0 + x1) / 2.0
            rx = (x1 - x0) / 2.0
            ry = (x1 - x0) / 8.0  # flatten the arc so labels read cleanly
            theta = np.linspace(0.0, np.pi, 32)
            ax.plot(xc + rx * np.cos(theta), ry * np.sin(theta),
                    color=arc_color,
                    linewidth=0.5 + 2.0 * (c / (max_count or 1.0)),
                    alpha=0.7)
            # Count label at apex.
            ax.text(xc, ry * 1.05, f"{int(c)}",
                    ha="center", va="bottom",
                    fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                    color="#333333")
        # Genomic baseline.
        x_min = float(np.nanmin(starts_arr[valid]))
        x_max = float(np.nanmax(ends_arr[valid]))
        ax.plot([x_min, x_max], [0, 0], color="#333333", linewidth=0.6)
        ax.set_xlim(x_min, x_max)
        span_max = float(np.nanmax(ends_arr[valid] - starts_arr[valid]))
        ax.set_ylim(-0.1, max(span_max / 8.0 * 1.3, 1.0))
    ax.set_yticks([])
    ax.set_xlabel("genomic position (bp)")
    ax.set_title(f"{title}\nn = {int(valid.sum())} junctions")
    return savefig(fig, out)


def peptide_coverage(
    frame: Any,
    *,
    title: str,
    out: Path,
    position_col: str = "position",
    coverage_col: str = "coverage",
    figsize: Tuple[float, float] = (9.0, 3.0),
) -> Path:
    """Peptide map across protein — coverage / detection-frequency as a
    function of residue position. Drawn as a step plot so the discrete
    residue boundaries are honest. Useful for proteomics result
    review: gaps highlight uncovered regions of the target protein.
    Coverage statistic on the title summarizes the input.

    Required columns:
        - ``position_col``  residue index (int; 1-based or 0-based)
        - ``coverage_col``  coverage at that residue (int/float)

    Plan reference: §S13.3.
    """
    pos = _col(frame, position_col).astype(np.int64)
    cov = _col(frame, coverage_col).astype(float)

    palette = THEME.get("palette", {})
    line_color = palette.get("sig_up", "#D55E00")
    fill_color = palette.get("non_sig", "#999999")
    fig, ax = plt.subplots(figsize=figsize)
    if len(pos) > 0:
        order = np.argsort(pos, kind="stable")
        x = pos[order]
        y = cov[order]
        ax.fill_between(x, 0, y, color=fill_color, alpha=0.35,
                        step="pre", linewidth=0)
        ax.step(x, y, where="pre", color=line_color, linewidth=0.6)
        ax.set_xlim(float(x.min()), float(x.max()))
    cov_pct = (
        100.0 * float(np.sum(cov > 0)) / float(len(cov)) if len(cov) else 0.0
    )
    ax.set_xlabel("residue position")
    ax.set_ylabel("coverage")
    ymax = float(np.nanmax(cov)) if len(cov) else 1.0
    ax.set_ylim(0, max(ymax * 1.1, 1.0))
    ax.set_title(f"{title}\n{cov_pct:.1f}% residues covered, n = {len(pos)}")
    return savefig(fig, out)


def ridgeline(
    frame: Any,
    *,
    title: str,
    out: Path,
    group_col: str = "group",
    value_col: str = "value",
    bandwidth: float = 0.4,
    overlap: float = 0.7,
    figsize: Tuple[float, float] = (7.0, 5.0),
) -> Path:
    """Overlapping density ridges (joyplot-style) — one row per group,
    each row a kernel density estimate of ``value_col`` shaded with the
    categorical palette. ``overlap`` controls vertical row spacing
    (1.0 = full overlap, 0.0 = stacked rows; default 0.7).

    Required columns:
        - ``group_col``  stratifying group (str)
        - ``value_col``  numeric value to estimate density of (float)

    Plan reference: §S13.3.
    """
    group = _col(frame, group_col).astype(object)
    value = _col(frame, value_col).astype(float)

    levels = list(dict.fromkeys(str(g) for g in group))
    colors = categorical_palette(max(len(levels), 1))

    fig, ax = plt.subplots(figsize=figsize)
    n_levels = len(levels)
    if n_levels == 0:
        ax.set_title(title)
        return savefig(fig, out)

    # Shared x grid covers the global range of values.
    x_min = float(np.nanmin(value)) if len(value) else 0.0
    x_max = float(np.nanmax(value)) if len(value) else 1.0
    pad = (x_max - x_min) * 0.1 + 1e-9
    grid = np.linspace(x_min - pad, x_max + pad, 200)

    # Vertical row positions; each row gets a unit and we scale densities.
    row_height = max(1.0 - max(0.0, min(overlap, 0.99)), 0.05)
    densities: List[np.ndarray] = []
    for level in levels:
        mask = np.asarray([str(g) == level for g in group])
        v = value[mask]
        if len(v) == 0:
            densities.append(np.zeros_like(grid))
            continue
        # Cheap Gaussian KDE without scipy: fixed bandwidth on standardized data.
        sd = float(np.nanstd(v)) or 1.0
        h = bandwidth * sd
        # Vectorized kernel sum: O(n_grid * n_v); fine at primitive scale.
        diffs = (grid[:, None] - v[None, :]) / (h or 1.0)
        kernel = np.exp(-0.5 * diffs ** 2) / np.sqrt(2.0 * np.pi)
        density = kernel.sum(axis=1) / (len(v) * (h or 1.0))
        densities.append(density)

    # Normalize per-row density to row_height + small headroom.
    max_density = max(float(np.nanmax(d)) for d in densities) or 1.0
    for i, (level, dens, color) in enumerate(zip(levels, densities, colors)):
        baseline = float(n_levels - 1 - i)
        scaled = baseline + dens / max_density * (1.0 + (1.0 - row_height))
        ax.fill_between(grid, baseline, scaled,
                        color=color, alpha=0.7, linewidth=0.4,
                        edgecolor="#333333")
        ax.plot(grid, scaled, color="#333333", linewidth=0.4)

    ax.set_yticks(np.arange(n_levels))
    ax.set_yticklabels(levels[::-1])
    ax.set_xlabel(value_col)
    ax.set_title(f"{title}\n{n_levels} group(s), n = {len(value)}")
    # Hide y-axis spine and ticks for cleaner ridgeline look.
    ax.spines["left"].set_visible(False)
    ax.tick_params(axis="y", length=0)
    return savefig(fig, out)


# ---------------------------------------------------------------------------
# Phase I (plan §S13.4-§S13.5) — metagenomics + spatial transcriptomics.
# ---------------------------------------------------------------------------


def taxonomic_stacked_bar(
    frame: Any,
    *,
    title: str,
    out: Path,
    sample_col: str = "sample",
    taxon_col: str = "taxon",
    abundance_col: str = "abundance",
    top_n: int = 12,
    horizontal: bool = False,
    figsize: Tuple[float, float] = (8.0, 5.0),
) -> Path:
    """Stacked relative-abundance bar by sample — one bar per sample,
    colored segments per taxon. Top-``top_n`` taxa by total abundance
    are kept; the rest aggregate into an "Other" segment so the legend
    stays interpretable.

    Required columns:
        - ``sample_col``     sample id (str)
        - ``taxon_col``      taxon label (str)
        - ``abundance_col``  abundance (counts or pre-normalized fractions)

    Plan reference: §S13.4.
    """
    sample = _col(frame, sample_col).astype(object)
    taxon = _col(frame, taxon_col).astype(object)
    abundance = _col(frame, abundance_col).astype(float)

    samples = list(dict.fromkeys(str(s) for s in sample))
    # Aggregate per-(sample, taxon) sums.
    agg: Dict[Tuple[str, str], float] = {}
    for s, t, a in zip(sample, taxon, abundance):
        k = (str(s), str(t))
        agg[k] = agg.get(k, 0.0) + float(a)

    # Total per taxon for top-N retention.
    taxon_totals: Dict[str, float] = {}
    for (_s, t), v in agg.items():
        taxon_totals[t] = taxon_totals.get(t, 0.0) + v
    top_taxa = [t for t, _ in sorted(
        taxon_totals.items(), key=lambda kv: kv[1], reverse=True
    )[:top_n]]
    other_present = len(taxon_totals) > len(top_taxa)
    plot_taxa = top_taxa + (["Other"] if other_present else [])

    # Build sample × taxon matrix and normalize to row sums.
    matrix = np.zeros((len(samples), len(plot_taxa)), dtype=float)
    for (s, t), v in agg.items():
        si = samples.index(s)
        if t in top_taxa:
            ti = top_taxa.index(t)
        elif other_present:
            ti = len(top_taxa)  # "Other" bucket
        else:
            continue
        matrix[si, ti] += v
    row_sums = matrix.sum(axis=1)
    row_sums[row_sums == 0] = 1.0
    rel = matrix / row_sums[:, None]

    colors = categorical_palette(len(plot_taxa))
    fig, ax = plt.subplots(figsize=figsize)
    if horizontal:
        y = np.arange(len(samples))
        offsets = np.zeros(len(samples))
        for ti, (taxon_label, color) in enumerate(zip(plot_taxa, colors)):
            vals = rel[:, ti]
            ax.barh(y, vals, left=offsets, color=color,
                    edgecolor="#333333", linewidth=0.2,
                    label=str(taxon_label))
            offsets += vals
        ax.set_yticks(y)
        ax.set_yticklabels(samples)
        ax.invert_yaxis()
        ax.set_xlabel("relative abundance")
        ax.set_xlim(0, 1)
    else:
        x = np.arange(len(samples))
        offsets = np.zeros(len(samples))
        for ti, (taxon_label, color) in enumerate(zip(plot_taxa, colors)):
            vals = rel[:, ti]
            ax.bar(x, vals, bottom=offsets, color=color,
                   edgecolor="#333333", linewidth=0.2,
                   label=str(taxon_label))
            offsets += vals
        ax.set_xticks(x)
        ax.set_xticklabels(samples, rotation=45, ha="right")
        ax.set_ylabel("relative abundance")
        ax.set_ylim(0, 1)
    ax.legend(loc="center left", bbox_to_anchor=(1.0, 0.5),
              frameon=False,
              fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_title(
        f"{title}\n{len(samples)} sample(s), top {len(top_taxa)} taxa"
        f"{' + Other' if other_present else ''}"
    )
    return savefig(fig, out)


def diversity_violin(
    frame: Any,
    *,
    title: str,
    out: Path,
    group_col: str = "group",
    diversity_col: str = "diversity",
    figsize: Tuple[float, float] = (6.0, 4.5),
) -> Path:
    """Alpha-diversity distribution per group — one violin per group
    using the existing ``violin`` helper underneath, with the diversity
    metric labeled on the y-axis. Mean ± SD annotation per group on
    the title summarizes the cohort.

    Required columns:
        - ``group_col``      stratifying group (str)
        - ``diversity_col``  diversity metric value (Shannon, Simpson, etc.)

    Plan reference: §S13.4.
    """
    group = _col(frame, group_col).astype(object)
    div = _col(frame, diversity_col).astype(float)

    levels = list(dict.fromkeys(str(g) for g in group))
    data = [div[np.asarray([str(g) == lvl for g in group])] for lvl in levels]
    colors = categorical_palette(max(len(levels), 1))

    fig, ax = plt.subplots(figsize=figsize)
    if len(levels) == 0 or all(len(d) == 0 for d in data):
        ax.set_title(title)
        return savefig(fig, out)

    parts = ax.violinplot(
        data, positions=np.arange(len(levels)),
        showmeans=False, showmedians=True, widths=0.7,
    )
    # Apply categorical palette colors.
    bodies = parts.get("bodies", []) if isinstance(parts, dict) else parts["bodies"]
    for body, color in zip(bodies, colors):
        body.set_facecolor(color)
        body.set_edgecolor("#333333")
        body.set_alpha(0.7)
        body.set_linewidth(0.4)
    # Subtle dashed lines for whiskers/medians.
    for key in ("cmedians", "cbars", "cmins", "cmaxes"):
        coll = parts.get(key) if isinstance(parts, dict) else parts.get(key)
        if coll is not None:
            coll.set_color("#333333")
            coll.set_linewidth(0.4)

    ax.set_xticks(np.arange(len(levels)))
    ax.set_xticklabels(levels)
    ax.set_xlabel(group_col)
    ax.set_ylabel(diversity_col)
    ax.set_title(f"{title}\n{len(levels)} group(s), n = {len(div)}")
    return savefig(fig, out)


def tissue_overlay(
    coords_df: Any,
    image: Any,
    *,
    title: str,
    out: Path,
    value_col: str = "value",
    x_col: str = "x",
    y_col: str = "y",
    cmap: str = "viridis",
    point_size: float = 8.0,
    image_alpha: float = 0.6,
    figsize: Tuple[float, float] = (7.0, 7.0),
) -> Path:
    """Per-spot value overlay on an H&E or fluorescent tissue image.
    ``image`` is any 2D / 3D numpy array (grayscale or RGB) drawn as
    the background; ``coords_df`` carries (x, y, value) per spot.

    Required ``coords_df`` columns:
        - ``x_col``       spot x pixel coordinate (float)
        - ``y_col``       spot y pixel coordinate (float)
        - ``value_col``   per-spot value to encode by colormap (float)

    Plan reference: §S13.5.
    """
    x = _col(coords_df, x_col).astype(float)
    y = _col(coords_df, y_col).astype(float)
    val = _col(coords_df, value_col).astype(float)

    fig, ax = plt.subplots(figsize=figsize)
    if image is not None:
        try:
            arr = np.asarray(image)
            ax.imshow(arr, alpha=image_alpha, zorder=1)
        except (TypeError, ValueError):
            # Image was not arrayable — skip background but still draw spots.
            pass
    sc = ax.scatter(
        x, y, c=val, s=point_size, cmap=cmap,
        edgecolor="white", linewidth=0.2, alpha=0.9, zorder=2,
    )
    cbar = fig.colorbar(sc, ax=ax, fraction=0.04, pad=0.02)
    cbar.set_label(value_col,
                   fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    cbar.ax.tick_params(labelsize=THEME.get("fonts", {}).get("tick_pt", 7))
    ax.set_xlabel("x")
    ax.set_ylabel("y")
    # Match imshow's flipped y-axis convention only when an image was drawn.
    if image is not None:
        try:
            np.asarray(image)
            ax.invert_yaxis()
        except (TypeError, ValueError):
            pass
    ax.set_title(f"{title}\nn = {len(x)} spots")
    return savefig(fig, out)


def morans_i_scatter(
    frame: Any,
    *,
    title: str,
    out: Path,
    gene_col: str = "gene",
    morans_i_col: str = "morans_i",
    p_col: str = "p_value",
    sig_threshold: float = 0.05,
    label_top_n: int = 10,
    figsize: Tuple[float, float] = (6.0, 5.0),
) -> Path:
    """Spatial autocorrelation scatter — Moran's I on the x-axis,
    -log10(p) on the y-axis. Genes above the BH-uncorrected p-value
    threshold get the sig_up color; the top-``label_top_n`` highest-I
    genes are labeled with collision avoidance via the volcano helper.

    Required columns:
        - ``gene_col``       gene id (str)
        - ``morans_i_col``   Moran's I statistic (float; typically -1..1)
        - ``p_col``          raw p-value (float)

    Plan reference: §S13.5.
    """
    gene = _col(frame, gene_col).astype(object)
    morans_i = _col(frame, morans_i_col).astype(float)
    pvalue = _col(frame, p_col).astype(float)
    nlogp = -np.log10(np.clip(pvalue, 1e-300, 1.0))

    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")
    non_sig_color = palette.get("non_sig", "#999999")
    sig = pvalue < sig_threshold
    colors = np.where(sig, sig_color, non_sig_color)

    fig, ax = plt.subplots(figsize=figsize)
    ax.scatter(morans_i, nlogp, c=colors, s=10,
               alpha=0.85, linewidths=0)
    ax.axhline(-np.log10(sig_threshold),
               color="#444444", linestyle="--", linewidth=0.4)
    ax.axvline(0.0, color="#444444", linestyle="--", linewidth=0.4)

    # Top-N most-spatially-autocorrelated labels via shared helper.
    if label_top_n > 0 and len(morans_i) > 0:
        order = np.argsort(morans_i)[::-1][:label_top_n]
        coords = [(float(morans_i[i]), float(nlogp[i])) for i in order]
        x_min = float(np.nanmin(morans_i))
        x_max = float(np.nanmax(morans_i))
        ymax = float(np.nanmax(nlogp)) if len(nlogp) else 1.0
        x_range = (x_max - x_min) or 1.0
        y_range = ymax or 1.0
        placements = _greedy_label_placement(
            coords, width=x_range, height=y_range, min_dist=0.05
        )
        for i, place in zip(order, placements):
            if place is None:
                continue
            ax.annotate(
                str(gene[i]),
                xy=(morans_i[i], nlogp[i]),
                xytext=place,
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#222222",
                arrowprops=(
                    dict(arrowstyle="-", color="#888888", linewidth=0.3)
                    if place != (float(morans_i[i]), float(nlogp[i]))
                    else None
                ),
            )

    ax.set_xlabel("Moran's I")
    ax.set_ylabel("−log₁₀ p")
    n_sig = int(sig.sum())
    ax.set_title(f"{title}\n{n_sig} / {len(gene)} genes p < {sig_threshold}")
    return savefig(fig, out)


def neighborhood_enrichment(
    frame: Any,
    *,
    title: str,
    out: Path,
    source_col: str = "source",
    target_col: str = "target",
    score_col: str = "score",
    cmap: str = "RdBu_r",
    vmax: Optional[float] = None,
    figsize: Tuple[float, float] = (6.5, 6.0),
) -> Path:
    """Squidpy-style neighborhood enrichment heatmap — pairwise z-scored
    co-occurrence of cell types / spatial domains. Diverging colormap
    centered on zero; ``vmax`` clips the symmetric range so a few
    extreme cells don't compress the rest of the matrix.

    Required columns:
        - ``source_col``  source cluster / cell-type label (str)
        - ``target_col``  target cluster / cell-type label (str)
        - ``score_col``   enrichment z-score (float; signed)

    Plan reference: §S13.5.
    """
    source = _col(frame, source_col).astype(object)
    target = _col(frame, target_col).astype(object)
    score = _col(frame, score_col).astype(float)

    # Stable label ordering: union of source + target, first-seen order.
    labels = list(dict.fromkeys(
        list(str(s) for s in source) + list(str(t) for t in target)
    ))
    n = len(labels)
    matrix = np.full((n, n), np.nan, dtype=float)
    for s, t, v in zip(source, target, score):
        i = labels.index(str(s))
        j = labels.index(str(t))
        matrix[i, j] = float(v)

    if vmax is None:
        if n > 0 and np.isfinite(matrix).any():
            vmax = float(np.nanmax(np.abs(matrix)))
        else:
            vmax = 1.0
    if vmax <= 0:
        vmax = 1.0

    fig, ax = plt.subplots(figsize=figsize)
    im = ax.imshow(
        matrix, cmap=cmap, vmin=-vmax, vmax=vmax,
        aspect="auto", interpolation="nearest",
    )
    ax.set_xticks(np.arange(n))
    ax.set_xticklabels(labels, rotation=45, ha="right")
    ax.set_yticks(np.arange(n))
    ax.set_yticklabels(labels)
    cbar = fig.colorbar(im, ax=ax, fraction=0.046, pad=0.04)
    cbar.set_label("z-score",
                   fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    cbar.ax.tick_params(labelsize=THEME.get("fonts", {}).get("tick_pt", 7))
    ax.set_title(f"{title}\n{n} × {n}")
    return savefig(fig, out)


def forecast_ribbon(
    frame: Any,
    *,
    title: str,
    out: Path,
    time_col: str = "time",
    value_col: str = "forecast",
    lower_col: str = "lower",
    upper_col: str = "upper",
    actual_col: Optional[str] = "actual",
    group_col: Optional[str] = None,
    figsize: Tuple[float, float] = (8.0, 4.5),
) -> Path:
    """Point + interval forecast ribbon. Plots the point forecast as a
    line with the prediction-interval ribbon shaded behind it; optional
    ``actual_col`` overlays observed values so the reader can see in-
    sample fit and out-of-sample divergence in one frame.

    Required columns:
        - ``time_col``    forecast time (float, datetime-like, or numeric index)
        - ``value_col``   point forecast (float)
        - ``lower_col``   interval lower bound (float)
        - ``upper_col``   interval upper bound (float)
    Optional:
        - ``actual_col``  observed values; drawn as overlay when present

    Plan reference: §S14.1 (time-series Phase J).
    """
    time = _col(frame, time_col)
    value = _col(frame, value_col).astype(float)
    lower = _col(frame, lower_col).astype(float)
    upper = _col(frame, upper_col).astype(float)
    actual = (
        _col(frame, actual_col, optional=True) if actual_col else None
    )
    actual_values = _optional_float_col(actual)
    groups = _col(frame, group_col, optional=True) if group_col else None

    palette = THEME.get("palette", {})
    forecast_color = palette.get("sig_up", "#D55E00")
    actual_color = palette.get("sig_down", "#0072B2")
    band_color = palette.get("non_sig", "#999999")

    raw_time = np.asarray(time)
    if np.issubdtype(raw_time.dtype, np.number):
        x_all = raw_time.astype(float)
        label_lookup: Optional[Dict[float, str]] = None
    else:
        labels = [str(x) for x in raw_time]
        ordered_labels = sorted(dict.fromkeys(labels))
        label_to_x = {label: float(i) for i, label in enumerate(ordered_labels)}
        x_all = np.asarray([label_to_x[label] for label in labels], dtype=float)
        label_lookup = {x: label for label, x in label_to_x.items()}

    fig, ax = plt.subplots(figsize=figsize)

    if groups is not None and len(groups):
        group_arr = np.asarray(groups, dtype=object)
        group_names = [str(g) for g in dict.fromkeys(group_arr)]
        colors = categorical_palette(len(group_names), name="forecast_ribbon")
        for idx, group_name in enumerate(group_names):
            mask = group_arr.astype(str) == group_name
            order = np.argsort(x_all[mask], kind="stable")
            gx = x_all[mask][order]
            gv = value[mask][order]
            glo = lower[mask][order]
            ghi = upper[mask][order]
            color = colors[idx]
            ax.fill_between(
                gx,
                glo,
                ghi,
                color=color,
                alpha=0.12,
                linewidth=0,
                label=f"{group_name} interval" if idx == 0 else None,
            )
            ax.plot(
                gx,
                gv,
                color=color,
                linewidth=1.1,
                label=str(group_name),
                zorder=3,
            )
            if actual_values is not None and len(actual_values):
                ga = actual_values[mask][order]
                finite = np.isfinite(ga)
                if not finite.any():
                    continue
                ax.plot(
                    gx[finite],
                    ga[finite],
                    color=color,
                    linewidth=0.8,
                    linestyle=":",
                    alpha=0.75,
                    zorder=2,
                )
    else:
        # Sort by time so the line and ribbon track contiguously even if the
        # caller passed un-ordered rows.
        order = np.argsort(x_all, kind="stable")
        t = x_all[order]
        v = value[order]
        lo = lower[order]
        hi = upper[order]

        ax.fill_between(
            t, lo, hi,
            color=band_color, alpha=0.30, linewidth=0,
            label="prediction interval",
        )
        ax.plot(t, v, color=forecast_color, linewidth=1.4,
                label="forecast", zorder=3)

        if actual_values is not None and len(actual_values):
            a = actual_values[order]
            finite = np.isfinite(a)
            if finite.any():
                ax.plot(t[finite], a[finite], color=actual_color, linewidth=1.0,
                        alpha=0.85, label="actual", zorder=2)

    if label_lookup:
        ticks = sorted(label_lookup)
        if len(ticks) > 12:
            step = int(np.ceil(len(ticks) / 12))
            ticks = ticks[::step]
        ax.set_xticks(ticks)
        ax.set_xticklabels([label_lookup[t] for t in ticks], rotation=45, ha="right")

    ax.set_xlabel(time_col)
    ax.set_ylabel(value_col)
    ax.legend(loc="best", frameon=False,
              fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_title(f"{title}\nn = {len(value)} forecast points")
    return savefig(fig, out)


def acf_pacf_panel(
    frame: Any,
    *,
    title: str,
    out: Path,
    value_col: str = "value",
    max_lag: int = 40,
    ci_level: float = 0.95,
    figsize: Tuple[float, float] = (8.0, 5.0),
) -> Path:
    """Paired ACF + PACF lag plot. Two stacked subplots — autocorrelation
    on top, partial-autocorrelation below — sharing the lag axis. The
    Bartlett ±1.96/√n CI band shades the noise envelope so significant
    lags read at a glance. Stems use the theme's sig_up color; non-
    significant lags fall back to the non_sig grey.

    Required columns:
        - ``value_col``   the time-series values (float, evenly spaced)

    Plan reference: §S14.1 (time-series Phase J).
    """
    series = _col(frame, value_col).astype(float)
    n = len(series)
    if n < 2:
        fig, ax = plt.subplots(figsize=figsize)
        ax.set_title(f"{title}\nn = {n} (too few points)")
        return savefig(fig, out)
    max_lag = min(int(max_lag), max(1, n - 1))
    s = series - float(np.nanmean(series))

    # ACF via the unbiased-ish autocovariance (divide by n, classic Box-Jenkins
    # form). Lag-0 is 1 by construction.
    denom = float(np.dot(s, s)) or 1.0
    acf = np.zeros(max_lag + 1, dtype=float)
    acf[0] = 1.0
    for k in range(1, max_lag + 1):
        acf[k] = float(np.dot(s[:-k], s[k:])) / denom

    # PACF via Durbin-Levinson recursion — matches statsmodels' default
    # 'ols' behavior to within numerical tolerance for stationary series.
    pacf = np.zeros(max_lag + 1, dtype=float)
    pacf[0] = 1.0
    phi: List[List[float]] = [[]]
    for k in range(1, max_lag + 1):
        prev = phi[k - 1]
        if k == 1:
            pk = acf[1]
        else:
            num = acf[k] - sum(prev[j] * acf[k - 1 - j] for j in range(k - 1))
            den = 1.0 - sum(prev[j] * acf[j + 1] for j in range(k - 1))
            pk = num / den if abs(den) > 1e-12 else 0.0
        pacf[k] = pk
        new = [prev[j] - pk * prev[k - 2 - j] for j in range(k - 1)]
        new.append(pk)
        phi.append(new)

    # Bartlett band — z * 1/√n. ci_level → z via inverse-normal approx;
    # avoid scipy import for parity with manhattan/qq fallback shape.
    z = 1.959963984540054 if abs(ci_level - 0.95) < 1e-6 else 2.5758293035489004
    band = z / np.sqrt(n)

    palette = THEME.get("palette", {})
    sig_color = palette.get("sig_up", "#D55E00")
    non_sig = palette.get("non_sig", "#999999")

    fig, (ax_a, ax_p) = plt.subplots(
        2, 1, figsize=figsize, sharex=True,
        gridspec_kw={"hspace": 0.15},
    )
    lags = np.arange(max_lag + 1)
    for ax, vals, label in ((ax_a, acf, "ACF"), (ax_p, pacf, "PACF")):
        ax.axhline(0.0, color="#444444", linewidth=0.4)
        ax.fill_between(lags, -band, band, color="#cccccc", alpha=0.5,
                        linewidth=0)
        for li, vi in zip(lags, vals):
            color = sig_color if abs(vi) > band else non_sig
            ax.vlines(li, 0, vi, color=color, linewidth=1.0)
            ax.scatter([li], [vi], s=12, color=color, zorder=3, linewidths=0)
        ax.set_ylabel(label)
        ax.set_ylim(-1.05, 1.05)

    ax_p.set_xlabel("lag")
    ax_a.set_title(f"{title}\nn = {n}, max lag = {max_lag}")
    return savefig(fig, out)


def decomposition_panel(
    frame: Any,
    *,
    title: str,
    out: Path,
    time_col: str = "time",
    value_col: str = "value",
    period: Optional[int] = None,
    figsize: Tuple[float, float] = (8.0, 7.0),
) -> Path:
    """STL-style trend + seasonal + residual three-panel. Inline classical
    decomposition (centered moving-average trend → seasonal mean by phase
    → residual) avoids the statsmodels dependency while matching the
    visual contract: observed/trend/seasonal/residual rows sharing a time
    axis. ``period`` defaults to a heuristic of len/4 (clamped to ≥2).

    Required columns:
        - ``time_col``    measurement time (float / datetime-like)
        - ``value_col``   measured value (float, evenly spaced)

    Plan reference: §S14.1 (time-series Phase J).
    """
    time = _col(frame, time_col)
    value = _col(frame, value_col).astype(float)
    n = len(value)
    if n == 0:
        fig, ax = plt.subplots(figsize=figsize)
        ax.set_title(f"{title}\nempty series")
        return savefig(fig, out)

    try:
        order = np.argsort(time, kind="stable")
    except TypeError:
        order = np.arange(n)
    t = np.asarray(time)[order]
    y = value[order]

    if period is None:
        period = max(2, min(n // 4, 365))
    period = max(2, int(period))

    # Centered moving-average trend with window = period (use period+1 if
    # period is even so the window stays odd / centered on an integer).
    win = period if period % 2 == 1 else period + 1
    half = win // 2
    trend = np.full(n, np.nan, dtype=float)
    if n >= win:
        kernel = np.ones(win, dtype=float) / win
        # Convolve in 'valid' then pad with NaN at the edges so the
        # plot honestly shows where trend can't be estimated.
        valid = np.convolve(y, kernel, mode="valid")
        trend[half:half + len(valid)] = valid

    # Detrend, then average per phase index modulo period.
    detrended = y - trend
    seasonal = np.zeros(n, dtype=float)
    for phase in range(period):
        idx = np.arange(phase, n, period)
        if len(idx) == 0:
            continue
        vals = detrended[idx]
        m = float(np.nanmean(vals)) if np.any(np.isfinite(vals)) else 0.0
        seasonal[idx] = m
    # Re-center seasonal to zero mean.
    if np.any(np.isfinite(seasonal)):
        seasonal -= float(np.nanmean(seasonal))

    residual = y - np.where(np.isnan(trend), 0.0, trend) - seasonal
    # Edges where trend is NaN — null-out residual rather than corrupt it.
    residual = np.where(np.isnan(trend), np.nan, residual)

    palette = THEME.get("palette", {})
    obs_color = "#222222"
    trend_color = palette.get("sig_up", "#D55E00")
    seas_color = palette.get("sig_down", "#0072B2")
    resid_color = palette.get("non_sig", "#999999")

    fig, axes = plt.subplots(4, 1, figsize=figsize, sharex=True,
                             gridspec_kw={"hspace": 0.18})
    rows = [
        ("observed", y, obs_color),
        ("trend", trend, trend_color),
        ("seasonal", seasonal, seas_color),
        ("residual", residual, resid_color),
    ]
    for ax, (label, series_y, color) in zip(axes, rows):
        ax.plot(t, series_y, color=color, linewidth=1.0)
        ax.set_ylabel(label)
        if label == "residual":
            ax.axhline(0.0, color="#444444", linewidth=0.3, linestyle="--")

    axes[-1].set_xlabel(time_col)
    axes[0].set_title(f"{title}\nn = {n}, period = {period}")
    return savefig(fig, out)


def anomaly_timeline(
    frame: Any,
    *,
    title: str,
    out: Path,
    time_col: str = "time",
    value_col: str = "value",
    anomaly_col: str = "is_anomaly",
    figsize: Tuple[float, float] = (8.0, 4.0),
) -> Path:
    """Value timeline with shaded anomaly windows. Contiguous runs of
    truthy ``anomaly_col`` are merged into vertical axvspans so a long
    incident reads as one block instead of N stacked points; anomaly
    points are also marked on the line in sig_up.

    Required columns:
        - ``time_col``      timestamp (float / datetime-like)
        - ``value_col``     measured value (float)
        - ``anomaly_col``   per-point anomaly flag (bool / 0-1)

    Plan reference: §S14.1 (time-series Phase J).
    """
    time = _col(frame, time_col)
    value = _col(frame, value_col).astype(float)
    flag_raw = _col(frame, anomaly_col)
    n = len(value)

    try:
        order = np.argsort(time, kind="stable")
    except TypeError:
        order = np.arange(n)
    t = np.asarray(time)[order]
    y = value[order]
    flag = np.asarray(flag_raw)[order].astype(bool)

    palette = THEME.get("palette", {})
    line_color = "#222222"
    anom_color = palette.get("sig_up", "#D55E00")

    fig, ax = plt.subplots(figsize=figsize)
    ax.plot(t, y, color=line_color, linewidth=0.9, zorder=2)

    # Merge contiguous runs of anomaly=True into axvspans.
    if n > 0 and flag.any():
        # Find run starts/ends.
        diffs = np.diff(flag.astype(int))
        starts = list(np.where(diffs == 1)[0] + 1)
        ends = list(np.where(diffs == -1)[0] + 1)
        if flag[0]:
            starts.insert(0, 0)
        if flag[-1]:
            ends.append(n)
        for s, e in zip(starts, ends):
            ax.axvspan(t[s], t[e - 1], color=anom_color,
                       alpha=0.18, linewidth=0, zorder=1)
        ax.scatter(t[flag], y[flag], color=anom_color,
                   s=14, zorder=3, linewidths=0,
                   label=f"anomaly · n={int(flag.sum())}")
        ax.legend(loc="best", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))

    ax.set_xlabel(time_col)
    ax.set_ylabel(value_col)
    ax.set_title(f"{title}\nn = {n}, anomalies = {int(flag.sum())}")
    return savefig(fig, out)


def ma_plot(
    frame: Any,
    *,
    title: str,
    out: Path,
    mean_col: str = "base_mean",
    lfc_col: str = "log2FoldChange",
    padj_col: str = "padj",
    padj_threshold: float = 0.05,
    fc_threshold: float = 1.0,
    label_top_n: int = 10,
    gene_col: Optional[str] = "gene",
    figsize: Tuple[float, float] = (7.0, 5.0),
) -> Path:
    """MA plot — log2 fold change vs log10 mean expression. Classic
    DESeq2-style alternative to volcano: x-axis is the average expression
    level (log10 base mean), y-axis is log2 fold change, points colored
    by direction × significance using the theme palette. A horizontal
    zero line and ±``fc_threshold`` guides anchor the eye; up/down counts
    annotate the corners.

    Required columns:
        - ``mean_col``    base mean / average expression (float, > 0)
        - ``lfc_col``     log2 fold change (float)
        - ``padj_col``    adjusted p-value (float, 0..1)
    Optional:
        - ``gene_col``    gene id; top-N labels via the volcano helper

    Plan reference: §S14.2 (bulk RNA-seq Phase J polish).
    """
    mean = _col(frame, mean_col).astype(float)
    lfc = _col(frame, lfc_col).astype(float)
    padj = _col(frame, padj_col).astype(float)
    labels = _col(frame, gene_col, optional=True) if gene_col else None

    # log10 of base mean. Clip to avoid log(0); base means are non-negative.
    log_mean = np.log10(np.clip(mean, 1e-9, None))
    sig_mask = (padj < padj_threshold) & (np.abs(lfc) >= fc_threshold)
    up_mask = sig_mask & (lfc > 0)
    down_mask = sig_mask & (lfc < 0)

    palette = THEME.get("palette", {})
    sig_up = palette.get("sig_up", "#D55E00")
    sig_down = palette.get("sig_down", "#0072B2")
    non_sig = palette.get("non_sig", "#999999")
    colors = np.where(up_mask, sig_up, np.where(down_mask, sig_down, non_sig))

    rasterize = (
        len(lfc) > THEME.get("output", {}).get("rasterize_threshold_n", 50000)
    )
    fig, ax = plt.subplots(figsize=figsize)
    ax.scatter(log_mean, lfc, c=colors, s=6, alpha=0.75, linewidths=0,
               rasterized=rasterize)
    ax.axhline(0.0, color="#444444", linestyle="-", linewidth=0.5)
    ax.axhline(fc_threshold, color="#444444", linestyle="--", linewidth=0.4)
    ax.axhline(-fc_threshold, color="#444444", linestyle="--", linewidth=0.4)

    n_up = int(up_mask.sum())
    n_down = int(down_mask.sum())
    n_total = int(len(lfc))
    ax.text(0.99, 0.97, f"↑ {n_up}",
            transform=ax.transAxes, ha="right", va="top",
            color=sig_up, fontweight="bold")
    ax.text(0.01, 0.03, f"↓ {n_down}",
            transform=ax.transAxes, ha="left", va="bottom",
            color=sig_down, fontweight="bold")

    ax.set_xlabel("log₁₀ mean expression")
    ax.set_ylabel("log₂ fold change")
    ax.set_title(f"{title}\nn = {n_total} features")

    # Top-N labels via the shared collision-avoidance helper. Score by
    # |LFC| × −log10(padj) so the most-significant high-effect genes win.
    if labels is not None and label_top_n > 0:
        with np.errstate(divide="ignore", invalid="ignore"):
            sig_score = -np.log10(np.clip(padj, 1e-300, 1.0))
        score = np.abs(lfc) * np.where(np.isfinite(sig_score), sig_score, 0.0)
        if sig_mask.any():
            score = np.where(sig_mask, score, -np.inf)
        order = np.argsort(score)[::-1][:label_top_n]
        order = [int(i) for i in order if np.isfinite(score[i])]
        coords = [(float(log_mean[i]), float(lfc[i])) for i in order]
        x_range = float(np.nanmax(log_mean) - np.nanmin(log_mean)) or 1.0
        y_range = float(np.nanmax(lfc) - np.nanmin(lfc)) or 1.0
        placements = _greedy_label_placement(
            coords, width=x_range, height=y_range, min_dist=0.04
        )
        for idx, place in zip(order, placements):
            if place is None:
                continue
            ax.annotate(
                str(labels[idx]),
                xy=(log_mean[idx], lfc[idx]),
                xytext=place,
                fontsize=THEME.get("fonts", {}).get("legend_pt", 7),
                color="#222222",
                arrowprops=(
                    dict(arrowstyle="-", color="#888888", linewidth=0.3)
                    if place != (float(log_mean[idx]), float(lfc[idx]))
                    else None
                ),
            )

    return savefig(fig, out)


def dashboard_grid(
    panels: Sequence[Dict[str, Any]],
    *,
    title: str,
    out: Path,
    layout: Tuple[int, int],
    figsize: Optional[Tuple[float, float]] = None,
) -> Path:
    """Cross-modality composite — multi-panel publication grid that
    routes each panel through one of the existing primitives. Each
    ``panels[i]`` is ``{"type": "<primitive>", "data": <frame>,
    "args": <kwargs dict>}`` where ``type`` names a primitive on this
    module (``manhattan``, ``volcano``, ``kaplan_meier``, ``forest``,
    ``ma_plot``, ``forecast_ribbon``, ``acf_pacf_panel``,
    ``decomposition_panel``, ``anomaly_timeline``, etc.). Each panel
    inherits the standard theme + provenance footer once at the figure
    level and gets a numbered subtitle ("(1) <subtitle>" …) so cross-
    references read unambiguously.

    Implementation note: the routed primitive owns the savefig path —
    here we render each panel into its own scratch axes by calling its
    drawing primitives on a dedicated subplot. To stay deterministic and
    keep the call surface small, the dashboard supports the stable
    subset listed in ``_DASHBOARD_PANEL_RENDERERS`` below; unknown
    types fall back to a placeholder block with the type name so the
    caller sees a clear "this panel was unrenderable" signal rather
    than a silent skip.

    Plan reference: §S14.4 (Phase K cross-modality composite).
    """
    rows, cols = int(layout[0]), int(layout[1])
    if rows < 1 or cols < 1:
        raise ValueError(
            f"dashboard_grid layout must be positive (got {layout!r})"
        )
    if figsize is None:
        figsize = (5.0 * cols, 4.0 * rows)

    fig, axes = plt.subplots(rows, cols, figsize=figsize,
                             squeeze=False)
    fig.suptitle(title, fontsize=THEME.get("fonts", {}).get("title_pt", 9))

    n_cells = rows * cols
    for cell_idx in range(n_cells):
        r, c = divmod(cell_idx, cols)
        ax = axes[r][c]
        if cell_idx >= len(panels):
            ax.axis("off")
            continue
        panel = panels[cell_idx]
        ptype = str(panel.get("type", "")).strip()
        data = panel.get("data")
        args = dict(panel.get("args", {}) or {})
        # Subtitle: "(N) caller's subtitle" if provided, else "(N) <type>".
        sub = args.pop("subtitle", None) or ptype or "panel"
        renderer = _DASHBOARD_PANEL_RENDERERS.get(ptype)
        if renderer is None or data is None:
            ax.text(0.5, 0.5, f"(unrenderable: {ptype or 'untyped'})",
                    transform=ax.transAxes, ha="center", va="center",
                    color="#888888")
            ax.set_xticks([])
            ax.set_yticks([])
        else:
            try:
                renderer(ax, data, args)
            except (ValueError, KeyError, TypeError) as e:
                ax.clear()
                ax.text(0.5, 0.5, f"(error: {e!s:.60s})",
                        transform=ax.transAxes, ha="center", va="center",
                        color="#888888")
                ax.set_xticks([])
                ax.set_yticks([])
        ax.set_title(f"({cell_idx + 1}) {sub}",
                     fontsize=THEME.get("fonts", {}).get("title_pt", 9))

    fig.tight_layout(rect=(0, 0, 1, 0.97))
    return savefig(fig, out)


# ── dashboard_grid panel renderers ────────────────────────────────────────
# Each renderer draws into the supplied axes using the same column
# contracts as the standalone primitive. Kept lean (scatter / line /
# hbar shapes) so byte-determinism within a panel is identical to the
# standalone version. Extends as Phase K composite needs grow.
def _panel_volcano(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    log_fc = np.asarray(_col(frame, args.get("lfc_col", "log_fc")), dtype=float)
    nlogp = np.asarray(_col(frame, args.get("nlogp_col", "neg_log10_p")), dtype=float)
    fc_t = float(args.get("fc_threshold", 1.0))
    p_t = float(args.get("p_threshold", 1.3))
    palette = THEME.get("palette", {})
    sig = (np.abs(log_fc) >= fc_t) & (nlogp >= p_t)
    colors = np.where(sig & (log_fc > 0), palette.get("sig_up", "#D55E00"),
                      np.where(sig & (log_fc < 0),
                               palette.get("sig_down", "#0072B2"),
                               palette.get("non_sig", "#999999")))
    ax.scatter(log_fc, nlogp, c=colors, s=6, alpha=0.75, linewidths=0)
    ax.axhline(p_t, color="#444444", linestyle="--", linewidth=0.5)
    ax.axvline(fc_t, color="#444444", linestyle="--", linewidth=0.5)
    ax.axvline(-fc_t, color="#444444", linestyle="--", linewidth=0.5)
    ax.set_xlabel("log₂FC")
    ax.set_ylabel("−log₁₀ p")


def _panel_manhattan(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    chrom = np.asarray(_col(frame, "chrom"), dtype=object)
    pos = np.asarray(_col(frame, "pos"), dtype=np.int64)
    nlogp = _col(frame, "neg_log10_p", optional=True)
    if nlogp is None:
        pvalue = _col(frame, "pvalue").astype(float)
        nlogp = -np.log10(np.clip(pvalue, 1e-300, 1.0))
    else:
        nlogp = nlogp.astype(float)
    palette = THEME.get("palette", {})
    chrom_str = np.asarray([str(c) for c in chrom])
    ordered = sorted(set(chrom_str.tolist()), key=_chrom_sort_key)
    cursor = 0.0
    vx = np.zeros(len(pos), dtype=float)
    pc = np.full(len(pos), palette.get("non_sig", "#999999"), dtype=object)
    alt = (palette.get("non_sig", "#999999"), "#444444")
    for i, c in enumerate(ordered):
        m = chrom_str == c
        if not m.any():
            continue
        vx[m] = cursor + pos[m].astype(float)
        pc[m] = alt[i % 2]
        cursor += float(pos[m].max()) + 1.0
    ax.scatter(vx, nlogp, c=pc, s=3, alpha=0.75, linewidths=0)
    sig_t = float(args.get("sig_threshold", -np.log10(5e-8)))
    ax.axhline(sig_t, color=palette.get("sig_up", "#D55E00"),
               linestyle="-", linewidth=0.5)
    ax.set_xlabel("genomic position")
    ax.set_ylabel("−log₁₀ p")


def _panel_kaplan_meier(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    time_col = args.get("time_col", "time")
    event_col = args.get("event_col", "event")
    group_col = args.get("group_col")
    time = _col(frame, time_col).astype(float)
    event = _col(frame, event_col).astype(int)
    if group_col:
        group = _col(frame, group_col).astype(object)
        levels = list(dict.fromkeys(str(g) for g in group))
        groups = [(lvl, np.asarray([str(g) == lvl for g in group])) for lvl in levels]
    else:
        groups = [(None, np.ones(len(time), dtype=bool))]
    colors = categorical_palette(max(len(groups), 1))
    for (label, mask), color in zip(groups, colors):
        ev_times, surv, _at_risk = _km_estimator(time[mask], event[mask])
        x = np.concatenate(([0.0], ev_times))
        y = np.concatenate(([1.0], surv))
        ax.step(x, y, where="post", color=color, linewidth=1.0,
                label=str(label) if label is not None else None)
    if group_col:
        ax.legend(loc="best", frameon=False,
                  fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.set_xlabel("time")
    ax.set_ylabel("S(t)")
    ax.set_ylim(0, 1.05)


def _panel_forest(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    label = np.asarray(_col(frame, args.get("label_col", "label")), dtype=object)
    estimate = np.asarray(_col(frame, args.get("estimate_col", "estimate")), dtype=float)
    lower = np.asarray(_col(frame, args.get("lower_col", "lower")), dtype=float)
    upper = np.asarray(_col(frame, args.get("upper_col", "upper")), dtype=float)
    null_value = float(args.get("null_value", 0.0))
    palette = THEME.get("palette", {})
    color = palette.get("sig_up", "#D55E00")
    y = np.arange(len(label))
    ax.errorbar(estimate, y,
                xerr=[estimate - lower, upper - estimate],
                fmt="s", color=color, ecolor=color, elinewidth=0.8,
                markersize=4, capsize=2, linewidth=0)
    ax.axvline(null_value, color="#444444", linestyle="--", linewidth=0.4)
    ax.set_yticks(y)
    ax.set_yticklabels([str(lab) for lab in label],
                       fontsize=THEME.get("fonts", {}).get("legend_pt", 7))
    ax.invert_yaxis()
    ax.set_xlabel(args.get("estimate_col", "estimate"))


def _panel_ma_plot(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    mean = np.asarray(_col(frame, args.get("mean_col", "base_mean")), dtype=float)
    lfc = np.asarray(_col(frame, args.get("lfc_col", "log2FoldChange")), dtype=float)
    padj = np.asarray(_col(frame, args.get("padj_col", "padj")), dtype=float)
    p_t = float(args.get("padj_threshold", 0.05))
    fc_t = float(args.get("fc_threshold", 1.0))
    palette = THEME.get("palette", {})
    log_mean = np.log10(np.clip(mean, 1e-9, None))
    sig = (padj < p_t) & (np.abs(lfc) >= fc_t)
    colors = np.where(sig & (lfc > 0), palette.get("sig_up", "#D55E00"),
                      np.where(sig & (lfc < 0),
                               palette.get("sig_down", "#0072B2"),
                               palette.get("non_sig", "#999999")))
    ax.scatter(log_mean, lfc, c=colors, s=5, alpha=0.75, linewidths=0)
    ax.axhline(0.0, color="#444444", linewidth=0.4)
    ax.set_xlabel("log₁₀ mean")
    ax.set_ylabel("log₂FC")


def _panel_forecast_ribbon(ax: "plt.Axes", frame: Any, args: Dict[str, Any]) -> None:
    time_col = args.get("time_col", "time")
    value_col = args.get("value_col", "forecast")
    lower_col = args.get("lower_col", "lower")
    upper_col = args.get("upper_col", "upper")
    actual_col = args.get("actual_col", "actual")
    time = _col(frame, time_col)
    value = _col(frame, value_col).astype(float)
    lower = _col(frame, lower_col).astype(float)
    upper = _col(frame, upper_col).astype(float)
    actual = _col(frame, actual_col, optional=True) if actual_col else None
    palette = THEME.get("palette", {})
    try:
        order = np.argsort(time, kind="stable")
    except TypeError:
        order = np.arange(len(time))
    t = np.asarray(time)[order]
    ax.fill_between(t, lower[order], upper[order],
                    color=palette.get("non_sig", "#999999"), alpha=0.30,
                    linewidth=0)
    ax.plot(t, value[order], color=palette.get("sig_up", "#D55E00"),
            linewidth=1.2)
    if actual is not None:
        ax.plot(t, np.asarray(actual, dtype=float)[order],
                color=palette.get("sig_down", "#0072B2"),
                linewidth=0.9, alpha=0.85)
    ax.set_xlabel(time_col)
    ax.set_ylabel(value_col)


_DASHBOARD_PANEL_RENDERERS: Dict[str, Callable[["plt.Axes", Any, Dict[str, Any]], None]] = {
    "volcano": _panel_volcano,
    "manhattan": _panel_manhattan,
    "kaplan_meier": _panel_kaplan_meier,
    "forest": _panel_forest,
    "ma_plot": _panel_ma_plot,
    "forecast_ribbon": _panel_forecast_ribbon,
}
