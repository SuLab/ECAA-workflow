# ecaa-workflow-ecaa-types

Canonical Rust binding of the ECAA v0.1 typed object model.

This crate is the small, focused dependency a second Rust-language ECAA implementation can import to get the canonical types without pulling in the full `awa-workflow` compiler.

**Spec:** [`docs/ecaa-spec/v0.1.md`](../../docs/ecaa-spec/v0.1.md)
**Spec profile IRI:** `https://w3id.org/ecaa/v0.1`

## What's in this crate

Closed types and constants downstream consumers bind to:

- **A sub-graph (audit-proof):** `InvariantId`, `InvariantStatus`, `InvariantVerdict`, `AuditProofReport`.
- **Q sub-graph (re-execution):** `ReexecutionBucket` — closed 5-class enum for `RerunOutcome.class`.
- **F sub-graph (typed blockers):** `BlockerKind` (47 variants, `#[non_exhaustive]`) and its payload cascade — `ValidationFailureCause`, `LiteratureClaimFailureKind`, `ExcludedPath`, `SandboxRefusalRecord`, `StallSignalWire`, `StallAction`, `BlockerContext`, `BlockerEntry`.
- **Atom safety payloads:** `NetworkPolicy`, `SandboxRequirement` (re-exported from the `atom` module; these are payload types of `BlockerKind` variants).
- **Tool-error envelope:** `ToolErrorEnvelope` — the on-disk shape consumed by the remediation proposer.
- **Ablation contract:** `AblationFlag` (6 variants) + `all_flags()`. The runtime `is_active()` check is in `ecaa-workflow-core::ablation::AblationFlagExt` — kept there so this crate stays free of env-var coupling.
- **`consts::{NODE_TYPES, EDGE_PREDICATES, INVARIANT_IDS, SIDECAR_PATHS, REQUIRED_PROFILE_IRIS}`** — canonical const arrays used by spec-check tooling for cross-doc consistency.

## How consumers should import

- **First-party code** (the `awa-workflow` workspace itself): use the `scripps_workflow_core::blocker::*` re-exports so call sites stay stable across future internal moves.
- **Second implementations** (a different Rust ECAA producer): depend on `ecaa-workflow-ecaa-types` directly. The crate has no async runtime, no filesystem access, and no environment access, so it's safe to vendor into a minimal compiler.

A second Rust-language ECAA implementation imports this crate, uses serde to deserialize sidecar JSON into the moved types, applies its own invariant-checking logic, and emits its own `audit-proof-report.json` per the normative shape.

## Dependencies

`serde` (derive), `ts-rs`. No async runtime, no filesystem, no environment access. The crate is `#[deny(missing_docs)]`-able.

## Versioning

Tracks the ECAA spec version. v0.1.0 of this crate = ECAA spec v0.1.0.
