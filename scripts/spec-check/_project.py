#!/usr/bin/env python3
"""Shared ECAA v0.1 package → RDF projection.

Factored out of `project_package.py` so the SHACL path
(`project_package.py`) and the OWL-DL path (`owl_consistency.py`) build the
package ABox the same way. Both import `project(pkg_dir) -> rdflib.Graph`.

The projection takes a package directory and runs every present sub-graph
sidecar (the 6 JSONL streams + the C and A single-document JSON files)
through `ecaa-v0.1.jsonld`, plus a synthesized `ecaa:Package` node built
from the RO-Crate descriptor's `conformsTo` profile IRIs. The result is the
package ABox the OWL ontology + SHACL shapes are evaluated against.

Spec-node stamping (`@type` for the method-choice decision kinds, the
`ecaa:Package` focus node) lives here so it is identical on both paths —
without `@type` the SHACL shapes have zero focus nodes and pass vacuously,
and OWL has no individuals to check against the disjointness axioms.

Dependencies (pip install --user --break-system-packages):
    pyld rdflib
"""
import json
import sys
from pathlib import Path

# `Graph` is re-exported so callers can `from _project import Graph, project`
# without importing rdflib directly and duplicating the import-error message.
try:
    from pyld import jsonld
    from rdflib import Graph
except ImportError as e:  # pragma: no cover - exercised only without deps
    print(
        f"ERROR: missing dependency ({e}). pip install --user pyld rdflib",
        file=sys.stderr,
    )
    sys.exit(2)


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

# The 6 JSONL sub-graph sidecars, keyed by §3.4 sub-graph letter.
SIDECAR_MAP = {
    "I": "runtime/intake-conversation.jsonl",
    "D": "runtime/decisions.jsonl",
    "E": "runtime/validation-reports.jsonl",
    "V": "runtime/proofs.jsonl",
    "Q": "runtime/verifier-decisions.jsonl",
    "F": "runtime/assumptions.jsonl",
}


def _spec_dir():
    return Path(__file__).parent.parent.parent / "docs" / "ecaa-spec"


def load_context():
    """Load the JSON-LD `@context` block once."""
    return json.load(open(_spec_dir() / "ecaa-v0.1.jsonld"))


def project_decision_record(entry, fallback_id):
    """Stamp a spec node `type` (and lift justification fields) onto a D record.

    A record already carrying a top-level `type` (hand-authored fixtures, or
    future C1-projected output) is returned unchanged. Otherwise, when the
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
        node["id"] = f"ecaa:decision:{entry.get('timestamp', fallback_id)}"
    return node


def conforms_to_iris(metadata):
    """Extract the package-level `conformsTo` profile IRIs from an
    ro-crate-metadata.json document.

    The §3.1 profile IRIs may be carried on the root Dataset (`@id == "./"`)
    OR on the metadata-descriptor node (`@id == "ro-crate-metadata.json"`) —
    the spec names the root dataset, but the reference emitter currently
    places them on the descriptor. Union both carriers so the synthesized
    `ecaa:Package` node sees the full set regardless of placement (the prior
    `./ or descriptor` short-circuit silently dropped them when `./` existed
    but was empty). Each entry is either `{"@id": iri}` or a bare string.
    Returns a de-duplicated, order-preserving list.
    """
    graph = metadata.get("@graph", [])
    by_id = {node.get("@id"): node for node in graph if isinstance(node, dict)}
    out = []
    for carrier_id in ("./", "ro-crate-metadata.json"):
        carrier = by_id.get(carrier_id) or {}
        raw = carrier.get("conformsTo", [])
        if isinstance(raw, dict):
            raw = [raw]
        for item in raw:
            iri = item.get("@id") if isinstance(item, dict) else item
            if iri and iri not in out:
                out.append(iri)
    return out


def _to_rdf(graph, doc, context_label=None):
    """Project one JSON-LD `doc` into `graph`; warn (not raise) on failure.

    Warnings go to stderr (matching the historical `project_package.py`
    behavior) so they never pollute the stdout lines the conformance tests
    parse (`projected: N RDF triples`, `SHACL conformance: …`).
    """
    try:
        rdf = jsonld.to_rdf(doc, options={"format": "application/n-quads"})
        graph.parse(data=rdf, format="nquads")
        return True
    except Exception as exc:  # noqa: BLE001 - any projection failure is a warn
        ctx = f" [{context_label}]" if context_label else ""
        print(f"  WARN:{ctx} {exc}", file=sys.stderr)
        return False


def project(pkg_dir, log=print):
    """Project a package directory into an rdflib `Graph` (the ABox).

    `log` receives human-readable progress lines (defaults to stdout `print`,
    matching the historical `project_package.py` output that the
    `shacl_non_vacuous.rs` test parses); pass `log=None` to silence.

    Returns the populated `Graph`. The `records_seen` count is exposed on the
    returned graph as the `ecaa_records_seen` attribute so the caller can
    detect the records-but-zero-triples vacuity case.
    """
    pkg_dir = Path(pkg_dir)
    ctx = load_context()
    projected = Graph()
    records_seen = 0

    for letter, rel in SIDECAR_MAP.items():
        p = pkg_dir / rel
        if not p.exists():
            if log is not None:
                log(f"  skip (absent): {rel}")
            continue
        with open(p) as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                entry = json.loads(line)
                records_seen += 1
                if letter == "D":
                    entry = project_decision_record(entry, records_seen)
                entry["@context"] = ctx["@context"]
                _to_rdf(projected, entry, context_label=rel)

    # C is a single document.
    c_path = pkg_dir / "runtime/claim-verification.json"
    if c_path.exists():
        c_doc = json.load(open(c_path))
        records_seen += 1
        c_doc["@context"] = ctx["@context"]
        _to_rdf(projected, c_doc, context_label="runtime/claim-verification.json")

    # A (audit-proof) is a single document — the report plus its embedded
    # InvariantVerdict array. One JSON-LD document, not a JSONL stream.
    a_path = pkg_dir / "runtime/audit-proof-report.json"
    if a_path.exists():
        a_doc = json.load(open(a_path))
        records_seen += 1
        a_doc["@context"] = ctx["@context"]
        _to_rdf(projected, a_doc, context_label="runtime/audit-proof-report.json")
    elif log is not None:
        log("  skip (absent): runtime/audit-proof-report.json")

    # Synthesize the ecaa:Package focus node from the RO-Crate descriptor's
    # conformsTo profile IRIs so SubstrateValidityShape (Invariant 6) binds.
    # Without a node typed ecaa:Package the shape has zero focus nodes and
    # SHACL passes vacuously. The conformsTo IRIs come from the single source
    # of truth, ro-crate-metadata.json, rather than being hard-coded here.
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
        if _to_rdf(projected, package_node, context_label="ro-crate-metadata.json") and log is not None:
            log(f"  package node: ecaa:Package with {len(iris)} conformsTo IRIs")
    elif log is not None:
        log("  skip (absent): ro-crate-metadata.json (no ecaa:Package focus node)")

    # Stash the records count so callers can detect the
    # records-but-zero-triples vacuity without re-walking the sidecars.
    try:
        projected.ecaa_records_seen = records_seen
    except Exception:  # pragma: no cover - rdflib Graph allows attrs
        pass
    return projected


def write_package_ttl(pkg_dir, graph, log=print):
    """Serialize `graph` to `<pkg_dir>/package.ttl` (Turtle).

    The serialized ABox is the §8.5 deliverable: a typed-node Turtle dump of
    the package the external validators consumed. Returns the written path.
    """
    out = Path(pkg_dir) / "package.ttl"
    out.write_text(graph.serialize(format="turtle"))
    if log is not None:
        log(f"  wrote package.ttl: {out}")
    return out
