#!/usr/bin/env python3
"""End-to-end ECAA v0.1 RDF projection + SHACL validation.

Takes a package directory, projects all 8 sub-graph sidecars (the 6 JSONL
streams plus the C and A single-document JSON files) plus a synthesized
`ecaa:Package` node through ecaa-v0.1.jsonld into RDF (via the shared
`_project.project` helper), serializes that ABox to `<pkg_dir>/package.ttl`,
then validates against ecaa-v0.1.shacl.ttl (SHACL) with the ecaa-v0.1.ttl
ontology supplied as the inference graph.

Usage:
    python3 project_package.py <package_dir>

Dependencies (pip install --user):
    pyld rdflib pyshacl
"""
import sys
from pathlib import Path

if len(sys.argv) != 2:
    print("usage: project_package.py <package_dir>", file=sys.stderr)
    sys.exit(2)

pkg_dir = Path(sys.argv[1])
spec_dir = Path(__file__).parent.parent.parent / "docs" / "ecaa-spec"

# `_project` owns the import-error message + sys.exit(2) for pyld/rdflib.
from _project import project, write_package_ttl, Graph  # noqa: E402

try:
    import pyshacl
except ImportError as e:
    print(
        f"ERROR: missing dependency ({e}). pip install --user pyld rdflib pyshacl",
        file=sys.stderr,
    )
    sys.exit(2)

# Build the package ABox (8 sidecars + synthesized ecaa:Package node). The
# helper logs the per-sidecar progress lines (`skip (absent): …`,
# `package node: …`) on stdout, matching the historical output the
# conformance tests parse.
projected = project(pkg_dir, log=print)
records_seen = getattr(projected, "ecaa_records_seen", 0)

# Serialize the typed-node ABox to package.ttl (§8.5 deliverable). Best-effort:
# a serialization failure is a warning, not a fatal — the SHACL gate below is
# what determines the exit code.
try:
    write_package_ttl(pkg_dir, projected, log=print)
except Exception as exc:  # noqa: BLE001
    print(f"  WARN: package.ttl serialization failed: {exc}", file=sys.stderr)

print(f"projected: {len(projected)} RDF triples")
if records_seen and len(projected) == 0:
    print(
        "ERROR: package sidecars contained records but projected zero RDF triples",
        file=sys.stderr,
    )
    sys.exit(1)

# Load ontology (inference graph).
onto = Graph()
onto.parse(spec_dir / "ecaa-v0.1.ttl", format="turtle")

# Load SHACL shapes.
shapes = Graph()
shapes.parse(spec_dir / "ecaa-v0.1.shacl.ttl", format="turtle")

# Run SHACL.
conforms, _, report = pyshacl.validate(
    data_graph=projected,
    shacl_graph=shapes,
    ont_graph=onto,
    inference="rdfs",
    debug=False,
)
print(f"SHACL conformance: {'PASS' if conforms else 'FAIL'}")
if not conforms:
    print(report)
    sys.exit(1)
