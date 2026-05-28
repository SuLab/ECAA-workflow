#!/usr/bin/env python3
"""Verify ecaa-v0.1.ttl is OWL-DL-satisfiable using HermiT via owlready2.

owlready2's default loader handles RDF/XML and NTriples but is finicky with
Turtle. We convert the TTL to RDF/XML via rdflib first, write to a temp
file, then load with owlready2 for HermiT reasoning.
"""
import sys
import tempfile
from pathlib import Path

ttl_path = Path(__file__).parent.parent.parent / "docs" / "ecaa-spec" / "ecaa-v0.1.ttl"

try:
    from rdflib import Graph
    from owlready2 import get_ontology, sync_reasoner, OwlReadyInconsistentOntologyError
except ImportError as e:
    print(
        f"ERROR: {e}. pip install --user --break-system-packages owlready2 rdflib",
        file=sys.stderr,
    )
    sys.exit(2)

# Round-trip TTL → RDF/XML for owlready2 consumption.
g = Graph()
g.parse(ttl_path, format="turtle")
print(f"parsed: {len(g)} triples from {ttl_path.name}")

with tempfile.NamedTemporaryFile(suffix=".owl", delete=False, mode="wb") as tmp:
    tmp.write(g.serialize(format="xml").encode())
    tmp_path = tmp.name

onto = get_ontology(f"file://{tmp_path}").load()
try:
    with onto:
        sync_reasoner(infer_property_values=True)
    print(
        f"OK: ecaa-v0.1.ttl is OWL-DL-satisfiable "
        f"({len(list(onto.classes()))} classes, "
        f"{len(list(onto.object_properties()))} object properties, "
        f"{len(list(onto.individuals()))} named individuals)"
    )
except OwlReadyInconsistentOntologyError as e:
    print(f"FAIL: ontology is inconsistent: {e}", file=sys.stderr)
    sys.exit(1)
