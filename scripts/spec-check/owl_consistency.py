#!/usr/bin/env python3
"""Verify ECAA v0.2 OWL-DL satisfiability using HermiT via owlready2.

Two modes:

    python3 owl_consistency.py
        Static check: the ecaa-v0.2.ttl ontology alone is OWL-DL-satisfiable.

    python3 owl_consistency.py <package_dir>
        ABox check: build the package's individuals via the shared
        `_project.project(pkg_dir)` helper, MERGE them with the static
        ecaa-v0.2.ttl ontology, and run HermiT over ontology + individuals so
        a node typed against a disjointness axiom surfaces as an
        inconsistency. (Without the package the reasoner only proves the
        ontology is self-consistent, never that the ABox is.)

owlready2's default loader handles RDF/XML and NTriples but is finicky with
Turtle. We assemble the combined graph in rdflib, serialize to RDF/XML, write
to a temp file, then load with owlready2 for HermiT reasoning.

Dependencies (pip install --user --break-system-packages):
    owlready2 rdflib  (+ pyld when a <package_dir> is supplied)
"""
import sys
import tempfile
from pathlib import Path

ttl_path = Path(__file__).parent.parent.parent / "docs" / "ecaa-spec" / "ecaa-v0.2.ttl"

try:
    from rdflib import Graph
    from owlready2 import get_ontology, sync_reasoner, OwlReadyInconsistentOntologyError
except ImportError as e:
    print(
        f"ERROR: {e}. pip install --user --break-system-packages owlready2 rdflib",
        file=sys.stderr,
    )
    sys.exit(2)

# Optional <package_dir>: when present, merge its ABox into the ontology.
pkg_dir = None
if len(sys.argv) == 2:
    pkg_dir = Path(sys.argv[1])
elif len(sys.argv) > 2:
    print("usage: owl_consistency.py [<package_dir>]", file=sys.stderr)
    sys.exit(2)

# Start from the static ontology (TBox).
g = Graph()
g.parse(ttl_path, format="turtle")
print(f"parsed: {len(g)} triples from {ttl_path.name}")

# Merge the package ABox (individuals) when a package dir is supplied. The
# shared `_project.project` helper builds the identical graph used on the
# SHACL path, so OWL and SHACL reason over the same individuals.
if pkg_dir is not None:
    from _project import project  # noqa: E402 - deferred so static mode needs no pyld

    abox = project(pkg_dir, log=print)
    before = len(g)
    g += abox
    print(
        f"merged package ABox: {len(abox)} triples from {pkg_dir} "
        f"(combined: {len(g)}, +{len(g) - before})"
    )

# Round-trip the combined graph TTL/ABox → RDF/XML for owlready2 consumption.
with tempfile.NamedTemporaryFile(suffix=".owl", delete=False, mode="wb") as tmp:
    tmp.write(g.serialize(format="xml").encode())
    tmp_path = tmp.name

onto = get_ontology(f"file://{tmp_path}").load()
try:
    with onto:
        sync_reasoner(infer_property_values=True)
    scope = "ontology + package ABox" if pkg_dir is not None else "ecaa-v0.2.ttl"
    print(
        f"OK: {scope} is OWL-DL-satisfiable "
        f"({len(list(onto.classes()))} classes, "
        f"{len(list(onto.object_properties()))} object properties, "
        f"{len(list(onto.individuals()))} named individuals)"
    )
except OwlReadyInconsistentOntologyError as e:
    scope = "ontology + package ABox" if pkg_dir is not None else "ontology"
    print(f"FAIL: {scope} is inconsistent: {e}", file=sys.stderr)
    sys.exit(1)
