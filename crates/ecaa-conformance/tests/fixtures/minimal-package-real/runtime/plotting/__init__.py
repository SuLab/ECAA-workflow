"""Plotting library shipped into every emitted package at runtime/plotting/.

Stage modules produce a fixed set of diagnostic figures for each compute
stage. The registry + dispatch pattern below keeps stage modules small and
keeps core primitives (savefig metadata, seed plumbing, headless backend,
theme baseline, palette) in one place so determinism + visual-style
guarantees hold uniformly.

Usage from an agent-generated stage script:

    from runtime.plotting.core import generate
    generate("clustering", outputs_dir, required=["umap_clusters"])

`generate()` dispatches to `runtime.plotting.stages.<stage_id>` and writes
each requested figure as a deterministic PNG plus, by default, a vector
PDF alongside it. Unknown figure ids surface as warnings in the returned
manifest; unknown stage ids return an empty manifest and do NOT raise —
SMEs reviewing a partial run must still be able to see plots for any
stage that registered figures.

The shipped `theme.json` controls fonts, palette, output formats, and the
provenance footer. Re-skin a package by editing that file (for instance
to switch to a journal-specific preset) and re-running `generate()`.
"""

from . import primitives  # noqa: F401 — expose runtime.plotting.primitives subpackage

from .core import (
    THEME,
    FigureManifest,
    FigureRegistry,
    ViewManifest,
    ViewRegistry,
    __version__,
    apply_theme,
    arc,
    available_presets,
    bar,
    categorical_palette,
    coloc_pp_panel,
    credible_set_track,
    extract,
    forest,
    generate,
    glasbey20_palette,
    heatmap,
    load_preset,
    locus_zoom,
    manhattan,
    miami,
    qq,
    register_alias,
    register_figure,
    register_view,
    sankey,
    savefig,
    scatter,
    seeded,
    stage_registry,
    stage_view_registry,
    use_preset,
    violin,
    volcano,
    wong_palette,
)

__all__ = [
    "THEME",
    "FigureManifest",
    "FigureRegistry",
    "ViewManifest",
    "ViewRegistry",
    "__version__",
    "apply_theme",
    "arc",
    "available_presets",
    "bar",
    "categorical_palette",
    "coloc_pp_panel",
    "credible_set_track",
    "extract",
    "forest",
    "generate",
    "glasbey20_palette",
    "heatmap",
    "load_preset",
    "locus_zoom",
    "manhattan",
    "miami",
    "qq",
    "register_alias",
    "register_figure",
    "register_view",
    "sankey",
    "savefig",
    "scatter",
    "seeded",
    "stage_registry",
    "stage_view_registry",
    "use_preset",
    "violin",
    "volcano",
    "wong_palette",
]
