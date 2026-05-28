"""Modality coverage assertions.

Walks ``config/stage-atoms/`` and ``config/archetypes/`` and confirms
each ``required_figures: [...]`` entry resolves to a registered
primitive in the matching ``lib/plotting/stages/<plot_stage_id>.py``
module. Closes plan §S14.6 + §S14.8 — the gap where an atom or
archetype YAML can name a figure id that no longer maps to anything
renderable.

Run with: ``pytest lib/plotting/tests/test_modality_coverage.py``.

Note: the prior taxonomy-driven walker (over the retired
``config/stage-taxonomies/`` directory) was removed at the A.S5 cutover.
Coverage now flows from atoms + archetypes — the v4 composer's catalog —
which together exercise every keyword-routable modality.
"""

from __future__ import annotations

import importlib
import sys
from pathlib import Path
from typing import Dict, List, Set

import yaml

ROOT = Path(__file__).resolve().parents[3]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

ATOMS_DIR = ROOT / "config" / "stage-atoms"
ARCHETYPES_DIR = ROOT / "config" / "archetypes"


def _load_yaml(path: Path) -> dict:
    with open(path, "rt") as f:
        return yaml.safe_load(f) or {}


def _stage_module_path(stage_id: str) -> Path:
    return ROOT / "lib" / "plotting" / "stages" / f"{stage_id}.py"


def _registered_figures(stage_id: str) -> Set[str]:
    try:
        mod = importlib.import_module(f"lib.plotting.stages.{stage_id}")
    except ImportError:
        return set()
    figures = getattr(mod, "FIGURES", None)
    if figures is None:
        return set()
    return set(figures.figures.keys()) if hasattr(figures, "figures") else set(figures)


def _walk_stage_atoms() -> List[dict]:
    rows: List[dict] = []
    for path in sorted(ATOMS_DIR.glob("*.yaml")):
        if path.name.startswith("_"):
            continue
        atom = _load_yaml(path)
        atom_id = atom.get("id", path.stem)
        req = atom.get("required_figures")
        if not isinstance(atom_id, str) or not isinstance(req, list) or not req:
            continue
        plot_stage_id = atom.get("plot_stage_id") or atom_id
        rows.append(
            {
                "atom_id": atom_id,
                "plot_stage_id": str(plot_stage_id),
                "required_figures": [str(f) for f in req if isinstance(f, str)],
            }
        )
    return rows


def _load_atom_catalog() -> Dict[str, dict]:
    catalog: Dict[str, dict] = {}
    for path in sorted(ATOMS_DIR.glob("*.yaml")):
        if path.name.startswith("_"):
            continue
        atom = _load_yaml(path)
        atom_id = atom.get("id", path.stem)
        if isinstance(atom_id, str):
            catalog[atom_id] = atom
    return catalog


def _walk_archetype_atom_figures() -> List[dict]:
    catalog = _load_atom_catalog()
    rows: List[dict] = []

    def append_rows(arch_id: str, atoms: list) -> None:
        if not isinstance(atoms, list):
            return
        for ref in atoms:
            if not isinstance(ref, dict):
                continue
            atom_id = ref.get("atom_id")
            if not isinstance(atom_id, str):
                continue
            atom = catalog.get(atom_id, {})
            req = (ref["required_figures"] if "required_figures" in ref
                   else atom.get("required_figures"))
            if not isinstance(req, list) or not req:
                continue
            plot_stage_id = ref.get("plot_stage_id") or atom.get("plot_stage_id") or atom_id
            rows.append(
                {
                    "archetype": arch_id,
                    "atom_id": atom_id,
                    "stage_id": str(ref.get("alias") or atom_id),
                    "plot_stage_id": str(plot_stage_id),
                    "required_figures": [str(f) for f in req if isinstance(f, str)],
                }
            )

    for path in sorted(ARCHETYPES_DIR.glob("*.yaml")):
        if path.name.startswith("_") or path.name.endswith(".slots.yaml"):
            continue
        arch = _load_yaml(path)
        arch_id = arch.get("id", path.stem)
        atoms = arch.get("atoms") or []
        if not isinstance(arch_id, str) or not isinstance(atoms, list):
            continue
        append_rows(arch_id, atoms)

        slots_path = path.with_name(f"{path.stem}.slots.yaml")
        if not slots_path.exists():
            continue
        slots = _load_yaml(slots_path)
        values = slots.get("values") or []
        if not isinstance(values, list):
            continue
        for value in values:
            if not isinstance(value, dict):
                continue
            slot_id = value.get("id")
            if not isinstance(slot_id, str) or slot_id == "generic":
                continue
            extra_atoms = value.get("extra_atoms") or []
            if not isinstance(extra_atoms, list) or not extra_atoms:
                continue
            append_rows(f"{arch_id}_{slot_id}", atoms + extra_atoms)
    return rows


def test_stage_atom_required_figures_resolve_via_plot_stage_id() -> None:
    rows = _walk_stage_atoms()
    missing: List[str] = []
    for row in rows:
        registered = _registered_figures(row["plot_stage_id"])
        for fig in row["required_figures"]:
            if fig not in registered:
                missing.append(
                    f"{row['atom_id']} routes to {row['plot_stage_id']} "
                    f"but '{fig}' is not registered; exports {sorted(registered)}"
                )
    assert not missing, (
        "Stage atom required_figures without renderers:\n  "
        + "\n  ".join(missing)
    )


def test_archetype_plottable_atom_refs_resolve_after_aliasing() -> None:
    rows = _walk_archetype_atom_figures()
    missing: List[str] = []
    for row in rows:
        registered = _registered_figures(row["plot_stage_id"])
        for fig in row["required_figures"]:
            if fig not in registered:
                missing.append(
                    f"{row['archetype']}:{row['stage_id']} ({row['atom_id']}) "
                    f"routes to {row['plot_stage_id']} but '{fig}' is not registered"
                )
    assert not missing, (
        "Archetype atom figure contracts without renderers:\n  "
        + "\n  ".join(missing)
    )


def test_cross_omics_archetypes_have_nonempty_plot_contracts() -> None:
    rows = _walk_archetype_atom_figures()
    by_arch: Dict[str, List[dict]] = {}
    for row in rows:
        if row["archetype"].startswith("cross_omics_"):
            by_arch.setdefault(row["archetype"], []).append(row)

    required_arches = {
        "cross_omics_rnaseq_proteomics",
        "cross_omics_rnaseq_proteomics_mofa",
        "cross_omics_rnaseq_proteomics_snf",
        "cross_omics_rnaseq_proteomics_diablo",
    }
    missing_arches = sorted(a for a in required_arches if not by_arch.get(a))
    assert not missing_arches, (
        "Cross-omics archetypes missing plot contracts: "
        + ", ".join(missing_arches)
    )

    thematic = [
        row for row in by_arch["cross_omics_rnaseq_proteomics"]
        if row["stage_id"] == "cross_omics_thematic_comparison"
    ]
    assert thematic and set(thematic[0]["required_figures"]) == {
        "concordance_heatmap",
        "pathway_overlap_bar",
    }
    for arch in [
        "cross_omics_rnaseq_proteomics_mofa",
        "cross_omics_rnaseq_proteomics_snf",
        "cross_omics_rnaseq_proteomics_diablo",
    ]:
        assert any(
            row["plot_stage_id"] == "multi_omics_integration"
            and {"modality_concordance_heatmap", "factor_variance_bar"}.issubset(
                set(row["required_figures"])
            )
            for row in by_arch[arch]
        ), f"{arch} missing multi-omics integration figures"
