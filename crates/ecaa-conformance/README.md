<!-- crates/ecaa-conformance/README.md -->
# ecaa-conformance — ECAA v0.1 conformance suite

This crate is the **machine-checkable conformance contract** referenced
in PAR-26-040 §C.0 deliverable D9. Any second implementation of the
ECAA primitive can claim ECAA v0.1 conformance by passing all tests
in `tests/`:

| Test | Checks |
|---|---|
| `wrroc_v05_conformance` | WRROC v0.5 Tier-3 round-trip over the 23-fixture corpus under `testdata/wrroc-fixtures/`; the G1 acceptance gate requires ≥17/23 to validate |
| `audit_proof_invariants` | All 6 audit-proof invariants over the 2-fixture audit-proof corpus under `crates/core/tests/fixtures/audit-proof/` (no fixture may Fail any invariant) |
| `ablation_contract` | Each of the 6 `ECAA_ABLATE_*` flags suppresses exactly one subgraph (composes its own in-test DAG; no fixture corpus) |

The external-validator gates (`shacl_non_vacuous`, `conformance_external_validators`,
`wrroc_runcrate`) are capability-probed: they run for real when the Python
validator toolchain is present and print a LOUD skip notice otherwise. Install it
with `pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate`
(WRROC pins also in `requirements-validator.txt`).

Run: `cargo test -p ecaa-workflow-conformance`.
