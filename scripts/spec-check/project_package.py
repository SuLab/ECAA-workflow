#!/usr/bin/env python3
"""End-to-end ECAA v0.1 RDF projection + OWL DL + SHACL validation.

Takes a package directory, projects all 8 sub-graph sidecars (the 6
JSONL streams plus the C and A single-document JSON files) through
ecaa-v0.1.jsonld into RDF, then validates against ecaa-v0.1.ttl
(OWL DL) and ecaa-v0.1.shacl.ttl (SHACL).

Usage:
    python3 project_package.py <package_dir>

Dependencies (pip install --user):
    pyld rdflib pyshacl
"""
import json
import sys
from pathlib import Path

if len(sys.argv) != 2:
    print("usage: project_package.py <package_dir>", file=sys.stderr)
    sys.exit(2)

pkg_dir = Path(sys.argv[1])
spec_dir = Path(__file__).parent.parent.parent / "docs" / "ecaa-spec"

try:
    from pyld import jsonld
    from rdflib import Graph
    import pyshacl
except ImportError as e:
    print(
        f"ERROR: missing dependency ({e}). pip install --user pyld rdflib pyshacl",
        file=sys.stderr,
    )
    sys.exit(2)

ctx = json.load(open(spec_dir / "ecaa-v0.1.jsonld"))

# Project JSONL sidecars to RDF
projected = Graph()
records_seen = 0
sidecar_map = {
    "I": "runtime/intake-conversation.jsonl",
    "D": "runtime/decisions.jsonl",
    "E": "runtime/validation-reports.jsonl",
    "V": "runtime/proofs.jsonl",
    "Q": "runtime/verifier-decisions.jsonl",
    "F": "runtime/assumptions.jsonl",
}
for letter, rel in sidecar_map.items():
    p = pkg_dir / rel
    if not p.exists():
        print(f"  skip (absent): {rel}")
        continue
    with open(p) as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            entry = json.loads(line)
            records_seen += 1
            entry["@context"] = ctx["@context"]
            try:
                rdf = jsonld.to_rdf(entry, options={"format": "application/n-quads"})
                projected.parse(data=rdf, format="nquads")
            except Exception as exc:
                print(f"  WARN: {rel} line {entry.get('id', '?')}: {exc}", file=sys.stderr)

# C is a single document
c_path = pkg_dir / "runtime/claim-verification.json"
if c_path.exists():
    c_doc = json.load(open(c_path))
    records_seen += 1
    c_doc["@context"] = ctx["@context"]
    rdf = jsonld.to_rdf(c_doc, options={"format": "application/n-quads"})
    projected.parse(data=rdf, format="nquads")

# A (audit-proof) is a single document — the report itself plus its
# embedded InvariantVerdict array. Mirrors the C sidecar shape: one
# JSON-LD document, not a JSONL stream.
a_path = pkg_dir / "runtime/audit-proof-report.json"
if a_path.exists():
    a_doc = json.load(open(a_path))
    records_seen += 1
    a_doc["@context"] = ctx["@context"]
    try:
        rdf = jsonld.to_rdf(a_doc, options={"format": "application/n-quads"})
        projected.parse(data=rdf, format="nquads")
    except Exception as exc:
        print(f"  WARN: runtime/audit-proof-report.json: {exc}", file=sys.stderr)
else:
    print("  skip (absent): runtime/audit-proof-report.json")

print(f"projected: {len(projected)} RDF triples")
if records_seen and len(projected) == 0:
    print(
        "ERROR: package sidecars contained records but projected zero RDF triples",
        file=sys.stderr,
    )
    sys.exit(1)

# Load ontology
onto = Graph()
onto.parse(spec_dir / "ecaa-v0.1.ttl", format="turtle")

# Load SHACL shapes
shapes = Graph()
shapes.parse(spec_dir / "ecaa-v0.1.shacl.ttl", format="turtle")

# Run SHACL
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
