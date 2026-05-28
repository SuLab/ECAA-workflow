<!-- docs/ecaa-spec/invariants.md -->
# ECAA Audit-Proof Invariants — Predicate Reference (v0.1)

Normative companion to `v0.1.md` §6. Defines the six audit-proof
invariants as first-order-logic predicates over the typed sub-graph
data model declared in `v0.1.md` §4–5. Reference implementation:
`crates/core/src/audit_proof/invariants/`.

## Verdict ladder

ECAA v0.1 defines four invariant verdicts. The set is **closed** —
implementations MUST emit exactly one of these per evaluated invariant.

| Verdict | Meaning |
|---|---|
| `Pass` | Predicate evaluated and held over the relevant sub-graph data. |
| `Warn` | Predicate evaluated and did NOT hold; spec policy is non-blocking. |
| `Fail` | Predicate evaluated and did NOT hold; spec policy blocks (a hard-block-policy implementation refuses emission). |
| `Unverified` | Predicate could not be evaluated because a prerequisite is missing (e.g., the relevant sub-graph file is absent or an external tool like `runcrate` is unavailable at runtime). |

`Unverified` is NOT a soft pass. Implementations MUST surface it in
`audit-proof-report.json` rather than coerce it to `Pass`.

## Default warn/fail mapping (normative)

Each invariant defines a default verdict for non-Pass cases. Implementations
MAY override globally to warn-only (typical for development environments)
but MUST record the override in `audit-proof-report.json` under
`evaluator.policy: "warn-only"`. Per-invariant overrides are out of scope
for v0.1.

| # | Invariant | Default on violation |
|---|---|---|
| 1 | `claim_completeness` | Warn |
| 2 | `decision_justification` | Warn |
| 3 | `evidence_coverage` | Warn |
| 4 | `equivalence_failure` | Fail |
| 5 | `cross_graph_integrity` | Fail |
| 6 | `substrate_validity` | Fail |

## Determinism requirement

Evaluators MUST be deterministic over package bytes. Predicates are
pure functions of the sub-graph data (plus, for invariant 6, an
external substrate validator). LLM-mediated predicates are NOT
v0.1-conformant.

## Predicate notation

Predicates use first-order logic over the typed object model
defined in `v0.1.md` §4. Sub-graph node sets are written `I.Questions`,
`D.MethodChoices`, etc. Edge sets are written as triples
`(source_id, target_id, predicate)`. Cross-graph references use the
prefix-scheme identifiers `<letter>:<id>` declared in `v0.1.md` §4.

## Invariants

### 1. `claim_completeness`

**One-line statement.** Every narrative claim in the Claim graph is either supported by Evidence or explicitly marked pending.

**Predicate.**

```
∀ c ∈ C.Claims :
    c.status = "pending"
  ∨ ∃ e ∈ C.edges :
        e.predicate = "supported-by"
      ∧ e.source = c.id
      ∧ e.target ∈ V.Statistics ∪ V.Figures ∪ V.Tables
```

**Inputs.** C (claim graph). V (evidence graph — only the existence and types of `Statistic` / `Figure` / `Table` nodes are read).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Predicate holds for all claims in C | `Pass` |
| C is present and at least one claim violates the predicate | `Warn` (default) |
| `runtime/claim-verification.json` is absent | `Unverified` |

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

**Inputs.** D (decision graph) only. The 30-character threshold is normative for v0.1 — a future minor version MAY relax it; implementations MUST NOT silently lower it.

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Predicate holds for all method choices in D | `Pass` |
| D contains zero `MethodChoice` entries | `Unverified` |
| D is present and at least one MethodChoice violates the predicate | `Warn` (default) |

**Rationale.** SME-stated method rationale is the only durable record of *why* an analysis chose DESeq2 over edgeR, BWA over Bowtie2, etc. Empty `rationale` strings with no `cites` edges convert the Decision sub-graph from auditable provenance into a stub.

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the `length(s) ≥ 30` constraint requires datatype facets that fall outside OWL DL. Encoded in `ecaa-v0.1.shacl.ttl` as a SHACL `sh:NodeShape`.

**Reference impl.** `crates/core/src/audit_proof/invariants/decision_justification.rs`.

### 3. `evidence_coverage`

**One-line statement.** Every output produced by the execution graph is either referenced as Evidence or explicitly marked unused.

**Predicate.**

```
∀ o ∈ E.OutputFiles :
    (∃ e ∈ V.edges : e.predicate = "computed-from" ∧ e.target = o.id)
  ∨ (∃ b ∈ F.Blockers : b.kind = "OutputUnused" ∧ b.refs ∋ o.id)
```

**Inputs.** E (execution graph), V (evidence graph), F (failure graph).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Every E.OutputFile is referenced in V or marked unused in F | `Pass` |
| At least one OutputFile is neither referenced nor marked unused | `Warn` (default) |
| C is absent entirely | `Warn` (cannot evaluate evidence-side; design `audit_proof/invariants/evidence_coverage.rs` Warn-on-no-C semantics) |

**Rationale.** Outputs that exist on disk but appear in no Evidence reference are a strong signal of dead-code analysis — figures generated but not interpreted, tables computed but never shown. The `output_unused` carve-out lets the SME declare an output is incidental rather than analytically load-bearing.

**Reference impl.** `crates/core/src/audit_proof/invariants/evidence_coverage.rs`.

### 4. `equivalence_failure`

**One-line statement.** Every re-execution divergence is acknowledged by a Failure-graph blocker.

**Predicate.**

```
∀ r ∈ Q.RerunOutcomes :
    r.class ∉ {"failed", "non-deterministic"}
  ∨ ∃ b ∈ F.Blockers :
        b.kind ∈ {"UnprovableEdge", "PolicyException"}
      ∧ b.refs ∋ r.id
```

**Inputs.** Q (equivalence graph), F (failure graph).

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| Predicate holds for all rerun outcomes | `Pass` |
| At least one rerun outcome violates the predicate | `Fail` (default) |
| Q is absent (no re-execution performed) | `Unverified` |

**Rationale.** A re-execution that diverged or failed but produced no Blocker is the silent-corruption failure mode. The conformant emit path MUST surface divergence as a typed Blocker even when the SME's preferred recovery action is "accept the divergence as the new baseline".

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the closed value-set comparison against the `BlockerKind` enum requires reasoning over a finite set of named individuals which extends OWL DL with closed-world assumptions. Encoded in `ecaa-v0.1.shacl.ttl`.

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
| Every cross-graph reference resolves | `Pass` |
| At least one cross-graph reference dangles | `Fail` (default) |

**Rationale.** Dangling references between sub-graphs break the typed-object closure. A `Claim` with `supported-by: V:fig_3a` is meaningless if no `V` node has id `fig_3a` — and worse, it silently masquerades as a supported claim under invariant 1.

**Reference impl.** `crates/core/src/audit_proof/invariants/cross_graph_integrity.rs`.

### 6. `substrate_validity`

**One-line statement.** The package loads under WRROC v0.5 Tier-3 readers and passes four post-checks.

**Predicate.**

```
package.passes(`runcrate validate ≥ 0.5.0`)
  ∧ |{iri ∈ package.conformsTo : iri ∈ REQUIRED_PROFILE_IRIS}| = 6
  ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "wfprov:ParameterConnection"
  ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "p-plan:Plan"
  ∧ ∀ sidecar ∈ REQUIRED_SIDECARS :
        sidecar ∈ package.@graph as CreativeWork
```

where `REQUIRED_PROFILE_IRIS` is the six-IRI set declared in `v0.1.md` §3 and `REQUIRED_SIDECARS` is the eight-filename set declared in the same section.

**Inputs.** The package's `ro-crate-metadata.json` and the external `runcrate` tool.

**Verdict mapping.**

| Condition | Verdict |
|---|---|
| All four sub-conditions hold | `Pass` |
| One or more sub-conditions fails | `Fail` (default) |
| `runcrate` is unavailable at runtime | `Unverified` (REQUIRED — implementations MUST NOT coerce to Pass) |

**Rationale.** The WRROC binding is what makes ECAA a portable analysis package rather than a project-local file convention. Substrate-validity is the gate that lets a v0.1-conformant package be consumed by every existing WRROC-compatible reader (WorkflowHub.eu, BCO crosswalk tools, etc.).

**OWL expressibility.** This predicate is NOT expressible in OWL 2 DL — the external-tool dependency on `runcrate` is outside any RDF schema language. Encoded in `ecaa-v0.1.shacl.ttl` only as a structural SHACL shape over the `@graph`; the `runcrate` invocation is performed by the conformance suite's Python harness.

**Reference impl.** `crates/core/src/audit_proof/invariants/substrate_validity.rs`.

## Semantics

Default operational policy is **warn-only**: invariant verdicts are
recorded in `runtime/audit-proof-report.json` but never block
`emit_package`. Implementations MAY adopt hard-block policies for
specific deployment contexts; see operations.md §3.

## `audit-proof-report.json` shape (normative)

```json
{
  "ecaa_version": "0.1",
  "package_iri": "<IRI of the package's ro-crate-metadata.json>",
  "evaluated_at": "<RFC 3339 timestamp>",
  "min_reader_version": "0.1",
  "verdicts": [
    {
      "invariant_id": "claim-completeness",
      "verdict": "Pass",
      "evidence": [],
      "details": "all 12 claims have supported-by edges or status=pending"
    }
  ],
  "evaluator": {
    "impl": "scripps-workflow-audit-proof",
    "version": "1.0.0",
    "policy": "warn-only"
  }
}
```

**`evidence` field.** REQUIRED on every non-`Pass` verdict. Each entry is `{path: <string>, reason: <string>}` where `path` SHOULD be a prefix-scheme reference (e.g., `D:method_001`) or a relative filesystem path. OPTIONAL on `Pass` verdicts.

**`evaluator.policy`.** When the implementation has overridden the per-invariant default warn/fail mapping (per §"Default warn/fail mapping"), `policy` MUST be `"warn-only"` or `"strict"`. Absence implies the normative defaults from this document apply.

**Deterministic comparison.** Two ECAA-v0.1-conformant evaluators evaluating the same package bytes MUST produce verdict arrays that agree on every `invariant_id`'s `verdict` value. The `evidence` and `details` fields are informative and MAY differ in wording. The `evaluator` object is informative.
