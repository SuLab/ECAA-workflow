#!/usr/bin/env python3
"""Executed gate for the enum-membership SHACL (ecaa-skos-membership.shacl.ttl).

This is the gate that makes the F10 enum-membership SKOS SHACL *executed*, not
merely *published*. The membership shapes
(`ecaa:BlockerKindMembershipShape` / `ecaa:RerunOutcomeClassMembershipShape`)
are deliberately NOT loaded into the live `project_package.py` validate() call —
that gate's conformance ABox carries `Blocker.kind` as the Invariant-4 carve-out
CamelCase string (`UnprovableEdge`/`PolicyException`) and `RerunOutcome.class`
as an IRI individual (`ecaa:failed`), neither of which is the snake_case
`skos:notation` wire token the membership SPARQL matches on, so loading them
there would mis-fire. Instead, THIS gate exercises the published membership
shapes over a snake_case ABox projected through the canonical `ecaa-v0.2.jsonld`
context, exactly as a real wire-form package serializes `kind`/`class`.

It runs real pyshacl over real SPARQL:

  * a REGISTERED token (e.g. `agent_error`, `byte_identical`) → the package
    CONFORMS, and the membership shape bound a focus node (non-vacuous);
  * an UNREGISTERED token (e.g. `agnt_error`, `totally_made_up`) → the
    membership shape FIRES (non-conformance), naming the correct source shape.

Run standalone (prints machine-readable verdict lines + exits 0/1):

    python3 scripts/spec-check/test_skos_membership.py

Or under pytest (the `test_*` functions below):

    python3 -m pytest scripts/spec-check/test_skos_membership.py -q

It is ALSO wired into `make conformance` via the Rust gate
`crates/ecaa-conformance/tests/conformance/skos_membership_shacl.rs`, which
shells out here and parses the `SKOS-MEMBERSHIP:` verdict lines.

Dependencies (pip install --user --break-system-packages):
    pyld rdflib pyshacl
"""
import json
import sys
from pathlib import Path

_SPEC_DIR = Path(__file__).parent.parent.parent / "docs" / "ecaa-spec"
_REG_DIR = _SPEC_DIR / "registration"

# SHACL source-shape local names the membership file declares; the assertions
# below pin that the firing/non-firing shape is exactly one of these (so the
# gate can never pass because some UNRELATED shape happened to flip).
_BLOCKER_SHAPE = "BlockerKindMembershipShape"
_RERUN_SHAPE = "RerunOutcomeClassMembershipShape"

# One registered + one deliberately-unregistered token per scheme. The
# registered tokens are real `skos:notation` entries in ecaa-skos-schemes.ttl;
# the unregistered tokens are plausible typos / fabrications that MUST NOT
# resolve to any concept in their scheme.
_REGISTERED_BLOCKER_KIND = "agent_error"
_UNREGISTERED_BLOCKER_KIND = "agnt_error"
_REGISTERED_RERUN_CLASS = "byte_identical"
_UNREGISTERED_RERUN_CLASS = "totally_made_up"


def _require_deps():
    """Import the validator toolchain; print an install hint + exit 2 if absent.

    Mirrors `project_package.py`'s contract: a missing-dep run is a LOUD,
    non-conformant exit (2), never a silent pass.
    """
    try:
        from pyld import jsonld  # noqa: F401
        from rdflib import Graph  # noqa: F401
        import pyshacl  # noqa: F401
    except ImportError as e:
        print(
            f"ERROR: missing dependency ({e}). "
            "pip install --user --break-system-packages pyld rdflib pyshacl",
            file=sys.stderr,
        )
        sys.exit(2)


def _canonical_context():
    """Return the canonical `ecaa-v0.2.jsonld` @context block (the same context
    `project_package.py` projects every sidecar through)."""
    doc = json.load(open(_SPEC_DIR / "ecaa-v0.2.jsonld"))
    return doc["@context"] if "@context" in doc else doc


def _project_abox(nodes):
    """Expand snake_case JSON-LD `nodes` through the canonical context into an
    rdflib Graph — the real wire-form projection path.

    Each node is a dict using the canonical short terms (`id`, `type`, `kind`,
    `class`). `kind`/`class` map to `ecaa:kind`/`ecaa:class` as plain literals,
    so a snake_case token projects to exactly the literal the membership SPARQL
    joins against `skos:notation`.
    """
    from pyld import jsonld
    from rdflib import Graph

    ctx = _canonical_context()
    g = Graph()
    for node in nodes:
        doc = {"@context": ctx, **node}
        nq = jsonld.to_rdf(doc, {"format": "application/n-quads"})
        g.parse(data=nq, format="nquads")
    return g


def _membership_shapes():
    from rdflib import Graph

    g = Graph()
    g.parse(_REG_DIR / "ecaa-skos-membership.shacl.ttl", format="turtle")
    return g


def _schemes_graph():
    from rdflib import Graph

    g = Graph()
    g.parse(_REG_DIR / "ecaa-skos-schemes.ttl", format="turtle")
    return g


def _local_name(iri):
    return str(iri).split("#")[-1].split("/")[-1]


def _validate(abox_nodes):
    """Run the published membership SHACL over a projected snake_case ABox
    (merged with the published schemes graph, since the membership join must
    resolve in the data graph). Returns (conforms, firing_shape_local_names).
    """
    import pyshacl
    from rdflib import RDF, URIRef

    data = _project_abox(abox_nodes)
    # The skos:inScheme/skos:notation concepts must sit alongside the ABox: the
    # membership SPARQL `FILTER NOT EXISTS { ?c skos:inScheme … ; skos:notation
    # ?tok }` is evaluated over the DATA graph, so the scheme triples are merged
    # into the data graph (not handed in as the ont graph).
    data += _schemes_graph()
    shapes = _membership_shapes()
    conforms, report_graph, _text = pyshacl.validate(
        data_graph=data,
        shacl_graph=shapes,
        inference="none",
        debug=False,
    )

    def _sh(local):
        return URIRef("http://www.w3.org/ns/shacl#" + local)

    firing = set()
    for result in report_graph.subjects(RDF.type, _sh("ValidationResult")):
        for src in report_graph.objects(result, _sh("sourceShape")):
            firing.add(_local_name(src))
    return conforms, firing


# Each case: (label, abox-nodes, expect_conforms, expect_shape_or_None).
_REGISTERED_CASE = (
    "registered",
    [
        {"id": "ecaa:blocker:reg", "type": "Blocker", "kind": _REGISTERED_BLOCKER_KIND},
        {"id": "ecaa:rerun:reg", "type": "RerunOutcome", "class": _REGISTERED_RERUN_CLASS},
    ],
    True,
    None,
)
_UNREGISTERED_BLOCKER_CASE = (
    "unregistered-blocker",
    [{"id": "ecaa:blocker:bad", "type": "Blocker", "kind": _UNREGISTERED_BLOCKER_KIND}],
    False,
    _BLOCKER_SHAPE,
)
_UNREGISTERED_RERUN_CASE = (
    "unregistered-rerun",
    [{"id": "ecaa:rerun:bad", "type": "RerunOutcome", "class": _UNREGISTERED_RERUN_CLASS}],
    False,
    _RERUN_SHAPE,
)


def _check_registered_conforms_non_vacuously():
    """A registered token conforms AND the membership shape actually bound a
    focus node (non-vacuity guard — a published-but-never-targeted shape would
    pass trivially)."""
    conforms, firing = _validate(_REGISTERED_CASE[1])
    assert conforms, (
        f"registered tokens (kind={_REGISTERED_BLOCKER_KIND}, "
        f"class={_REGISTERED_RERUN_CLASS}) MUST conform; firing shapes={firing}"
    )
    assert not firing, f"registered ABox must fire NO membership shape; got {firing}"
    # Non-vacuity: prove the membership shapes have focus nodes by re-running
    # with the SAME node ids but UNREGISTERED tokens and confirming the shapes
    # DO fire — i.e. the targetClass actually selects these nodes.
    bad_conforms, bad_firing = _validate(
        [
            {"id": "ecaa:blocker:probe", "type": "Blocker", "kind": _UNREGISTERED_BLOCKER_KIND},
            {"id": "ecaa:rerun:probe", "type": "RerunOutcome", "class": _UNREGISTERED_RERUN_CLASS},
        ]
    )
    assert not bad_conforms, "non-vacuity probe: unregistered tokens must NOT conform"
    assert bad_firing == {_BLOCKER_SHAPE, _RERUN_SHAPE}, (
        "non-vacuity probe: both membership shapes must select their focus nodes; "
        f"fired={bad_firing}"
    )


def _check_unregistered_blocker_fires():
    conforms, firing = _validate(_UNREGISTERED_BLOCKER_CASE[1])
    assert not conforms, (
        f"unregistered Blocker.kind={_UNREGISTERED_BLOCKER_KIND} MUST NOT conform"
    )
    assert _BLOCKER_SHAPE in firing, (
        f"{_BLOCKER_SHAPE} must fire on unregistered kind; firing={firing}"
    )


def _check_unregistered_rerun_fires():
    conforms, firing = _validate(_UNREGISTERED_RERUN_CASE[1])
    assert not conforms, (
        f"unregistered RerunOutcome.class={_UNREGISTERED_RERUN_CLASS} MUST NOT conform"
    )
    assert _RERUN_SHAPE in firing, (
        f"{_RERUN_SHAPE} must fire on unregistered class; firing={firing}"
    )


# ---- pytest entry points (auto-import the deps; skip-loud if absent) --------


def _pytest_deps_ok():
    try:
        from pyld import jsonld  # noqa: F401
        from rdflib import Graph  # noqa: F401
        import pyshacl  # noqa: F401

        return True
    except ImportError:
        return False


def test_registered_conforms_non_vacuously():
    if not _pytest_deps_ok():
        import pytest

        pytest.skip("pyld/rdflib/pyshacl not importable — SKOS membership gate not run")
    _check_registered_conforms_non_vacuously()


def test_unregistered_blocker_kind_fires():
    if not _pytest_deps_ok():
        import pytest

        pytest.skip("pyld/rdflib/pyshacl not importable — SKOS membership gate not run")
    _check_unregistered_blocker_fires()


def test_unregistered_rerun_class_fires():
    if not _pytest_deps_ok():
        import pytest

        pytest.skip("pyld/rdflib/pyshacl not importable — SKOS membership gate not run")
    _check_unregistered_rerun_fires()


def main():
    """Standalone runner: print `SKOS-MEMBERSHIP:` verdict lines + exit 0/1.

    The Rust conformance gate parses these lines, so they are the stable
    machine-readable contract:
        SKOS-MEMBERSHIP: registered=PASS
        SKOS-MEMBERSHIP: unregistered-blocker=FIRES
        SKOS-MEMBERSHIP: unregistered-rerun=FIRES
        SKOS-MEMBERSHIP: gate=PASS|FAIL
    """
    _require_deps()
    failures = []

    # Registered → conforms, non-vacuously.
    try:
        _check_registered_conforms_non_vacuously()
        print("SKOS-MEMBERSHIP: registered=PASS")
    except AssertionError as e:
        print("SKOS-MEMBERSHIP: registered=FAIL")
        failures.append(f"registered: {e}")

    # Unregistered Blocker.kind → membership shape fires.
    try:
        _check_unregistered_blocker_fires()
        print("SKOS-MEMBERSHIP: unregistered-blocker=FIRES")
    except AssertionError as e:
        print("SKOS-MEMBERSHIP: unregistered-blocker=PASS")  # wrongly conformed
        failures.append(f"unregistered-blocker: {e}")

    # Unregistered RerunOutcome.class → membership shape fires.
    try:
        _check_unregistered_rerun_fires()
        print("SKOS-MEMBERSHIP: unregistered-rerun=FIRES")
    except AssertionError as e:
        print("SKOS-MEMBERSHIP: unregistered-rerun=PASS")  # wrongly conformed
        failures.append(f"unregistered-rerun: {e}")

    if failures:
        print("SKOS-MEMBERSHIP: gate=FAIL")
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    print("SKOS-MEMBERSHIP: gate=PASS")
    return 0


if __name__ == "__main__":
    sys.exit(main())
