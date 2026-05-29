<!-- docs/ecaa-spec/README.md -->
# ECAA — Evidence-Carrying Analysis Artifact Specification

This directory holds the v0.1 normative specification for the Evidence-Carrying Analysis Artifact (ECAA). An ECAA package is a typed object `P = (I, D, E, V, C, Q, F, A)` over a WRROC v0.5 Tier-3 substrate, binding eight sub-graphs under four validity-preserving operations and six machine-checkable audit-proof invariants.

**Profile IRI:** `https://w3id.org/ecaa/v0.1`
**Media type:** `application/vnd.ecaa+zip`
**Spec date:** 2026-05-18

## File index

| File | Role | Normative? |
|---|---|---|
| [`v0.1.md`](v0.1.md) | Full normative spec — start here | Yes |
| [`invariants.md`](invariants.md) | Predicate reference for the 6 audit-proof invariants | Yes |
| [`operations.md`](operations.md) | Operation contracts for compose/amend/re-execute/ablate | Yes |
| [`ecaa-v0.1.ttl`](ecaa-v0.1.ttl) | OWL ontology profile (full normative) | Yes |
| [`ecaa-v0.1.shacl.ttl`](ecaa-v0.1.shacl.ttl) | SHACL shapes for invariants 2, 4, 6 | Yes |
| [`ecaa-v0.1.jsonld`](ecaa-v0.1.jsonld) | JSON-LD context mapping JSON sidecars to RDF | Yes |
| [`subgraph-schemas/`](subgraph-schemas/) | 8 JSON Schema (draft-07) sidecar validators | Yes |
| [`registration/`](registration/) | w3id PR draft + IANA media-type registration template | Informative |

## Verification

The helpers under [`scripts/spec-check/`](../../scripts/spec-check/) verify the spec: `validate_schemas.sh` checks JSON-Schema syntax, `owl_consistency.py` checks OWL-DL satisfiability, and `project_package.py` projects a reference package's RDF for SHACL conformance. End-to-end conformance is exercised by the `crates/ecaa-conformance/` test suite.

## Reference implementation

`ECAA-workflow` (this repository's Rust workspace) is the first open-source reference implementation of ECAA. The conformance suite (`crates/ecaa-conformance/`) is implementation-independent — any second-implementation candidate passing the 5-bar conformance check in `v0.1.md` §8 may claim ECAA-v0.1 compliance regardless of source.

The Rust binding of the typed object model lives in [`crates/ecaa-types/`](../../crates/ecaa-types/). Second-implementation candidates in Rust MAY depend on this small focused crate; candidates in other languages implement the types from the spec directly.

## Versioning

ECAA follows Semantic Versioning. This document is v0.1.0. Change-classification rules and the version-negotiation protocol are in `v0.1.md` §9.

## Status

Draft. PAR-26-040 Aim 1 Track B deliverable D7. Reviewer comments welcomed via the project repository.
