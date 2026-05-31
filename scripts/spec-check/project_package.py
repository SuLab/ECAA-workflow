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


# Decision `kind` values (the `#[serde(tag = "kind")]` discriminator on
# DecisionRecord) that denote a methodological choice and therefore project
# to an `ecaa:MethodChoice` node — the focus node DecisionJustificationShape
# (Invariant 2) targets. This is the narrow, additive complement to the full
# C1 typed-node projection: it stamps `@type` + lifts `rationale`/`cites` for
# the method-choice decision kinds so the SHACL shape binds against real
# emitted packages. Hand-authored fixtures that already carry a top-level
# `type` pass through untouched (see `project_decision_record`).
METHOD_CHOICE_DECISION_KINDS = {
    "amend_stage_method",
    "select_sensitivity_winner",
    "set_intake_method",
}


def project_decision_record(entry):
    """Stamp a spec node `type` (and lift justification fields) onto a D record.

    A record already carrying a top-level `type` (hand-authored fixtures,
    or future C1-projected output) is returned unchanged. Otherwise, when the
    nested `decision.kind` is a method-choice kind, the record is rewritten to
    a typed `ecaa:MethodChoice` JSON-LD node with top-level `rationale` (and
    `cites`, if present) so DecisionJustificationShape has a focus node.
    Records that are not method choices are returned unchanged (they project
    to plain nodes that no shape targets).
    """
    if "type" in entry:
        return entry
    decision = entry.get("decision")
    if not isinstance(decision, dict):
        return entry
    if decision.get("kind") not in METHOD_CHOICE_DECISION_KINDS:
        return entry
    node = dict(entry)
    node["type"] = "MethodChoice"
    if "rationale" in entry:
        node["rationale"] = entry["rationale"]
    # A `cites` edge may live on the decision payload (e.g. a Citation IRI the
    # SME named when choosing the method); surface it at the node level.
    cites = decision.get("cites")
    if cites is not None:
        node["cites"] = cites
    if "id" not in node:
        node["id"] = f"ecaa:decision:{entry.get('timestamp', records_seen)}"
    return node


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
            if letter == "D":
                entry = project_decision_record(entry)
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


def conforms_to_iris(metadata):
    """Extract the package-level `conformsTo` profile IRIs from an
    ro-crate-metadata.json document.

    The IRIs live on the root Dataset (`@id == "./"`); fall back to the
    metadata-descriptor node. Each entry is either `{"@id": iri}` or a bare
    string. Returns a de-duplicated, order-preserving list.
    """
    graph = metadata.get("@graph", [])
    by_id = {node.get("@id"): node for node in graph if isinstance(node, dict)}
    carrier = by_id.get("./") or by_id.get("ro-crate-metadata.json") or {}
    raw = carrier.get("conformsTo", [])
    if isinstance(raw, dict):
        raw = [raw]
    out = []
    for item in raw:
        iri = item.get("@id") if isinstance(item, dict) else item
        if iri and iri not in out:
            out.append(iri)
    return out


# Synthesize the ecaa:Package focus node from the RO-Crate descriptor's
# conformsTo profile IRIs so SubstrateValidityShape (Invariant 6) binds.
# Without a node typed ecaa:Package the shape has zero focus nodes and SHACL
# passes vacuously. The conformsTo IRIs are read from ro-crate-metadata.json
# (the single source of truth) rather than hard-coded here.
metadata_path = pkg_dir / "ro-crate-metadata.json"
if metadata_path.exists():
    metadata = json.load(open(metadata_path))
    iris = conforms_to_iris(metadata)
    package_node = {
        "@context": ctx["@context"],
        "id": "ecaa:package",
        "type": "Package",
        "conformsTo": iris,
    }
    try:
        rdf = jsonld.to_rdf(package_node, options={"format": "application/n-quads"})
        projected.parse(data=rdf, format="nquads")
        print(f"  package node: ecaa:Package with {len(iris)} conformsTo IRIs")
    except Exception as exc:
        print(f"  WARN: ro-crate-metadata.json package node: {exc}", file=sys.stderr)
else:
    print("  skip (absent): ro-crate-metadata.json (no ecaa:Package focus node)")

# DEFERRED (C5 Task 3): serialize this graph to `<pkg_dir>/package.ttl`
# (`projected.serialize(format="turtle")`), factor a shared `project(pkg_dir)
# -> Graph` helper into `_project.py` for `owl_consistency.py` to reuse over
# the package ABox, and default the conformance build to Full + block-on-fail.
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
