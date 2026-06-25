<!-- docs/ecaa-spec/invariants.md -->
# ECAA Audit-Proof Invariants — Predicate Reference (v0.2)

Normative companion to `v0.2.md` §6. Defines the six audit-proof
invariants as first-order-logic predicates over the typed sub-graph
data model declared in `v0.2.md` §4–5. Reference implementation:
`crates/core/src/audit_proof/invariants/`.

## Verdict ladder

ECAA v0.2 defines four invariant verdicts. The set is **closed** —
implementations MUST emit exactly one of these per evaluated invariant.

| Verdict | Meaning |
|---|---|
| `pass` | Predicate evaluated and held over the relevant sub-graph data. |
| `warn` | Predicate evaluated and did NOT hold; spec policy is non-blocking. |
| `fail` | Predicate evaluated and did NOT hold; spec policy blocks under a strict hard-block policy. |
| `unverified` | Predicate could not be evaluated because a prerequisite is missing (e.g., the relevant sub-graph file is absent or an external tool like `runcrate` is unavailable at runtime). |

`unverified` is NOT a soft pass. Implementations MUST surface it in
`audit-proof-report.json` rather than coerce it to `pass`.

## Default warn/fail mapping (normative)

Each invariant defines a default verdict for non-`pass` cases. Implementations
MAY override globally to warn-only (typical for development environments)
but MUST record the override in `audit-proof-report.json` under
`evaluator.policy: "warn-only"`. Per-invariant overrides are out of scope
for v0.2.

| # | Invariant | Default on violation |
|---|---|---|
| 1 | `claim_completeness` | `warn` |
| 2 | `decision_justification` | `warn` |
| 3 | `evidence_coverage` | `warn` |
| 4 | `equivalence_failure` | `fail` |
| 5 | `cross_graph_integrity` | `fail` |
| 6 | `substrate_validity` | `fail` |

## Determinism requirement

Evaluators MUST be deterministic over package bytes. Predicates are
pure functions of the sub-graph data (plus, for invariant 6, an
external substrate validator). LLM-mediated predicates are NOT
v0.2-conformant.

## Predicate notation

Predicates use first-order logic over the typed object model
defined in `v0.2.md` §4. Sub-graph node sets are written `I.Questions`,
`D.MethodChoices`, etc. Edge sets are written as triples
`(source_id, target_id, predicate)`. Cross-graph references use the
prefix-scheme identifiers `<letter>:<id>` declared in `v0.2.md` §4.

## Invariants

### 1. `claim_completeness`

**One-line statement.** Every narrative claim verdict in the Claim graph is either supported by Evidence or explicitly marked pending.

**Predicate.**

```
∀ c ∈ C.verdicts :
    c.status = "pending"
  ∨ |c.supported_by| ≥ 1
```

If the signed runtime sink carries a `coverage` block, every required structured expected claim MUST be addressed; any required absent or unverifiable entry is a recall gap.

**Inputs.** C (`runtime/claim-verification.json` and, when present, the trusted signed sink under `runtime/verification-reports/claim-verification.signed.json`).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Predicate holds for all claim verdicts in C and required coverage has no gaps | `pass` |
| C is present and at least one non-pending verdict has empty `supported_by` | `warn` (default) |
| Signed-sink `coverage` contains required absent or unverifiable entries | `fail` |
| `runtime/claim-verification.json` is absent or has no `verdicts` array | `unverified` |
| Verdict array is empty and no signed-sink `coverage` block is present | `unverified` |

**Rationale.** Narrative claims with no traceable evidence are the failure mode the ECAA contract exists to address. Marking a claim `pending` is a legitimate acknowledged state — the spec explicitly carves it out so SMEs can communicate work-in-progress claims without violating the contract.

**Reference impl.** `crates/core/src/audit_proof/invariants/claim_completeness.rs`.

### 2. `decision_justification`

**One-line statement.** Every method choice carries either a citation or a free-text rationale of substantial length.

**Predicate.**

```
∀ m ∈ D.MethodChoices :
    (∃ e ∈ D.edges : e.predicate = "cites" ∧ e.source = m.id)
  ∨ length(m.rationale) ≥ 30
```

**Inputs.** D (decision graph) only. The 30-character threshold is normative for v0.2 — a future minor version MAY relax it; implementations MUST NOT silently lower it.

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Predicate holds for all method choices in D | `pass` |
| D contains zero `MethodChoice` entries | `unverified` |
| D is present and at least one MethodChoice violates the predicate | `warn` (default) |

**Rationale.** SME-stated method rationale is the only durable record of *why* an analysis chose DESeq2 over edgeR, BWA over Bowtie2, etc. Empty `rationale` strings with no `cites` edges convert the Decision sub-graph from auditable provenance into a stub.

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the `length(s) ≥ 30` constraint requires datatype facets that fall outside OWL DL. Encoded in `ecaa-v0.2.shacl.ttl` as a SHACL `sh:NodeShape`.

**Reference impl.** `crates/core/src/audit_proof/invariants/decision_justification.rs`.

### 3. `evidence_coverage`

**One-line statement.** Every output produced by the execution graph is either referenced as Evidence or explicitly marked unused.

**Predicate.**

```
∀ o ∈ analytical_outputs(package) :
    o ∈ strip_fragment(C.verdicts[].supported_by)
  ∨ basename(o) ∈ basename(strip_fragment(C.verdicts[].supported_by))
  ∨ (∃ a ∈ F.Assumptions : a.kind = "output_unused" ∧ a.detail = o)
```

**Inputs.** Analytical outputs from the RO-Crate output entities and real-path `proofs.jsonl` rows, C (`verdicts[].supported_by`), and F (`assumptions.jsonl` rows with `kind: "output_unused"`).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| No analytical outputs are declared | `unverified` |
| Every analytical output is referenced by C or marked unused in F | `pass` |
| At least one analytical output is neither referenced nor marked unused | `warn` (default) |
| C is absent while analytical outputs exist | `warn` |

**Rationale.** Outputs that exist on disk but appear in no Evidence reference are a strong signal of dead-code analysis — figures generated but not interpreted, tables computed but never shown. The `output_unused` carve-out lets the SME declare an output is incidental rather than analytically load-bearing.

**Reference impl.** `crates/core/src/audit_proof/invariants/evidence_coverage.rs`.

### 4. `equivalence_failure`

**One-line statement.** Every re-execution divergence is acknowledged by a Failure-graph blocker.

**Predicate.**

```
∀ r ∈ Q.RerunOutcomes :
    r.class ∉ {"failed", "acknowledged_non_determinism"}
  ∨ ∃ b ∈ F.Blockers :
        b.kind ∈ {"UnprovableEdge", "PolicyException"}
      ∧ b.refs ∋ r.id
```

**Inputs.** Q (equivalence graph), F (failure graph).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Re-execution ran and every divergent outcome is acknowledged | `pass` |
| At least one divergent outcome or compile-time prove failure is unacknowledged | `fail` (default) |
| Q is absent or `runtime/reexecution.json::per_artifact[]` is empty (no re-execution performed) | `unverified` |

**Rationale.** A re-execution that diverged or failed but produced no Blocker is the silent-corruption failure mode. The conformant emit path MUST surface divergence as a typed Blocker even when the SME's preferred recovery action is "accept the divergence as the new baseline".

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the closed value-set comparison against the `BlockerKind` enum requires reasoning over a finite set of named individuals which extends OWL DL with closed-world assumptions. Encoded in `ecaa-v0.2.shacl.ttl`.

**Reference impl.** `crates/core/src/audit_proof/invariants/equivalence_failure.rs`.

### 5. `cross_graph_integrity`

**One-line statement.** Every cross-sub-graph reference dereferences to an existing node.

**Predicate.**

```
∀ e ∈ ⋃_{G∈{I,D,E,V,C,Q,F,A}} G.edges :
    cross_graph(e) ⇒
        ∃ G' ∈ {I,D,E,V,C,Q,F,A} :
            (e.target matches "<G'.letter>:<id>")
          ∧ (∃ n ∈ G'.nodes : n.id = e.target_local_id)
```

where `cross_graph(e)` is true iff `e.target` is prefixed with a sub-graph letter (`I:`, `D:`, `E:`, `V:`, `C:`, `Q:`, `F:`, `A:`).

**Inputs.** All 8 sub-graphs.

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Every cross-graph reference resolves | `pass` |
| At least one cross-graph reference dangles | `fail` (default) |

**Rationale.** Dangling references between sub-graphs break the typed-object closure. A `Claim` with `supported_by: V:fig_3a` is meaningless if no `V` node has id `fig_3a` — and worse, it silently masquerades as a supported claim under invariant 1.

**Reference impl.** `crates/core/src/audit_proof/invariants/cross_graph_integrity.rs`.

### 6. `substrate_validity`

**One-line statement.** The package loads under WRROC v0.5 Tier-3 readers and passes four post-checks; its declared `conformsTo` profiles are EXECUTION-AWARE — a plan crate claims only the profiles a workflow definition truthfully meets, an executed crate additionally claims the WRROC v0.5 run profiles.

**Predicate.**

```
package.passes(`runcrate report ≥ 0.5.0` parseability proxy; runcrate ships no `validate` subcommand)
  ∧ records_execution(package)
        ? REQUIRED_PROFILE_IRIS ⊆ package.conformsTo            (executed crate: all 6)
        : PLAN_PROFILE_IRIS     ⊆ package.conformsTo            (plan crate: the 3 definition profiles)
  ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "wfprov:ParameterConnection"
  ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "p-plan:Plan"
  ∧ ∀ sidecar ∈ REQUIRED_SIDECARS :
        sidecar ∈ package.@graph as CreativeWork
```

where `records_execution(package)` is true iff `∃ entity ∈ package.@graph : entity.@type ∋ "CreateAction" ∧ entity.instrument` (a real run `CreateAction` carrying an `instrument`); `PLAN_PROFILE_IRIS` is the three definition profiles (RO-Crate 1.1 + WorkflowHub Workflow-RO-Crate 1.0 + ECAA v0.2) and `REQUIRED_PROFILE_IRIS` is the full six-IRI set, both declared in `v0.2.md` §3.1; and `REQUIRED_SIDECARS` is the eight-filename set declared in the same section. The conformsTo conjunct is a ⊆ (superset) floor over the branch the crate's `records_execution` value selects: a plan crate MUST declare ⊇ the 3 definition profiles, an executed crate MUST declare ⊇ all 6, and either MAY declare additional implementation-specific IRIs. The reference emitter is what keeps a plan crate from CLAIMING a WRROC v0.5 run profile it cannot truthfully meet — it emits only `PLAN_PROFILE_IRIS` on a plan crate and adds the run profiles only on execution; the invariant's job is to reject the dangerous direction (UNDER-declaration), so an executed crate that drops any of the 6 fails, and a plan crate that drops any of the 3 fails.

**Inputs.** The package's `ro-crate-metadata.json` and the external `runcrate` tool.

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| All four sub-conditions hold | `pass` |
| One or more sub-conditions fails | `fail` (default) |
| `runcrate` is unavailable at runtime | `unverified` (REQUIRED — implementations MUST NOT coerce to `pass`) |

**Rationale.** The WRROC binding is what makes ECAA a portable analysis package rather than a project-local file convention. Substrate-validity is the gate that lets a v0.2-conformant package be consumed by every existing WRROC-compatible reader (WorkflowHub.eu, BCO crosswalk tools, etc.).

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the external-tool dependency on `runcrate` is outside any RDF schema language. Encoded in `ecaa-v0.2.shacl.ttl` only as a structural SHACL shape over the `@graph`; the `runcrate` invocation is performed by the conformance suite's Python harness.

**Reference impl.** `crates/core/src/audit_proof/invariants/substrate_validity.rs`.

## Semantics

Default operational policy is **warn-only**: invariant verdicts are
recorded in `runtime/audit-proof-report.json` but never block
`emit_package`. Implementations MAY adopt hard-block policies for
specific deployment contexts; see operations.md §3.

## `audit-proof-report.json` shape (normative)

```json
{
  "schema_version": "0.1",
  "ecaa_version": "0.2",
  "min_reader_version": "0.2",
  "package_iri": "<IRI of the package's ro-crate-metadata.json>",
  "evaluated_at": "<RFC 3339 timestamp>",
  "verdicts": [
    {
      "id": "claim_completeness",
      "status": "pass",
      "detail": null,
      "n_inspected": 12,
      "n_violations": 0
    }
  ],
  "evaluator": {
    "impl": "ecaa-workflow-audit-proof",
    "version": "1.0.0",
    "policy": "warn-only"
  }
}
```

**Verdict fields.** Each verdict MUST carry `id`, `status`, `n_inspected`, and `n_violations`. `detail` is nullable and SHOULD explain every non-`pass` result.

**`evaluator.policy`.** When the implementation has overridden the per-invariant default warn/fail mapping (per §"Default warn/fail mapping"), `policy` MUST be `"warn-only"` or `"strict"`. Absence implies the normative defaults from this document apply.

**Deterministic comparison.** Two ECAA-v0.2-conformant evaluators evaluating the same package bytes MUST produce verdict arrays that agree on every `id`'s `status` value. The `detail` field is informative and MAY differ in wording. The `evaluator` object is informative.
