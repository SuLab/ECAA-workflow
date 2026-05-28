"""Stage modules. Each module defines a module-level FIGURES registry and
one function per figure_id. Core's generate() imports by stage_id and
dispatches.

New stages: copy one of the existing modules as a template. Do NOT import
matplotlib directly — go through core.savefig / core.violin / core.bar /
core.scatter / core.volcano / core.heatmap so determinism guarantees stay
uniform across the package.
"""
