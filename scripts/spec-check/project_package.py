#!/usr/bin/env python3
"""End-to-end ECAA v0.2 RDF projection + SHACL validation.

Takes a package directory, projects all 8 sub-graph sidecars (the 6 JSONL
streams plus the C and A single-document JSON files) plus a synthesized
`ecaa:Package` node through ecaa-v0.2.jsonld into RDF (via the shared
`_project.project` helper), serializes that ABox to `<pkg_dir>/package.ttl`,
then validates against ecaa-v0.2.shacl.ttl (SHACL) with the ecaa-v0.2.ttl
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

# Content-addressed cache (opt-in via ECAA_SPEC_CACHE_DIR): the expensive
# pyshacl.validate over (ABox + ontology + shapes) is a deterministic function
# of those inputs, so an identical re-validation replays the cached verdict and
# skips both the ~0.8s pyshacl run and the ~40ms Turtle parses. Disabled ⇒
# every call below is a no-op and the output/timing are unchanged.
from _cache import enabled as _cache_enabled, validation_key, lookup, store  # noqa: E402

_cache_on = _cache_enabled()
_source_files = [
    spec_dir / "ecaa-v0.2.shacl.ttl",
    spec_dir / "ecaa-v0.2.ttl",
    spec_dir / "registration" / "ecaa-skos-schemes.ttl",
    spec_dir / "registration" / "ecaa-profiles.ttl",
]
_cache_key = validation_key("shacl", projected, _source_files) if _cache_on else None
_cached = lookup(_cache_key)
if _cached is not None:
    sys.stdout.write(_cached["output"])
    print("shacl_projection: cache HIT (skipped pyshacl)", file=sys.stderr)
    sys.exit(_cached["exit_code"])

# Load ontology (inference graph).
onto = Graph()
onto.parse(spec_dir / "ecaa-v0.2.ttl", format="turtle")

# Load SHACL shapes.
shapes = Graph()
shapes.parse(spec_dir / "ecaa-v0.2.shacl.ttl", format="turtle")

# SKOS concept schemes + profile IRIs — additive, optional. The schemes are
# merged into the DATA graph (not the ont graph): pyshacl evaluates sh:sparql
# constraints against the data graph (plus inferences), NOT the ont graph, so a
# skos:inScheme/skos:notation membership join only resolves when the concept
# notations sit alongside the package ABox. Loaded after write_package_ttl so
# package.ttl stays the clean §8.5 ABox. ecaa-profiles.ttl (governance profile
# IRIs) has no membership-join role and goes into the ont graph.
#
# Enum-membership SHACL (ecaa-skos-membership.shacl.ttl) is NOT loaded into the
# live shapes graph here: the projected conformance ABox represents Q
# RerunOutcome.class as an IRI individual (ecaa:failed) and F Blocker.kind as
# the spec-canonical carve-out string the Invariant-4 SHACL lists
# (UnprovableEdge / PolicyException), neither of which is the snake_case
# skos:notation wire token the membership shapes match on. Loading them into
# this live gate would mis-fire on the Phase-3 Invariant-4 fixtures.
#
# The membership shapes ARE executed — by a DEDICATED gate, not by this call
# and not by the Rust enum↔scheme lint. `scripts/spec-check/test_skos_membership.py`
# (run standalone, under pytest, and inside `make conformance` via the Rust gate
# `crates/ecaa-conformance/tests/conformance/skos_membership_shacl.rs`) projects a
# snake_case ABox through THIS canonical context and runs real pyshacl/SPARQL
# over the published membership shapes + published SKOS schemes: a registered
# token (agent_error / byte_identical) conforms; an unregistered token
# (agnt_error / totally_made_up) fires the membership shape. Separately, the
# unconditional Rust lint
# (crates/ecaa-conformance/tests/conformance/skos_scheme_agreement.rs) checks
# Rust-enum ⇄ SKOS-scheme set agreement by string-parse only — it runs NO
# pyshacl/SPARQL. The two are complementary: the lint pins the vocabulary size
# and membership of the closed enums against the Rust source; the dedicated gate
# proves the published SPARQL actually rejects out-of-vocabulary wire tokens.
reg = spec_dir / "registration"
for fname in ("ecaa-skos-schemes.ttl", "ecaa-profiles.ttl"):
    f = reg / fname
    if f.exists():
        onto.parse(f, format="turtle")

# Run SHACL. `report_graph` is the RDF validation report; we bucket
# violations per shape so the Rust↔pyshacl agreement gate can compare per
# invariant (not just the global verdict).
conforms, report_graph, report = pyshacl.validate(
    data_graph=projected,
    shacl_graph=shapes,
    ont_graph=onto,
    inference="rdfs",
    debug=False,
)

# Per-shape machine-readable verdicts: one `SHACL-INVARIANT: <Shape>=PASS|FAIL`
# line per NodeShape the SHACL file declares. A shape is FAIL iff the report
# carries a ValidationResult whose sh:sourceShape is that shape; every other
# declared shape is PASS (it had focus nodes or none, but no violation). The
# global `SHACL conformance:` line below is retained for the existing
# `shacl_non_vacuous.rs` / per-invariant gates that parse it.
from rdflib import RDF, URIRef  # noqa: E402


def _sh(local):
    return URIRef("http://www.w3.org/ns/shacl#" + local)


def _local_name(iri):
    return str(iri).split("#")[-1].split("/")[-1]


declared_shapes = {
    _local_name(s) for s in shapes.subjects(RDF.type, _sh("NodeShape"))
}
violated_shapes = set()
for result in report_graph.subjects(RDF.type, _sh("ValidationResult")):
    for src in report_graph.objects(result, _sh("sourceShape")):
        violated_shapes.add(_local_name(src))
out_lines = []
for shape in sorted(declared_shapes):
    verdict = "FAIL" if shape in violated_shapes else "PASS"
    out_lines.append(f"SHACL-INVARIANT: {shape}={verdict}")

# Two conformsTo tiers (registration/ecaa-profiles.ttl), graded by per-result
# sh:resultSeverity — purely additive lines that do NOT affect the global
# `SHACL conformance:` verdict or the exit code below (so the existing
# `*_shacl.rs` gates that parse that line are untouched). pyshacl flips
# `conforms` on ANY result regardless of severity, so the binary gate stays
# strict; these tier lines let an adopter read the F10 graded floor:
#   substrate-hygiene = PASS iff NO sh:Violation-severity result (warnings OK)
#   apparatus         = PASS iff NO result of any severity
# A shape's tier is its declared sh:severity (Violation→hygiene, Warning→
# apparatus). An unsevered shape defaults to sh:Violation per SHACL.
_SEV = _sh("resultSeverity")
violation_severity = _sh("Violation")
has_violation = False
has_any_result = False
for result in report_graph.subjects(RDF.type, _sh("ValidationResult")):
    has_any_result = True
    sevs = list(report_graph.objects(result, _SEV))
    # No explicit severity on a result ⇒ SHACL default sh:Violation.
    if (not sevs) or (violation_severity in sevs):
        has_violation = True
out_lines.append(f"SHACL-TIER: substrate-hygiene={'FAIL' if has_violation else 'PASS'}")
out_lines.append(f"SHACL-TIER: apparatus={'FAIL' if has_any_result else 'PASS'}")
out_lines.append(f"SHACL conformance: {'PASS' if conforms else 'FAIL'}")

# Emit the verdict block in one write so the cached replay is byte-identical to
# a live run, then persist it (no-op when caching is disabled).
output = "\n".join(out_lines) + "\n"
exit_code = 0
if not conforms:
    output += f"{report}\n"
    exit_code = 1
sys.stdout.write(output)
store(_cache_key, exit_code, output)
sys.exit(exit_code)
