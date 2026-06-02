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


def _strip_fragment(s):
    """Drop any `#fragment` so an evidence reference resolves against the bare
    output path — mirrors the Rust `evidence_coverage::strip_fragment`."""
    return s.split("#", 1)[0]


# Map a Q `RerunOutcome.class` value (as written by the harness classifier,
# `equivalence_failure::DIVERGED_CLASSES` + the non-divergent classes) onto the
# spec §5.6 `ecaa:` individual the Invariant-4 SHACL FILTER tests against
# (`?class IN (ecaa:failed, ecaa:nonDeterministic)`).
_CLASS_IRI = {
    "failed": "failed",
    "acknowledged_non_determinism": "nonDeterministic",
    "non_deterministic": "nonDeterministic",
    "nonDeterministic": "nonDeterministic",
}


def _class_iri(cls):
    """Map a raw class string to its `ecaa:` local name (passthrough on miss)."""
    return _CLASS_IRI.get(cls, cls)


def project_rerun_outcome_row(entry, fallback_id):
    """Stamp `ecaa:RerunOutcome` on a Q (`verifier-decisions.jsonl`) row and
    lift its `class` to the `ecaa:` individual the Invariant-4 SHACL expects.

    A row already carrying a top-level `type` (hand-authored fixtures) passes
    through untouched. The node id is `ecaa:rerun:<id>` so a Blocker's `refs`
    edge (see `project_blocker_row`) resolves to it. Returns the rewritten
    node; the caller stamps `@context`.
    """
    if "type" in entry:
        return entry
    node = dict(entry)
    node["type"] = "RerunOutcome"
    cls = entry.get("class") or entry.get("bucket")
    if cls:
        node["class"] = {"@id": f"ecaa:{_class_iri(cls)}"}
    raw_id = entry.get("id") or entry.get("edge_id") or fallback_id
    node["id"] = f"ecaa:rerun:{raw_id}"
    return node


def project_blocker_row(entry, fallback_id):
    """Stamp `ecaa:Blocker` on an F (`assumptions.jsonl`) row and project its
    `refs` field as an `@id` edge to the referenced Q `RerunOutcome` node.

    The `refs` object IRI is `ecaa:rerun:<refs>` so it equals the
    `RerunOutcome` node id minted by `project_rerun_outcome_row`, letting the
    Invariant-4 `EquivalenceFailureShape` SPARQL bind. `kind` is left as
    written (the spec-canonical CamelCase the SHACL FILTER lists). A row
    already carrying a top-level `type` passes through untouched.
    """
    if "type" in entry:
        return entry
    node = dict(entry)
    node["type"] = "Blocker"
    refs = entry.get("refs")
    if isinstance(refs, str):
        node["refs"] = {"@id": f"ecaa:rerun:{refs}"}
    raw_id = entry.get("id") or entry.get("assumption_id") or fallback_id
    node["id"] = f"ecaa:blocker:{raw_id}"
    return node


def project_claim_verdicts(claim_doc):
    """Synthesize typed `ecaa:Claim` nodes from a `claim-verification.json`
    document so Invariants 1 (claim-completeness) and 5 (cross-graph) have
    focus nodes.

    The raw C document nests its verdicts under a `verdicts` array with no
    `@id`/`@type`, so projecting it whole yields zero triples (no shape binds).
    This helper emits, per verdict, a `{id, type:"Claim", status, supported_by}`
    JSON-LD node. Each `supported_by` reference is fragment-stripped so its
    object IRI equals the V `OutputFile` node IRI synthesized by
    `project_evidence_outputs` — that shared IRI is what lets Inv-3
    (evidence-coverage) and Inv-5 (cross-graph) bind. Returns a list of
    JSON-LD nodes (without `@context`; the caller stamps it).
    """
    verdicts = claim_doc.get("verdicts")
    if not isinstance(verdicts, list):
        return []
    nodes = []
    for idx, v in enumerate(verdicts):
        if not isinstance(v, dict):
            continue
        cid = v.get("claim_id") or f"claim_{idx:03d}"
        node = {
            "id": f"ecaa:claim:{cid}",
            "type": "Claim",
            "status": v.get("status", "pending"),
        }
        refs = v.get("supported_by")
        if isinstance(refs, list):
            stripped = [_strip_fragment(r) for r in refs if isinstance(r, str)]
            if stripped:
                node["supported_by"] = stripped
        nodes.append(node)
    return nodes


def project_evidence_outputs(proofs_rows):
    """Synthesize one typed `ecaa:OutputFile` node per distinct V output path
    (`proofs[].computed_from`/`produces`, fragment-stripped).

    These are the focus nodes for Invariant 3 (evidence-coverage): every
    `OutputFile` must be referenced by a Claim `supported_by` (or an
    `OutputUnused` Blocker carve-out) or it is a dangling, uncovered output.
    The node IRI is the bare output path so it coincides with the
    fragment-stripped `supported_by` IRI from `project_claim_verdicts`.

    A `workflow:<id>`-prefixed value is a STEP-lineage reference (a dependency
    edge endpoint, as `render_dependency_proofs_jsonl` emits), NOT a produced
    file — those are the execution-consistency domain (Invariant 6 sub-check),
    not evidence-coverage, so they are skipped here. Returns a list of JSON-LD
    nodes (without `@context`).
    """
    seen = set()
    nodes = []
    for row in proofs_rows:
        if not isinstance(row, dict):
            continue
        output = row.get("computed_from") or row.get("produces")
        if not isinstance(output, str):
            continue
        if output.startswith("workflow:"):
            continue
        output = _strip_fragment(output)
        if output in seen:
            continue
        seen.add(output)
        nodes.append({"id": output, "type": "OutputFile"})
    return nodes


def _bare_step(token):
    """Reduce an execution-step id to its bare token — mirrors the Rust
    `execution_consistency::bare` (`#step-de` ↔ `workflow:de` ↔ `de`)."""
    for prefix in ("#step-", "#step/", "workflow:"):
        if token.startswith(prefix):
            return token[len(prefix):]
    return token


def project_execution_steps(graph_nodes, proofs_rows):
    """Single-source E (F11): derive one `ecaa:WorkflowStep` node per distinct
    execution step and tag it with the materialization(s) it appears in.

    The authoritative source is the WRROC `@graph` HowToStep set; the E sidecar
    (`proofs.jsonl`) is the second materialization. Each step node carries
    `appears_in ecaa:graph` and/or `appears_in ecaa:evidence` so the
    `ExecutionConsistencyShape` (folded under Invariant 6) flags any step in one
    materialization but not the other. `ecaa:graph`/`ecaa:evidence` are
    projection-side sentinel individuals, not ECAA closed predicates. Returns a
    list of JSON-LD nodes (without `@context`).
    """
    def _is_howtostep(n):
        t = n.get("@type") if isinstance(n, dict) else None
        return t == "HowToStep" or (isinstance(t, list) and "HowToStep" in t)

    howtosteps = [n for n in graph_nodes if _is_howtostep(n)]
    # Execution-consistency only applies to packages whose WRROC @graph
    # actually materializes execution lineage (HowToSteps). A package with no
    # HowToStep set (e.g. a fixture isolating another invariant) has nothing to
    # reconcile, so emit no WorkflowStep focus nodes and let the
    # ExecutionConsistencyShape stay inert there.
    if not howtosteps:
        return []

    steps = {}

    def _mark(token, sentinel):
        bare = _bare_step(token)
        if not bare:
            return
        node = steps.setdefault(
            bare, {"id": f"ecaa:step:{bare}", "type": "WorkflowStep", "appears_in": []}
        )
        ref = {"@id": f"ecaa:{sentinel}"}
        if ref not in node["appears_in"]:
            node["appears_in"].append(ref)

    for n in howtosteps:
        sid = n.get("@id")
        if isinstance(sid, str):
            _mark(sid, "graph")

    for row in proofs_rows:
        if not isinstance(row, dict):
            continue
        # Both endpoints (`id` = producing step, `computed_from` = its source)
        # are E execution steps; a root step appears only as a `computed_from`.
        # Only `workflow:`-prefixed values are step refs — a `computed_from`
        # that is a file path (the evidence-coverage form) is an output, not a
        # step, and is excluded so it does not register as spurious drift.
        for key in ("id", "computed_from"):
            v = row.get(key)
            if isinstance(v, str) and v.startswith("workflow:"):
                _mark(v, "evidence")

    return list(steps.values())


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
    # V (proofs.jsonl) rows are retained so the evidence-coverage focus nodes
    # (`ecaa:OutputFile`, Invariant 3) can be synthesized once after the loop.
    proofs_rows = []

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
                if letter == "V":
                    # V rows are NOT projected raw: their `computed_from`
                    # step-lineage edge would RDFS-infer the step ref as an
                    # `ecaa:OutputFile` (range axiom) and their bare
                    # `type:WorkflowStep` would lack the `appears_in` sentinels
                    # the ExecutionConsistencyShape needs. The V sub-graph is
                    # instead owned by the dedicated synthesizers
                    # (`project_evidence_outputs`, `project_execution_steps`).
                    proofs_rows.append(entry)
                    continue
                if letter == "D":
                    entry = project_decision_record(entry, records_seen)
                elif letter == "Q":
                    entry = project_rerun_outcome_row(entry, records_seen)
                elif letter == "F":
                    entry = project_blocker_row(entry, records_seen)
                entry["@context"] = ctx["@context"]
                _to_rdf(projected, entry, context_label=rel)

    # V evidence-coverage focus nodes: one typed `ecaa:OutputFile` per distinct
    # output path (Invariant 3 / Invariant 5 binding). Synthesized from the
    # retained proofs rows; projection-layer only (off the BagIt path).
    for of_node in project_evidence_outputs(proofs_rows):
        node = dict(of_node)
        node["@context"] = ctx["@context"]
        _to_rdf(projected, node, context_label="runtime/proofs.jsonl#OutputFile")

    # C is a single document. Project typed `ecaa:Claim` nodes (status +
    # fragment-stripped `supported_by`) so Invariants 1 and 5 bind; projecting
    # the raw doc yields zero triples (the nested verdicts carry no @id/@type).
    c_path = pkg_dir / "runtime/claim-verification.json"
    if c_path.exists():
        c_doc = json.load(open(c_path))
        records_seen += 1
        for claim_node in project_claim_verdicts(c_doc):
            node = dict(claim_node)
            node["@context"] = ctx["@context"]
            _to_rdf(projected, node, context_label="runtime/claim-verification.json")

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

        # Single-source E (F11): derive `ecaa:WorkflowStep` focus nodes from the
        # authoritative WRROC @graph + the E sidecar so the
        # ExecutionConsistencyShape (Invariant 6 sub-check) can flag drift.
        graph_nodes = metadata.get("@graph", [])
        step_nodes = project_execution_steps(graph_nodes, proofs_rows)
        for step in step_nodes:
            node = dict(step)
            node["@context"] = ctx["@context"]
            _to_rdf(projected, node, context_label="ro-crate-metadata.json#WorkflowStep")
        if step_nodes and log is not None:
            log(f"  execution steps: {len(step_nodes)} WorkflowStep nodes (@graph + E)")
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
