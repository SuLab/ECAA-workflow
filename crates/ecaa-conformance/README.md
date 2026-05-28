<!-- crates/ecaa-conformance/README.md -->
# ecaa-conformance — ECAA v0.1 conformance suite

This crate is the **machine-checkable conformance contract** referenced
in PAR-26-040 §C.0 deliverable D9. Any second implementation of the
ECAA primitive can claim ECAA v0.1 conformance by passing all tests
in `tests/`:

| Test | Checks |
|---|---|
| `wrroc_v05_conformance` | WRROC v0.5 Tier-3 round-trip (13-fixture corpus) |
| `audit_proof_invariants` | All 6 audit-proof invariants on the corpus |
| `ablation_contract` | Each of the 6 `ECAA_ABLATE_*` flags suppresses exactly one subgraph |

Run: `cargo test -p ecaa-workflow-conformance`.
