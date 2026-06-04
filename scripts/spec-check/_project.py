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
from urllib.parse import quote

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


def _curie_token(raw):
    """Percent-encode an arbitrary value for use in a synthesized CURIE local.

    Runtime IDs can contain characters that are legal in workflow strings but
    illegal in Turtle IRIs once expanded through the `ecaa:` prefix, such as
    `union(data:...)`. Escaping only the dynamic token keeps stable prefixes
    readable while preserving equality across referenced IDs.
    """
    return quote(str(raw), safe="")


# Post-Phase-1, the authoritative C-graph source is the host-signed verdict
# sink; the agent-writable stub is the fallback. Mirrors the Rust keystone
# loader (`audit_proof::loader`), which prefers the signed sink over the stub.
SIGNED_SINK_REL = "runtime/verification-reports/claim-verification.signed.json"
CLAIM_STUB_REL = "runtime/claim-verification.json"


def _union_claim_docs(docs):
    """Union per-task signed-sink rows into one C-graph document, mirroring the
    Rust loader's `union_signed_rows` (`audit_proof::loader`). Verdicts are
    concatenated (claim ids embed the task id, so they do not collide across
    tasks); coverage is unioned per-entity by BEST outcome
    (`addressed` > `unverifiable` > `absent`), so a later task addressing an
    entity resolves an earlier gap while a coverage-less row contributes
    nothing. The `coverage` key is omitted when no row carried one."""
    verdicts = []
    for d in docs:
        verdicts.extend(d.get("verdicts") or [])
    rank = {"addressed": 2, "unverifiable": 1}
    best = {}
    any_coverage = False
    for d in docs:
        cov = d.get("coverage")
        if not isinstance(cov, dict):
            continue
        per = cov.get("per_entity")
        if not isinstance(per, dict):
            continue
        any_coverage = True
        for entity, outcome in per.items():
            r = rank.get(outcome, 0)  # "absent"/unknown ⇒ worst, never erases a gap
            if r > best.get(entity, 0):
                best[entity] = r
    out = {"schema_version": "1", "source": "runtime-verifier", "verdicts": verdicts}
    if any_coverage:
        label = {2: "addressed", 1: "unverifiable"}
        per_entity = {e: label.get(r, "absent") for e, r in best.items()}
        out["coverage"] = {
            "required_total": len(best),
            "required_addressed": sum(1 for r in best.values() if r == 2),
            "required_unverifiable": sum(1 for r in best.values() if r == 1),
            "required_absent": sum(1 for r in best.values() if r == 0),
            "per_entity": per_entity,
        }
    return out


def load_claim_doc(pkg_dir):
    """Load the C-graph claim document, preferring the host-signed verdict
    sink over the agent-writable stub.

    The signed sink is APPEND-ONLY signed JSONL: one HMAC-signed row per task
    verification (`claim_sink::persist_signed_verdicts`). The projection reads
    the cleartext `verdicts`/`coverage` (the host already verified the
    signature — the projector does not re-verify). A single row is returned
    as-is (`_mac` left in place; no shape targets it); multiple rows are
    unioned exactly as the Rust loader does, so the SHACL projection sees the
    same cross-task C-graph the Rust invariants do — without this, a 2+-row
    sink threw `ValueError` here and silently fell back to the empty stub,
    diverging from the Rust union. Returns the parsed document (a dict) or
    `None` when neither file is present."""
    pkg_dir = Path(pkg_dir)
    signed = pkg_dir / SIGNED_SINK_REL
    if signed.exists():
        try:
            rows = [
                json.loads(ln)
                for ln in signed.read_text().splitlines()
                if ln.strip()
            ]
            if len(rows) == 1:
                return rows[0]
            if rows:
                return _union_claim_docs(rows)
        except (ValueError, OSError):
            pass
    stub = pkg_dir / CLAIM_STUB_REL
    if stub.exists():
        try:
            return json.load(open(stub))
        except (ValueError, OSError):
            pass
    return None


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
    node["id"] = f"ecaa:rerun:{_curie_token(raw_id)}"
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
    kind = entry.get("kind")
    if kind == "output_unused":
        node["kind"] = "OutputUnused"
        refs = entry.get("refs") or entry.get("detail")
        if isinstance(refs, str):
            node["refs"] = {"@id": _strip_fragment(refs)}
        raw_id = entry.get("id") or entry.get("assumption_id") or fallback_id
        node["id"] = f"ecaa:blocker:{_curie_token(raw_id)}"
        return node
    refs = entry.get("refs")
    if isinstance(refs, str):
        node["refs"] = {"@id": f"ecaa:rerun:{_curie_token(refs)}"}
    raw_id = entry.get("id") or entry.get("assumption_id") or fallback_id
    node["id"] = f"ecaa:blocker:{_curie_token(raw_id)}"
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
        cid_token = _curie_token(cid)
        node = {
            "id": f"ecaa:claim:{cid_token}",
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


# Nanopublication + schema.org bridge vocabulary. These are projection-side
# standards-bridge terms (F7-engineering) — NOT part of the ECAA closed term
# set, so they are bound in a local `@context` here, never in
# `ecaa-v0.1.jsonld`.
_NANOPUB_CTX = {
    "np": "http://www.nanopub.org/nschema#",
    "schema": "https://schema.org/",
    "prov": "http://www.w3.org/ns/prov#",
    "ecaa": "https://w3id.org/ecaa/ns/0.1#",
    "id": "@id",
    "type": "@type",
    "hasAssertion": {"@id": "np:hasAssertion", "@type": "@id"},
    "hasProvenance": {"@id": "np:hasProvenance", "@type": "@id"},
    "hasPublicationInfo": {"@id": "np:hasPublicationInfo", "@type": "@id"},
    "reviewRating": {"@id": "schema:reviewRating"},
    "citation": {"@id": "schema:citation", "@type": "@id"},
    "itemReviewed": {"@id": "schema:itemReviewed", "@type": "@id"},
    "wasGeneratedBy": {"@id": "prov:wasGeneratedBy", "@type": "@id"},
    "specVersion": {"@id": "ecaa:specVersion"},
}


def project_nanopub(claim_doc, ecaa_version="0.1"):
    """Wrap each C-graph verdict as a `schema:ClaimReview`/`schema:Claim`
    inside a nanopublication (F7-engineering) so the replaceable contract is
    expressible on a recognised standard.

    Emits, per verdict: (a) an ASSERTION graph of a `schema:ClaimReview` (with
    the verdict `status` as `schema:reviewRating` and each `supported_by` as a
    `schema:citation`) reviewing a `schema:Claim`; plus one nanopublication node
    linking (b) a PROVENANCE graph attributing the assertion to the ECAA
    verifier activity and (c) a PUBINFO graph stamping the ECAA spec version.
    Returns a list of JSON-LD docs (each with the local `np`/`schema` context),
    or an empty list when the C document carries no verdicts. `np`/`schema` are
    projection-side bridge terms, NOT ECAA closed predicates.
    """
    if not isinstance(claim_doc, dict):
        return []
    verdicts = claim_doc.get("verdicts")
    if not isinstance(verdicts, list):
        return []
    docs = []
    for idx, v in enumerate(verdicts):
        if not isinstance(v, dict):
            continue
        cid = v.get("claim_id") or f"claim_{idx:03d}"
        cid_token = _curie_token(cid)
        status = v.get("status", "pending")
        refs = v.get("supported_by")
        citations = (
            [_strip_fragment(r) for r in refs if isinstance(r, str)]
            if isinstance(refs, list)
            else []
        )
        np_id = f"ecaa:nanopub:{cid_token}"
        review_id = f"ecaa:claimreview:{cid_token}"
        claim_id = f"ecaa:schemaclaim:{cid_token}"
        review = {
            "id": review_id,
            "type": "schema:ClaimReview",
            "reviewRating": status,
            "itemReviewed": claim_id,
        }
        if citations:
            review["citation"] = citations
        # Nanopublication head + the three named graphs.
        docs.append(
            {
                "@context": _NANOPUB_CTX,
                "id": np_id,
                "type": "np:Nanopublication",
                "hasAssertion": f"{np_id}:assertion",
                "hasProvenance": f"{np_id}:provenance",
                "hasPublicationInfo": f"{np_id}:pubinfo",
            }
        )
        docs.append({"@context": _NANOPUB_CTX, **review})
        docs.append(
            {
                "@context": _NANOPUB_CTX,
                "id": claim_id,
                "type": "schema:Claim",
            }
        )
        # Provenance: the assertion was generated by the ECAA verifier activity.
        docs.append(
            {
                "@context": _NANOPUB_CTX,
                "id": f"{np_id}:assertion",
                "wasGeneratedBy": "ecaa:claim-verifier",
            }
        )
        # Pubinfo: stamp the ECAA spec version.
        docs.append(
            {
                "@context": _NANOPUB_CTX,
                "id": f"{np_id}:pubinfo",
                "specVersion": ecaa_version,
            }
        )
    return docs


def project_evidence_outputs(proofs_rows, graph_nodes=None, pkg_dir=None):
    """Synthesize one typed `ecaa:OutputFile` node per distinct V output path.

    These are the focus nodes for Invariant 3 (evidence-coverage): every
    `OutputFile` must be referenced by a Claim `supported_by` (or an
    `OutputUnused` Blocker carve-out) or it is a dangling, uncovered output.
    The node IRI is the bare output path so it coincides with the
    fragment-stripped `supported_by` IRI from `project_claim_verdicts`.

    Source-of-truth parity with Rust matters here: produced analytical outputs
    are existing RO-Crate output entities under `runtime/outputs/**` plus any
    real-path `proofs[].computed_from`/`produces` row. A pre-execution package
    can list planned output entities before their files exist; those are not
    evidence-coverage focus nodes yet because there is no produced file to
    cover. A `workflow:<id>`-prefixed proof value is a STEP-lineage reference,
    not a produced file, and is skipped.

    Returns a list of JSON-LD nodes (without `@context`).
    """
    seen = set()
    nodes = []
    pkg_path = Path(pkg_dir) if pkg_dir is not None else None

    def _types(node):
        raw = node.get("@type") if isinstance(node, dict) else None
        if isinstance(raw, str):
            return {raw}
        if isinstance(raw, list):
            return {x for x in raw if isinstance(x, str)}
        return set()

    def _add(output):
        if not isinstance(output, str):
            return
        if output.startswith("workflow:"):
            return
        output = _strip_fragment(output)
        if not output or output in seen:
            return
        seen.add(output)
        nodes.append({"id": output, "type": "OutputFile"})

    def _exists_in_package(output):
        if pkg_path is None:
            return True
        if not isinstance(output, str):
            return False
        if output.startswith("workflow:"):
            return False
        output = _strip_fragment(output)
        if not output:
            return False
        # RO-Crate ids can be relative paths. Keep absolute/IRI-like ids
        # eligible for tests and future external outputs, but only treat local
        # package paths as produced evidence when the file is present.
        if "://" in output or output.startswith("ecaa:"):
            return True
        rel = output[2:] if output.startswith("./") else output
        if rel.startswith("/"):
            return Path(rel).exists()
        return (pkg_path / rel).exists()

    for entity in graph_nodes or []:
        if not isinstance(entity, dict):
            continue
        output = entity.get("@id")
        if not isinstance(output, str):
            continue
        ty = _types(entity)
        is_image = "ImageObject" in ty or "schema:Image" in ty
        is_dataset_or_file = bool(ty & {"Dataset", "File", "dcat:Dataset"})
        if is_image or (output.startswith("runtime/outputs/") and is_dataset_or_file):
            if _exists_in_package(output):
                _add(output)

    for row in proofs_rows:
        if not isinstance(row, dict):
            continue
        _add(row.get("computed_from") or row.get("produces"))
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
        # The on-disk proofs.jsonl carries TWO row schemas; accept both:
        #
        # 1. Bare EdgeContract (the production form — conversation's
        #    `build_proofs_jsonl` and `emit_proofs_jsonl` serialize an
        #    EdgeContract directly): `{from_node, from_port, to_node, to_port,
        #    proof}`. BOTH endpoints (from_node, to_node) are execution steps;
        #    their bare node ids (`qc`, `de`) reduce to the same key as the
        #    @graph `#step-<id>` HowToSteps via `_bare_step`, so E is non-empty
        #    on real packages.
        for key in ("from_node", "to_node"):
            v = row.get(key)
            if isinstance(v, str) and v:
                _mark(v, "evidence")
        # 2. workflow:-enveloped (core's `render_dependency_proofs_jsonl`):
        #    `id` = producing step, `computed_from` = its source; a root step
        #    appears only as a `computed_from`. Only `workflow:`-prefixed
        #    values are step refs — a `computed_from` that is a file path (the
        #    evidence-coverage form) is an output, not a step, and is excluded
        #    so it does not register as spurious drift.
        for key in ("id", "computed_from"):
            v = row.get(key)
            if isinstance(v, str) and v.startswith("workflow:"):
                _mark(v, "evidence")

    return list(steps.values())


def project_qualified_derivation(metadata):
    """Upgrade an amendment/branch lineage edge to a reified
    `prov:qualifiedDerivation` (F7-engineering).

    The reference emitter records lineage as a plain `prov:wasDerivedFrom` on
    the root Dataset (`./`). This helper ADDITIONALLY synthesizes a reified
    `prov:Derivation` node (`prov:entity` = the parent) and links the package
    node to it via `prov:qualifiedDerivation`, so a second-impl can inspect the
    derivation's structure. The plain `prov:wasDerivedFrom` is retained
    (additive — RO-Crate-1.1 readers keep working). `prov:qualifiedDerivation`
    / `prov:Derivation` are RDF projection terms, NOT ECAA closed predicates,
    so they are bound in a local `@context` here (not in the canonical
    `ecaa-v0.1.jsonld`). Returns a list of JSON-LD docs (each with its own
    `@context`), or an empty list when no lineage edge is present.
    """
    graph = metadata.get("@graph", []) if isinstance(metadata, dict) else []
    root = next(
        (n for n in graph if isinstance(n, dict) and n.get("@id") == "./"),
        None,
    )
    if not root:
        return None
    parent = root.get("prov:wasDerivedFrom")
    if isinstance(parent, dict):
        parent_id = parent.get("@id")
    elif isinstance(parent, str):
        parent_id = parent
    else:
        return None
    if not parent_id:
        return None
    prov_ctx = {
        "prov": "http://www.w3.org/ns/prov#",
        "id": "@id",
        "type": "@type",
        "qualifiedDerivation": {"@id": "prov:qualifiedDerivation", "@type": "@id"},
        "entity": {"@id": "prov:entity", "@type": "@id"},
    }
    derivation_id = f"prov:derivation:{parent_id}"
    return [
        {
            "@context": prov_ctx,
            "id": "ecaa:package",
            "qualifiedDerivation": derivation_id,
        },
        {
            "@context": prov_ctx,
            "id": derivation_id,
            "type": "prov:Derivation",
            "entity": parent_id,
        },
    ]


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

    metadata_path = pkg_dir / "ro-crate-metadata.json"
    metadata = None
    graph_nodes = []
    if metadata_path.exists():
        metadata = json.load(open(metadata_path))
        graph_nodes = metadata.get("@graph", [])

    # V evidence-coverage focus nodes: one typed `ecaa:OutputFile` per distinct
    # output path (Invariant 3 / Invariant 5 binding). Synthesized from the
    # retained proofs rows and the RO-Crate output entities; projection-layer
    # only (off the BagIt path).
    for of_node in project_evidence_outputs(proofs_rows, graph_nodes, pkg_dir):
        node = dict(of_node)
        node["@context"] = ctx["@context"]
        _to_rdf(projected, node, context_label="V#OutputFile")

    # C is a single document, sourced from the host-signed verdict sink when
    # present (Phase-1 keystone) and the agent-writable stub otherwise. Project
    # typed `ecaa:Claim` nodes (status + fragment-stripped `supported_by`) so
    # Invariants 1 and 5 bind; projecting the raw doc yields zero triples (the
    # nested verdicts carry no @id/@type). Additionally wrap each verdict as a
    # `schema:ClaimReview`/`schema:Claim` inside a nanopublication (F7).
    c_doc = load_claim_doc(pkg_dir)
    if c_doc is not None:
        records_seen += 1
        for claim_node in project_claim_verdicts(c_doc):
            node = dict(claim_node)
            node["@context"] = ctx["@context"]
            _to_rdf(projected, node, context_label="runtime/claim-verification")
        np_docs = project_nanopub(c_doc, c_doc.get("ecaa_version", "0.1"))
        for np_doc in np_docs:
            _to_rdf(projected, np_doc, context_label="runtime/claim-verification#nanopub")
        if np_docs and log is not None:
            log(f"  nanopublication: {len(np_docs)} schema.org/nanopub nodes")

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
    if metadata is not None:
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

        # Amendment/branch lineage: upgrade the plain `prov:wasDerivedFrom` edge
        # to a reified `prov:qualifiedDerivation` (additive; F7-engineering).
        derivation_docs = project_qualified_derivation(metadata)
        if derivation_docs:
            for doc in derivation_docs:
                _to_rdf(projected, doc, context_label="ro-crate-metadata.json#qualifiedDerivation")
            if log is not None:
                log("  qualified derivation: prov:Derivation + prov:qualifiedDerivation")
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
