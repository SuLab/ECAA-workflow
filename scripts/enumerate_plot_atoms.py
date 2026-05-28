#!/usr/bin/env python3
"""Enumerate non-figure_exempt atoms that have an edam_data output and
resolve to a registered plot-affordance entry.

Output: JSON array on stdout, one object per (atom_id, figure_id) pair.
Each object:
  {
    "atom_id": str,
    "semantic_type_iri": str,
    "renderer_module": str,
    "figure_ids": [str, ...],
    "stage_id": str          # module suffix after last '.'
  }

IRI normalisation: registered.yaml stores keys in two formats—
  - compact: "data:3134"
  - prefixed: "EDAM:data_3134"
Atoms use compact form in both legacy `edam_data:` fields and rich-port
`iri:` values. We build the lookup with all keys as-is (the file has
both forms), then try compact first, then the EDAM-prefixed form.

Adapter atoms (id in adapter_registry) are skipped; the adapter list
is approximated by the id patterns used in the Rust test rather than
loading Rust code.
"""
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

import yaml

REPO_ROOT = Path(__file__).resolve().parent.parent
ATOMS_DIR = REPO_ROOT / "config" / "stage-atoms"
AFFORDANCES_FILE = REPO_ROOT / "config" / "plot-affordances" / "registered.yaml"

# Adapter atom id patterns — mirrors adapter_registry heuristic
_ADAPTER_PREFIXES = (
    "bam_sort", "bam_index", "bam_merge", "fastq_merge", "vcf_merge",
    "file_transfer", "data_import", "data_export",
)

def _is_adapter(atom_id: str) -> bool:
    return any(atom_id.startswith(p) for p in _ADAPTER_PREFIXES)


def _load_affordances() -> dict[str, dict]:
    with open(AFFORDANCES_FILE) as f:
        raw = yaml.safe_load(f)
    by_iri: dict[str, dict] = {}
    for entry in raw.get("affordances", []):
        by_iri[entry["semantic_type"]] = entry
    return by_iri


def _edam_alt_forms(iri: str) -> list[str]:
    """Return alternate lookup keys for an IRI so compact and prefixed
    forms both resolve. E.g. "data:3134" → ["data:3134", "EDAM:data_3134"]
    and vice-versa.
    """
    forms = [iri]
    # compact → prefixed: "data:NNNN" → "EDAM:data_NNNN"
    m = re.match(r'^(data|format|operation):(\d+)$', iri)
    if m:
        forms.append(f"EDAM:{m.group(1)}_{m.group(2)}")
    # prefixed → compact: "EDAM:data_NNNN" → "data:NNNN"
    m2 = re.match(r'^EDAM:(data|format|operation)_(\d+)$', iri)
    if m2:
        forms.append(f"{m2.group(1)}:{m2.group(2)}")
    return forms


def _resolve_affordance(iri: str, by_iri: dict) -> dict | None:
    """Direct lookup then alternate-form lookup."""
    for form in _edam_alt_forms(iri):
        if form in by_iri:
            return by_iri[form]
    # BFS over parents (one level — enough for the common case)
    for form in _edam_alt_forms(iri):
        if form in by_iri:
            entry = by_iri[form]
            return entry
        # Try parents of form
        for candidate in list(by_iri.values()):
            if form in _edam_alt_forms(candidate["semantic_type"]):
                return candidate
    return None


def _primary_iri_from_atom(atom: dict) -> str | None:
    """Extract the primary output IRI. Walks rich-port outputs first,
    falls back to legacy edam_data field.
    """
    outputs = atom.get("outputs") or []
    if outputs:
        first = outputs[0]
        st = first.get("semantic_type") or {}
        kind = st.get("kind")
        if kind == "ontology_term":
            return st.get("iri")
        if kind == "local_extension":
            ns = st.get("namespace", "swfc")
            sid = st.get("id", "")
            return f"{ns}:{sid}"
        if kind == "opaque":
            return f"swfc:opaque:{atom.get('id', 'unknown')}"
        # union — skip, treat as opaque
        return f"swfc:union:{atom.get('id', 'unknown')}"
    # Legacy fallback
    return atom.get("edam_data")


def main() -> None:
    by_iri = _load_affordances()

    results = []
    for yaml_path in sorted(ATOMS_DIR.glob("*.yaml")):
        if yaml_path.name.startswith("_"):
            continue  # skip schema sidecars
        with open(yaml_path) as f:
            atom = yaml.safe_load(f)
        if not isinstance(atom, dict):
            continue

        atom_id = atom.get("id", yaml_path.stem)

        # Skip figure_exempt atoms
        if atom.get("figure_exempt") is not None:
            continue

        # Skip adapter atoms
        if _is_adapter(atom_id):
            continue

        iri = _primary_iri_from_atom(atom)
        if not iri:
            continue  # no data product output

        # Resolve affordance
        entry = _resolve_affordance(iri, by_iri)
        if entry is None:
            # Try parent-term walk from the port's proposed_parent_terms
            outputs = atom.get("outputs") or []
            if outputs:
                st = outputs[0].get("semantic_type") or {}
                for parent in st.get("proposed_parent_terms", []):
                    entry = _resolve_affordance(parent, by_iri)
                    if entry:
                        iri = parent
                        break
        if entry is None:
            continue  # Deferred — skip (no renderer to smoke-test)

        renderer_module = entry["renderer_module"]
        # stage_id is the last dotted segment of the renderer_module
        stage_id = renderer_module.rsplit(".", 1)[-1]

        results.append({
            "atom_id": atom_id,
            "semantic_type_iri": iri,
            "renderer_module": renderer_module,
            "figure_ids": list(entry["figure_ids"]),
            "stage_id": stage_id,
        })

    json.dump(results, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
