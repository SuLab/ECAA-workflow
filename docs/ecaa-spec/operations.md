<!-- docs/ecaa-spec/operations.md -->
# ECAA Validity-Preserving Operations — Operation Contracts (v0.1)

Normative companion to `v0.1.md` §7. Defines the four operations
that ECAA-v0.1-conformant implementations MUST implement: `compose`,
`amend`, `re-execute`, and `ablate`. Each operation is specified by
its pre-condition, post-condition, and the ablation contract guarantee.

## Operation summary

| Operation | Pre-condition | Post-condition |
|---|---|---|
| `compose` | A valid Intent sub-graph (per `subgraph-schemas/intent.schema.json`). | A package P satisfying `v0.1.md` §3 substrate requirements with all 8 sub-graph sidecars present and `audit-proof-report.json` recording 6 verdicts. |
| `amend` | P is v0.1-conformant; Δ targets D only (v0.1 restriction). | A new package P′ with `prov:wasDerivedFrom` lineage to P; cross-graph-integrity invariant preserved; P′.A references P.A. |
| `re-execute` | P is v0.1-conformant; re-execution environment recorded. | Every E.OutputFile has a corresponding Q.RerunOutcome with class assignment; P′.A freshly evaluated against P′ (not inherited). |
| `ablate` | A pre-emission package state. | A non-conformant package P′ from which one or more sub-graphs are omitted per the 6-flag ablation contract (§4). |

## 1. `compose`

### 1.1 Signature

`compose : Intent → Package`

### 1.2 Pre-conditions

- Input I MUST validate against `subgraph-schemas/intent.schema.json`.
- Input I MUST declare exactly one Modality node.
- Input I MUST declare ≥1 Question node and ≥1 ExpectedOutput node.

### 1.3 Post-conditions

- Output package P MUST satisfy all six normative `conformsTo` profile IRIs (`v0.1.md` §3).
- Output package P MUST contain all 8 sidecars at their normative paths (`v0.1.md` §3 sidecar table).
- Output package P's `audit-proof-report.json` MUST record exactly 6 `InvariantVerdict` nodes.
- Output package P MUST round-trip through `runcrate validate ≥ 0.5.0` (verifying via the substrate-validity invariant).
- Conformance MAY be claimed only when the invariant verdicts are evaluated post-emission, not predicted pre-emission.

### 1.4 Determinism guarantee

For identical input I, `compose(I) = compose(I)` byte-wise, with the following documented exclusions:
- The `evaluated_at` timestamp in `audit-proof-report.json`.
- RNG-seeded entries explicitly declared as non-reproducible in `decisions.jsonl`.
- `intake-conversation.jsonl` and `decisions.jsonl` — SME-interaction-dependent, NOT byte-reproducible.

All other sidecars and the `ro-crate-metadata.json` descriptor MUST be byte-reproducible.

### 1.5 Reference implementation

`scripps-workflow intake` (non-interactive path) + `scripps-workflow build` (taxonomy-driven path). Both exit after emission; neither requires a live LLM connection in the deterministic path.

## 2. `amend`

### 2.1 Signature

`amend : Package × DecisionDelta → Package`

### 2.2 Pre-conditions

- Input package P MUST be v0.1-conformant (passing the 5-item conformance bar in `v0.1.md` §8).
- Amendment Δ MUST target the D sub-graph only in v0.1. Amendments to I, E, V, C, Q, F are RESERVED for future versions (see §"Out of scope" in `v0.1.md` §13).
- Δ MUST specify exactly one of: a new `MethodChoice` node (with cardinality contracts per `v0.1.md` §5.2), an updated `Justification` node, an added `Citation` node, or an added `Alternative` node.

### 2.3 Post-conditions

- Output package P′ MUST contain at least one D-graph node with a `prov:wasDerivedFrom` edge → corresponding node in P.
- P′'s cross-graph-integrity invariant (Invariant 5) MUST hold: all dangling references to amended/replaced D nodes MUST be repaired.
- P′'s `audit-proof-report.json` MUST reference P's `audit-proof-report.json` IRI via a `derived_from` field (informative).
- P′ MUST be re-emitted with a fresh `evaluated_at` timestamp and freshly evaluated verdicts (NOT inherited from P).

### 2.4 Lineage guarantee

The chain `P₀ →amend P₁ →amend P₂ →amend … →amend Pₙ` MUST be traversable in a single direction by following `prov:wasDerivedFrom` edges. Implementations MUST refuse a cycle in this chain.

### 2.5 Reference implementation

`scripps-workflow chat` `/amend` slash-command, mediated by the `amend_stage_method` LLM tool, gated by the deterministic server-state `Amending{target_stage, invalidated_tasks}` transition.

## 3. `re-execute`

### 3.1 Signature

`re-execute : Package × Environment → Package`

### 3.2 Pre-conditions

- Input package P MUST be v0.1-conformant.
- The re-execution Environment MUST be recordable in `RuntimeEnvironment` nodes (container digests, OS version, hardware capability flags).

### 3.3 Post-conditions

- Every E.OutputFile in P′ MUST have a corresponding Q.RerunOutcome with `class` assigned from the closed 5-class set: `byte_identical`, `semantic_equivalent`, `acknowledged_non_determinism`, `unavailable`, `failed`.
- P′'s `audit-proof-report.json` MUST be freshly evaluated against P′. Implementations MUST NOT copy the report from P.
- P′ MUST declare lineage to P via `prov:wasDerivedFrom` in the package descriptor (NOT just in D).

### 3.4 Equivalence rules

Per-modality semantic-equivalence rules (numerical tolerance per output type) are declared by the implementation in a non-normative `equivalence-rules.json` file referenced from `Q.RerunOutcome.diverges-from` edges. v0.1 normatively fixes the 5 class names; it does NOT fix the per-modality tolerances.

Implementations MUST document their equivalence-rules.json content alongside the package so that re-execution outcomes are interpretable.

### 3.5 Reference implementation

The harness `--rerun` mode invokes the deterministic shim (`crates/core/src/determinism_shim.rs`) to classify Q outcomes; the audit-proof checker re-evaluates against P′ post-rerun.

## 4. `ablate`

### 4.1 Signature

`ablate : Package × {AblationFlag} → NonConformantPackage`

where `{AblationFlag}` is a finite subset of the six normative flags (§4.3).

### 4.2 Pre-conditions

- Input package state is pre-emission (the flags are typically engaged during `compose`).
- Each flag in the subset is one of the six normative names (§4.3); unknown flag names MUST be rejected.

### 4.3 The ablation contract (NORMATIVE, load-bearing)

The six `SWFC_ABLATE_*` flags suppress exactly one sub-graph artifact each:

| Flag | Suppresses | Effect on conformance |
|---|---|---|
| `SWFC_ABLATE_DECISION_RECORDS` | `runtime/decisions.jsonl` (D sub-graph) | Non-conformant: D missing |
| `SWFC_ABLATE_AMENDMENT_PROVENANCE` | `prov:wasDerivedFrom` edges within D | Non-conformant: D lacks required predicate |
| `SWFC_ABLATE_CLAIM_CONSISTENCY` | `runtime/claim-verification.json` (C sub-graph) | Non-conformant: C missing |
| `SWFC_ABLATE_TYPED_BLOCKERS` | typed `kind` field on F.Blocker nodes | Non-conformant: F nodes lack required typed kind |
| `SWFC_ABLATE_REEXECUTION_CLASS` | `class` field on Q.RerunOutcome nodes | Non-conformant: Q nodes lack required class assignment |
| `SWFC_ABLATE_AUDIT_PROOF` | `runtime/audit-proof-report.json` (A sub-graph) | Non-conformant: A missing |

### 4.4 The byte-identity contract

The ablation contract is more than a list of suppressed files — it is a guarantee about upstream behavior. For any package P:

```
∀ flag ∈ AblationFlags :
    let P_flagged   = compose(I, flags = {flag})
    let P_unflagged = compose(I, flags = ∅)
in
    bytewise_diff(P_flagged, P_unflagged) ⊆ {sidecar(flag)}
```

That is: engaging one flag MUST modify ONLY the corresponding sidecar (or its declared sub-field). Upstream agent behavior — task decomposition, code generation, tool calls, container choice, retry, orchestration — MUST be byte-identical between the flagged and unflagged compose runs.

This contract is what makes the Aim 3A A vs. B′ contrast a mechanistic causal test of the ECAA layer rather than just a feature comparison.

### 4.5 Conformance suite verification

The D9 conformance suite (`crates/ecaa-conformance/tests/ablation_contract.rs`) verifies one-flag-at-a-time byte-identity on a fixture corpus. A second-implementation candidate that fails this test cannot claim ECAA v0.1 conformance even if all other conformance bars pass.

### 4.6 Hard-block adoption mode (informative)

Implementations that adopt the optional hard-block policy SHOULD route an audit-proof `Fail` verdict to a typed blocker (e.g., `BlockerKind::AuditProofInvariantFailure`) that the SME can override. This is out of scope for v0.1; the default operational policy is warn-only.

### 4.7 Reference implementation

`crates/core/src/ablation.rs` declares the closed 6-variant `AblationFlag` enum; `crates/core/src/audit_proof/mod.rs` and the emit-side gates respect each flag's contract. Ablation contract testing: `crates/ecaa-conformance/tests/ablation_contract.rs`.
