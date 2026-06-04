# Methods

This repository implements a deterministic compiler and execution workflow for Evidence-Carrying Analysis Artifacts (ECAA). The current emitted package contract is **ECAA v0.2** with profile IRI `https://w3id.org/ecaa/v0.2`. The normative spec lives in [`docs/ecaa-spec/v0.2.md`](docs/ecaa-spec/v0.2.md); the machine-readable assets are [`docs/ecaa-spec/ecaa-v0.2.ttl`](docs/ecaa-spec/ecaa-v0.2.ttl), [`docs/ecaa-spec/ecaa-v0.2.shacl.ttl`](docs/ecaa-spec/ecaa-v0.2.shacl.ttl), and [`docs/ecaa-spec/ecaa-v0.2.jsonld`](docs/ecaa-spec/ecaa-v0.2.jsonld).

## Workflow Method

1. The user describes the analysis in chat or through the CLI.
2. The compiler classifies the intake, selects modality/archetype configuration from `config/`, and composes a task DAG from typed stage atoms.
3. The user confirms the plan through the server-side Accept gate. Chat text alone cannot authorize emission.
4. The emitter writes a self-contained RO-Crate package with `WORKFLOW.json`, policy files, runtime sidecars, and `ro-crate-metadata.json`.
5. The harness executes ready DAG tasks through a local, AWS, or SLURM executor and records task outputs under `runtime/outputs/<task_id>/`.
6. Result review, blockers, reruns, amendments, sensitivity selection, and branching are recorded as typed decisions.

The LLM is a UX layer around a closed tool vocabulary. High-impact actions are gated by deterministic server state and typed request handlers.

## ECAA v0.2 Contract

Every normal emitted package carries these eight required ECAA sidecars:

| Subgraph | Path |
|---|---|
| I - Intent | `runtime/intake-conversation.jsonl` |
| D - Decision | `runtime/decisions.jsonl` |
| E - Execution | `runtime/validation-reports.jsonl` |
| V - Evidence | `runtime/proofs.jsonl` |
| C - Claim | `runtime/claim-verification.json` |
| Q - Equivalence | `runtime/verifier-decisions.jsonl` |
| F - Failure | `runtime/assumptions.jsonl` |
| A - Audit-proof | `runtime/audit-proof-report.json` |

The source constants are in [`crates/ecaa-types/src/consts.rs`](crates/ecaa-types/src/consts.rs): ECAA version `0.2`, minimum reader version `0.2`, 25 node types, 20 edge predicates, 6 invariant IDs, 8 sidecar paths, and 6 required `conformsTo` profile IRIs.

There is no reduced ECAA mode switch in v0.2. Setting the retired `ECAA_ECAA_MODE` environment variable must not suppress sidecars. Non-conformant control artifacts are produced only by the six code-backed `ECAA_ABLATE_*` flags documented in [`docs/ecaa-spec/operations.md`](docs/ecaa-spec/operations.md).

## Validation Method

Emit-time validation writes `runtime/validation-summary.json`. The default mode is `schema_only`, which runs pure-Rust JSON Schema validation over the sidecars and remains warn-only unless `ECAA_VALIDATION_BLOCK_ON_FAIL=1` is set. `ECAA_VALIDATE_ON_EMIT=full` additionally attempts external SHACL, OWL, and runcrate checks when the required Python/tooling dependencies are installed.

The conformance gate is `make conformance`. It sets `ECAA_CONFORMANCE_MODE=1` and `ECAA_VALIDATION_BLOCK_ON_FAIL=1` inline, then exercises the conformance suite in `crates/ecaa-conformance/`.

## Audit-Proof Invariants

ECAA v0.2 evaluates six audit-proof invariants:

| Invariant | Default non-pass policy |
|---|---|
| `claim_completeness` | warn |
| `decision_justification` | warn |
| `evidence_coverage` | warn |
| `equivalence_failure` | fail |
| `cross_graph_integrity` | fail |
| `substrate_validity` | fail |

Verdicts are deterministic over package bytes and are written in `runtime/audit-proof-report.json` with `ecaa_version`, `min_reader_version`, the 6 verdict rows, and evaluator provenance. The predicate reference is [`docs/ecaa-spec/invariants.md`](docs/ecaa-spec/invariants.md).

## Local Production Defaults

[`.env.example`](.env.example) is the checked-in environment catalog. Its active defaults are intended for local production: loopback server URL, durable `$HOME/.ecaa-workflow` storage, local executor, subscription agent billing, and serial harness execution. Live API gates, eval-only knobs, debug flags, AWS/SLURM provisioning, external validators, and host-specific cache paths are commented until an operator deliberately enables them.

## Scope Limits

ECAA conformance proves machine-checkable consistency among claims, evidence, decisions, execution provenance, blockers, and audit-proof verdicts. It does not prove biological validity, statistical power, clinical appropriateness, or regulatory sufficiency. Those remain SME and institutional review responsibilities.
