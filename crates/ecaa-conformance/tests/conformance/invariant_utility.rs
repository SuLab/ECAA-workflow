//! Invariant-utility (specificity) matrix.
//!
//! Proves the six audit-proof invariants each fire on their OWN injected
//! violation — and ONLY their own. For every injected corruption we assert
//! two things:
//!   (a) the TARGET invariant flips from its clean-baseline verdict to the
//!       predicted non-Pass status (Warn or Fail), and
//!   (b) the OTHER FIVE invariants are byte-for-byte unchanged from the
//!       clean baseline (isolation / no collateral detection).
//!
//! Each case copies the hand-built `tests/fixtures/minimal-package` into a
//! fresh tempdir, applies a single minimal mutator that corrupts exactly one
//! invariant's precondition, then runs `run_audit_proof` over the mutated
//! tree with `NoopWrrocValidator`.
//!
//! SUBSTRATE CAVEAT (Invariant 6): `NoopWrrocValidator::validate_outcome`
//! returns `Unverified` regardless of the descriptor's contents, so a
//! `conformsTo`-IRI mutation can never reach `Fail` under Noop. That single
//! row is therefore gated behind `ECAA_CONFORMANCE_MODE` + a reachable
//! `runcrate` toolchain (driven by the harness `PythonRuncrateWrrocValidator`);
//! when the toolchain is absent it is reported as "requires runcrate; not
//! exercised hermetically" and SKIPPED without failing the test. The other
//! five invariants are exercised fully hermetically under Noop.

use ecaa_workflow_conformance::{
    run_audit_proof, AuditProofReport, InvariantId, InvariantStatus, InvariantVerdict,
    NoopWrrocValidator,
};
use ecaa_workflow_core::clock::WallClock;
use ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator;
use serde_json::Value;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixture plumbing
// ---------------------------------------------------------------------------

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("complete-package")
}

/// The all-`Pass` baseline package transcribed from `docs/ecaa-spec/v0.1.md`
/// Appendix C ("a minimal valid ECAA package"). Under `NoopWrrocValidator`
/// the five hermetic invariants genuinely `Pass`; substrate is `Unverified`
/// under Noop by design (exercised in Task 5).
///
/// SPEC/IMPL SHAPE GAP (reference-impl limitation, not a spec defect): the
/// spec models each sub-graph as typed nodes + `{source,target,predicate}`
/// edge triples, but the reference invariants under
/// `crates/core/src/audit_proof/invariants/` read flat per-row fields instead
/// — `proofs[].computed_from`/`produces` (output path string),
/// `claim-verification.json::verdicts[].supported_by` (output-path array), and
/// `decisions[].decision.kind == "set_intake_method"` + record `rationale`.
/// So each fixture sidecar carries BOTH shapes: the Appendix C nodes/edges (for
/// spec fidelity) AND the impl-read flat fields (so the invariants reach
/// `Pass`), kept value-consistent (the single output `data/figures/fig_qc.png`
/// is the `computed_from` target AND the claim's `supported_by` reference).
/// The fixture is NOT scoped down — all five hermetic invariants `Pass`
/// cleanly. (`docs/known-limitations.md` is git-ignored in this slim surface,
/// so the gap is recorded here in trackable code rather than that file.)
fn complete_fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("complete-package")
}

/// Recursively copy `src` → `dst`.
fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).expect("mkdir dst");
    for entry in std::fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().expect("file_type").is_dir() {
            copy_tree(&from, &to);
        } else {
            std::fs::copy(&from, &to).expect("copy file");
        }
    }
}

/// Copy the clean fixture into a fresh tempdir and return the guard + root.
fn fresh_package() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().join("pkg");
    copy_tree(&fixture_root(), &root);
    (tmp, root)
}

// ---- small JSONL / JSON helpers for the mutators -------------------------

fn runtime(root: &Path) -> PathBuf {
    root.join("runtime")
}

/// Append one JSON object as a new JSONL line to `runtime/<name>`.
fn append_jsonl(root: &Path, name: &str, value: &Value) {
    let path = runtime(root).join(name);
    let mut raw = std::fs::read_to_string(&path).unwrap_or_default();
    if !raw.is_empty() && !raw.ends_with('\n') {
        raw.push('\n');
    }
    raw.push_str(&serde_json::to_string(value).expect("serialize jsonl row"));
    raw.push('\n');
    std::fs::write(&path, raw).expect("write jsonl");
}

/// Overwrite `runtime/<name>` (a single JSON document) with `value`.
fn write_json(root: &Path, name: &str, value: &Value) {
    let path = runtime(root).join(name);
    std::fs::write(&path, serde_json::to_string_pretty(value).expect("serialize json"))
        .expect("write json");
}

fn read_json(root: &Path, name: &str) -> Value {
    let path = runtime(root).join(name);
    let raw = std::fs::read_to_string(&path).expect("read json");
    serde_json::from_str(&raw).expect("parse json")
}

/// Parse `runtime/<name>` (JSONL) into one `Value` per non-empty line.
/// Used by spec-fidelity assertions to confirm a mutator wrote the node/edge
/// the spec predicate ranges over.
fn read_jsonl(root: &Path, name: &str) -> Vec<Value> {
    let path = runtime(root).join(name);
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("parse jsonl row"))
        .collect()
}

// ---------------------------------------------------------------------------
// Verdict lookup helpers
// ---------------------------------------------------------------------------

fn verdict<'a>(report: &'a AuditProofReport, id: InvariantId) -> &'a InvariantVerdict {
    report
        .verdicts
        .iter()
        .find(|v| v.id == id)
        .unwrap_or_else(|| panic!("report missing invariant {id:?}"))
}

fn run(root: &Path) -> AuditProofReport {
    run_audit_proof(root, &NoopWrrocValidator, &WallClock).expect("run_audit_proof")
}

/// The non-degenerate baseline: a complete package (spec Appendix C) where
/// the five hermetic invariants genuinely `Pass` under Noop. This is what
/// makes every injected violation a real `Pass → Warn/Fail` flip (rather
/// than `Unverified → Warn`) and lets the false-positive claim ("a valid
/// package passes") exist at all.
#[test]
fn complete_fixture_is_all_pass_for_hermetic_invariants() {
    let (_g, root) = {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("pkg");
        copy_tree(&complete_fixture_root(), &root);
        (tmp, root)
    };
    let report = run(&root); // Noop validator
    for id in [
        InvariantId::ClaimCompleteness,
        InvariantId::DecisionJustification,
        InvariantId::EvidenceCoverage,
        InvariantId::EquivalenceFailure,
        InvariantId::CrossGraphIntegrity,
    ] {
        assert_eq!(
            verdict(&report, id).status,
            InvariantStatus::Pass,
            "complete fixture must Pass {id:?} (detail: {:?})",
            verdict(&report, id).detail
        );
    }
    // substrate is Unverified under Noop by design (exercised in Task 5).
    assert_eq!(
        verdict(&report, InvariantId::SubstrateValidity).status,
        InvariantStatus::Unverified
    );
}

/// Assert every invariant EXCEPT `target` has the same status as in `baseline`.
fn assert_others_unchanged(
    baseline: &AuditProofReport,
    mutated: &AuditProofReport,
    target: InvariantId,
) {
    for id in InvariantId::ALL {
        if id == target {
            continue;
        }
        let b = verdict(baseline, id).status;
        let m = verdict(mutated, id).status;
        assert_eq!(
            b, m,
            "ISOLATION VIOLATION: injecting a {target:?} fault also changed \
             {id:?} (baseline={b:?}, mutated={m:?})"
        );
    }
}

fn status_glyph(s: InvariantStatus) -> &'static str {
    match s {
        InvariantStatus::Pass => "PASS",
        InvariantStatus::Warn => "WARN",
        InvariantStatus::Fail => "FAIL",
        InvariantStatus::Unverified => "UNVERIFIED",
        _ => "??",
    }
}

// ---------------------------------------------------------------------------
// Per-invariant mutators (spec-derived)
//
// Each mutator's contract is the NORMATIVE first-order-logic predicate from
// `docs/ecaa-spec/invariants.md` (quoted verbatim in its doc comment), stated
// over the §5 node/edge vocabulary — NOT the Rust field names of the reference
// implementation. The mutator falsifies that spec predicate, then asserts
// in-line that the spec-level node/edge it ranges over is actually present in
// the mutated sub-graph. This makes the spec the independent oracle: if the
// implementation drifted from the spec predicate, the matrix flip would stop
// reproducing and the test would fail, rather than the test silently tracking
// the implementation.
//
// RESIDUAL CIRCULARITY (honest scope): spec-grounding REDUCES but does not
// ELIMINATE circularity. The same workspace emits the package AND evaluates
// the invariants, so a shared misreading of the spec could be masked by both.
// Full implementation-independence requires a SECOND, independent
// emitter/evaluator running this same harness against its own packages (out of
// scope here; this is the preprint's stated implementation-independence
// limitation). What spec-grounding buys: the mutators are now derived from
// `invariants.md`, so a divergence between the spec predicate and the
// implementation surfaces as a test failure instead of being absorbed.
//
// SPEC §5 vs IMPL FIELDS: the spec models sub-graphs as nodes +
// `{source,target,predicate}` edge triples; the reference impl reads flat
// per-row fields. The mutators therefore write the impl-read field that
// encodes the spec edge (and the fidelity assertions check that field), with
// the spec predicate quoted so the intent is anchored to the normative text.
// ---------------------------------------------------------------------------

/// Invariant 1 — `claim_completeness` → Warn.
///
/// Spec predicate (`invariants.md` §1, verbatim):
/// ```text
/// ∀ c ∈ C.Claims :
///     c.status = "pending"
///   ∨ ∃ e ∈ C.edges :
///         e.predicate = "supported-by"
///       ∧ e.source = c.id
///       ∧ e.target ∈ V.Statistics ∪ V.Figures ∪ V.Tables
/// ```
/// Violation injected: a `C.Claim` with `status = "verified"` (NOT "pending")
/// and NO `supported-by` edge — falsifying both disjuncts.
fn mutate_claim_completeness(root: &Path) {
    let mut claims = read_json(root, "claim-verification.json");
    let verdicts = claims
        .get_mut("verdicts")
        .and_then(|v| v.as_array_mut())
        .expect("verdicts array");
    verdicts.push(serde_json::json!({
        "claim_id": "claim_orphan_001",
        "status": "verified",
        "supported_by": []
    }));
    write_json(root, "claim-verification.json", &claims);

    // Spec-fidelity: the C sub-graph now holds a non-pending Claim with no
    // supported-by edge (the node/edge shape the §1 predicate ranges over).
    let claims = read_json(root, "claim-verification.json");
    let v = claims["verdicts"].as_array().expect("verdicts array");
    assert!(
        v.iter().any(|c| c["status"] == "verified"
            && c["supported_by"]
                .as_array()
                .map_or(true, |a| a.is_empty())),
        "spec §1 violation (verified Claim with no supported-by) not present in C sub-graph"
    );
}

/// Invariant 2 — `decision_justification` → Warn.
///
/// Spec predicate (`invariants.md` §2, verbatim):
/// ```text
/// ∀ m ∈ D.MethodChoices :
///     (∃ e ∈ D.edges : e.predicate = "cites" ∧ e.source = m.id)
///   ∨ length(m.rationale) ≥ 30
/// ```
/// Violation injected: a `D.MethodChoice` (encoded as the `set_intake_method`
/// decision the impl reads) with NO `cites` edge and `length(rationale) < 30`.
fn mutate_decision_justification(root: &Path) {
    append_jsonl(
        root,
        "decisions.jsonl",
        &serde_json::json!({
            "schema_version": "0.1.0",
            "timestamp": "2026-05-18T00:00:01Z",
            "session_id": "minimal-session-001",
            "decision": {
                "kind": "set_intake_method",
                "stage": "alignment",
                "method_prose": "STAR"
            },
            "rationale": "fast",
            "actor": "sme"
        }),
    );

    // Spec-fidelity: the D sub-graph now holds a MethodChoice (set_intake_method)
    // with no `cites` edge and a rationale shorter than the §2 threshold of 30.
    let decisions = read_jsonl(root, "decisions.jsonl");
    assert!(
        decisions.iter().any(|d| {
            d["decision"]["kind"] == "set_intake_method"
                && d["rationale"].as_str().map_or(0, |s| s.chars().count()) < 30
                && d.get("cites").is_none()
        }),
        "spec §2 violation (MethodChoice, no cites, rationale <30) not present in D sub-graph"
    );
}

/// Invariant 3 — `evidence_coverage` → Warn.
///
/// Spec predicate (`invariants.md` §3, verbatim):
/// ```text
/// ∀ o ∈ E.OutputFiles :
///     (∃ e ∈ V.edges : e.predicate = "computed-from" ∧ e.target = o.id)
///   ∨ (∃ b ∈ F.Blockers : b.kind = "OutputUnused" ∧ b.refs ∋ o.id)
/// ```
/// Violation injected: an `E.OutputFile` (a V `computed-from` row whose output
/// path is the bare `o.id`) referenced by NO `C.supported-by` and marked by NO
/// `F` `output_unused` blocker — falsifying both disjuncts.
fn mutate_evidence_coverage(root: &Path) {
    append_jsonl(
        root,
        "proofs.jsonl",
        &serde_json::json!({
            "edge_id": "edge_evidence_001",
            "computed_from": "data/outputs/orphan_result.tsv"
        }),
    );

    // Spec-fidelity: the V sub-graph now declares an output via a
    // `computed-from` row, and neither C nor F covers it — exactly the
    // un-referenced OutputFile the §3 predicate ranges over.
    let proofs = read_jsonl(root, "proofs.jsonl");
    let declared = proofs.iter().any(|p| {
        p.get("computed_from").or_else(|| p.get("produces")).and_then(|v| v.as_str())
            == Some("data/outputs/orphan_result.tsv")
    });
    assert!(
        declared,
        "spec §3 setup (computed-from OutputFile) not present in V sub-graph"
    );
    let claims = read_json(root, "claim-verification.json");
    let covered = claims["verdicts"].as_array().is_some_and(|vs| {
        vs.iter()
            .filter_map(|c| c["supported_by"].as_array())
            .flatten()
            .any(|r| r.as_str().map(|s| s.split('#').next().unwrap_or(s))
                == Some("data/outputs/orphan_result.tsv"))
    });
    assert!(
        !covered,
        "spec §3 violation: the new OutputFile must NOT be referenced in C"
    );
}

/// Invariant 4 — `equivalence_failure` → Fail.
///
/// Spec predicate (`invariants.md` §4, verbatim):
/// ```text
/// ∀ r ∈ Q.RerunOutcomes :
///     r.class ∉ {"failed", "non-deterministic"}
///   ∨ ∃ b ∈ F.Blockers :
///         b.kind ∈ {"UnprovableEdge", "PolicyException"}
///       ∧ b.refs ∋ r.id
/// ```
/// NOTE (spec discrepancy): the committed `invariants.md` §4 carries a STALE
/// token `"non-deterministic"`. The closed `Q.RerunOutcome` class enum is
/// `byte_identical | semantic_equivalent | acknowledged_non_determinism |
/// unavailable | failed`; `"non-deterministic"` is the obsolete spelling of
/// `acknowledged_non_determinism`. This mutator injects the `failed` class
/// (which the reference impl reads as a `prove`/`failed` verifier-decision row);
/// the second-branch mutator in Task 3 exercises `acknowledged_non_determinism`.
/// Violation injected: a `Q.RerunOutcome` of class `failed` with NO
/// `F.Blocker` of kind `UnprovableEdge`/`PolicyException` acknowledging it.
fn mutate_equivalence_failure(root: &Path) {
    append_jsonl(
        root,
        "verifier-decisions.jsonl",
        &serde_json::json!({
            "event": "prove",
            "outcome": "failed",
            "edge_id": "edge_unproven_001"
        }),
    );

    // Spec-fidelity: the Q sub-graph now holds a failed RerunOutcome and the F
    // sub-graph holds no acknowledging Blocker for it.
    let q = read_jsonl(root, "verifier-decisions.jsonl");
    assert!(
        q.iter().any(|r| r["event"] == "prove"
            && r["outcome"] == "failed"
            && r["edge_id"] == "edge_unproven_001"),
        "spec §4 violation (failed RerunOutcome) not present in Q sub-graph"
    );
    let f = read_jsonl(root, "assumptions.jsonl");
    assert!(
        !f.iter().any(|b| {
            matches!(
                b.get("kind").and_then(|k| k.as_str()),
                Some("unprovable_edge" | "policy_exception")
            )
        }),
        "spec §4 violation requires NO acknowledging F.Blocker"
    );
}

/// Invariant 5 — `cross_graph_integrity` → Fail.
///
/// Spec predicate (`invariants.md` §5, verbatim):
/// ```text
/// ∀ e ∈ ⋃_{G∈{I,D,E,V,C,Q,F,A}} G.edges :
///     cross_graph(e) ⇒
///         ∃ G' ∈ {I,D,E,V,C,Q,F,A} :
///             (e.target matches "<G'.letter>:<id>")
///           ∧ (∃ n ∈ G'.nodes : n.id = e.target_local_id)
/// ```
/// Violation injected: a `C` `supported-by` edge whose target output resolves
/// to NO node in the `V` (Evidence) sub-graph — a dangling cross-graph
/// reference falsifying the consequent.
fn mutate_cross_graph_integrity(root: &Path) {
    let mut claims = read_json(root, "claim-verification.json");
    let verdicts = claims
        .get_mut("verdicts")
        .and_then(|v| v.as_array_mut())
        .expect("verdicts array");
    verdicts.push(serde_json::json!({
        "claim_id": "claim_dangling_001",
        "status": "verified",
        "supported_by": ["data/outputs/ghost_node.tsv#row1"]
    }));
    write_json(root, "claim-verification.json", &claims);

    // Spec-fidelity: the C edge points at an output that no V `computed-from`
    // row declares — the dangling cross-graph reference §5 ranges over.
    let claims = read_json(root, "claim-verification.json");
    let dangling_ref = claims["verdicts"]
        .as_array()
        .expect("verdicts array")
        .iter()
        .filter_map(|c| c["supported_by"].as_array())
        .flatten()
        .filter_map(|r| r.as_str())
        .any(|s| s.split('#').next() == Some("data/outputs/ghost_node.tsv"));
    assert!(
        dangling_ref,
        "spec §5 setup: C supported-by edge to the ghost output must be present"
    );
    let known: std::collections::BTreeSet<String> = read_jsonl(root, "proofs.jsonl")
        .iter()
        .filter_map(|p| {
            p.get("computed_from")
                .or_else(|| p.get("produces"))
                .and_then(|v| v.as_str())
                .map(|s| s.split('#').next().unwrap_or(s).to_string())
        })
        .collect();
    assert!(
        !known.contains("data/outputs/ghost_node.tsv"),
        "spec §5 violation: the referenced V node must NOT exist (dangling)"
    );
}

// ---------------------------------------------------------------------------
// Second-branch mutators (a DISTINCT violating shape per invariant)
//
// One mutator exercises one branch of one predicate. A real invariant fails in
// several ways; a second, structurally-different violation per invariant tests
// that the check is not over-fit to a single corruption shape. Each is wired
// against the SAME flip + isolation assertions as the first branch.
// ---------------------------------------------------------------------------

/// Invariant 1 — `claim_completeness` → Warn (branch B).
/// Distinct from branch A (explicit `status:"verified"`, empty `supported_by`):
/// here the Claim has NO `status` field at all (so it is not "pending") and NO
/// `supported_by` key — a missing-fields shape rather than an empty array.
fn mutate_claim_completeness_b(root: &Path) {
    let mut claims = read_json(root, "claim-verification.json");
    let verdicts = claims
        .get_mut("verdicts")
        .and_then(|v| v.as_array_mut())
        .expect("verdicts array");
    verdicts.push(serde_json::json!({
        "claim_id": "claim_nostatus_002"
    }));
    write_json(root, "claim-verification.json", &claims);

    // Spec-fidelity: a Claim that is neither pending nor supported is present.
    let claims = read_json(root, "claim-verification.json");
    assert!(
        claims["verdicts"].as_array().expect("verdicts").iter().any(|c| {
            c["claim_id"] == "claim_nostatus_002"
                && c.get("status").and_then(|s| s.as_str()) != Some("pending")
                && c.get("supported_by").is_none()
        }),
        "branch-B spec §1 violation (Claim, no status, no supported_by) not present"
    );
}

/// Invariant 2 — `decision_justification` → Warn (branch B).
/// Distinct from branch A (`set_intake_method`): the OTHER method-choice
/// variant the predicate ranges over, `amend_stage`, with no `cites` edge and
/// `length(rationale) < 30`.
fn mutate_decision_justification_b(root: &Path) {
    append_jsonl(
        root,
        "decisions.jsonl",
        &serde_json::json!({
            "schema_version": "0.1.0",
            "timestamp": "2026-05-18T00:00:03Z",
            "session_id": "minimal-session-001",
            "decision": {
                "kind": "amend_stage",
                "stage": "quantification",
                "method_prose": "salmon"
            },
            "rationale": "swap",
            "actor": "sme"
        }),
    );

    // Spec-fidelity: an amend_stage MethodChoice with rationale <30 and no cites.
    let decisions = read_jsonl(root, "decisions.jsonl");
    assert!(
        decisions.iter().any(|d| {
            d["decision"]["kind"] == "amend_stage"
                && d["rationale"].as_str().map_or(0, |s| s.chars().count()) < 30
                && d.get("cites").is_none()
        }),
        "branch-B spec §2 violation (amend_stage MethodChoice, rationale <30) not present"
    );
}

/// Invariant 3 — `evidence_coverage` → Warn (branch B).
/// Distinct from branch A (`computed_from` field): the same uncovered-output
/// predicate reached via the V row's `produces` field (the impl's
/// `computed_from`-OR-`produces` fallback), an output referenced by no C edge.
fn mutate_evidence_coverage_b(root: &Path) {
    append_jsonl(
        root,
        "proofs.jsonl",
        &serde_json::json!({
            "edge_id": "edge_evidence_002",
            "produces": "data/outputs/orphan_via_produces.tsv"
        }),
    );

    // Spec-fidelity: a `produces` OutputFile is declared and uncovered in C.
    let proofs = read_jsonl(root, "proofs.jsonl");
    assert!(
        proofs.iter().any(|p| p.get("produces").and_then(|v| v.as_str())
            == Some("data/outputs/orphan_via_produces.tsv")),
        "branch-B spec §3 setup (produces OutputFile) not present in V sub-graph"
    );
}

/// Invariant 4 — `equivalence_failure` → Fail (branch B).
/// Distinct from branch A (a `failed` RerunOutcome with NO acknowledgement):
/// here a `failed` RerunOutcome carries an `F.Blocker` of the WRONG kind
/// (`output_unused`, which is NOT in the `{UnprovableEdge, PolicyException}`
/// ack set), so the predicate's existential is still unsatisfied → Fail.
///
/// SPEC DISCREPANCY / IMPL GAP: the §4 predicate also names the
/// `acknowledged_non_determinism` outcome class (committed `invariants.md`
/// spells it with the stale token `"non-deterministic"`). The reference impl
/// only reads `outcome == "failed"` verifier-decision rows and does NOT read a
/// `class: acknowledged_non_determinism` field, so that second outcome class
/// cannot be exercised hermetically against the current implementation. We
/// therefore drive branch B via a `failed` row with a mis-kinded ack, and
/// record the unreachable-class gap here rather than asserting a flip the impl
/// cannot produce.
fn mutate_equivalence_failure_b(root: &Path) {
    append_jsonl(
        root,
        "verifier-decisions.jsonl",
        &serde_json::json!({
            "event": "prove",
            "outcome": "failed",
            "edge_id": "edge_miskinded_002"
        }),
    );
    append_jsonl(
        root,
        "assumptions.jsonl",
        &serde_json::json!({
            "assumption_id": "assume_002",
            "kind": "output_unused",
            "detail": "edge_miskinded_002",
            "stage_id": "x"
        }),
    );

    // Spec-fidelity: a failed RerunOutcome exists AND its only F.Blocker is of a
    // kind NOT in the acknowledging set, so the §4 existential is unsatisfied.
    let q = read_jsonl(root, "verifier-decisions.jsonl");
    assert!(
        q.iter().any(|r| r["outcome"] == "failed" && r["edge_id"] == "edge_miskinded_002"),
        "branch-B spec §4 setup (failed RerunOutcome) not present in Q sub-graph"
    );
    let acked = read_jsonl(root, "assumptions.jsonl").iter().any(|b| {
        matches!(
            b.get("kind").and_then(|k| k.as_str()),
            Some("unprovable_edge" | "policy_exception")
        ) && b.get("detail").and_then(|d| d.as_str()) == Some("edge_miskinded_002")
    });
    assert!(
        !acked,
        "branch-B spec §4 violation requires the ack to be of a NON-acknowledging kind"
    );
}

/// Invariant 5 — `cross_graph_integrity` → Fail (branch B).
/// Distinct from branch A (a dangling `C.supported-by` → `V`): here an
/// `F.Blocker` (assumption) carries an `edge_id` that resolves to NO `V`
/// `edge_id` — a different cross-graph reference (F→V) that dangles. (The
/// reference impl reads cross-graph references from claim `supported_by` and
/// assumption `edge_id`; `decisions.jsonl prov:wasDerivedFrom` is not yet read,
/// so the assumption-edge_id path is the genuine second dangling shape.)
fn mutate_cross_graph_integrity_b(root: &Path) {
    append_jsonl(
        root,
        "assumptions.jsonl",
        &serde_json::json!({
            "assumption_id": "assume_dangling_002",
            "kind": "note",
            "detail": "references a non-existent proof edge",
            "edge_id": "edge_ghost_999"
        }),
    );

    // Spec-fidelity: an F edge_id reference with no matching V edge_id.
    let known_edges: std::collections::BTreeSet<String> = read_jsonl(root, "proofs.jsonl")
        .iter()
        .filter_map(|p| p.get("edge_id").and_then(|s| s.as_str()).map(String::from))
        .collect();
    let dangling = read_jsonl(root, "assumptions.jsonl").iter().any(|a| {
        a.get("edge_id").and_then(|s| s.as_str()) == Some("edge_ghost_999")
    });
    assert!(
        dangling && !known_edges.contains("edge_ghost_999"),
        "branch-B spec §5 violation (F edge_id with no matching V edge) not present"
    );
}

/// Invariant 6 — `substrate_validity` → Fail (gated; runcrate-only).
///
/// Spec predicate (`invariants.md` §6, verbatim — the conjunct this mutator
/// falsifies):
/// ```text
/// package.passes(`runcrate validate ≥ 0.5.0`)
///   ∧ |{iri ∈ package.conformsTo : iri ∈ REQUIRED_PROFILE_IRIS}| = 6
///   ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "wfprov:ParameterConnection"
///   ∧ ∃ entity ∈ package.@graph : entity.@type ∋ "p-plan:Plan"
///   ∧ ∀ sidecar ∈ REQUIRED_SIDECARS : sidecar ∈ package.@graph as CreativeWork
/// ```
/// Violation injected: drop one required `conformsTo` profile IRI so the
/// `|{…}| = 6` conjunct no longer holds (the §3 `REQUIRED_PROFILE_IRIS` set is
/// undersatisfied). Under `NoopWrrocValidator` this is inert (always
/// `Unverified`); the row is gated behind `runcrate`. See SUBSTRATE CAVEAT.
fn mutate_substrate_validity(root: &Path) {
    let path = root.join("ro-crate-metadata.json");
    let raw = std::fs::read_to_string(&path).expect("read descriptor");
    let mut meta: Value = serde_json::from_str(&raw).expect("parse descriptor");
    let graph = meta
        .get_mut("@graph")
        .and_then(|g| g.as_array_mut())
        .expect("@graph array");
    for entry in graph.iter_mut() {
        if entry.get("@id").and_then(|v| v.as_str()) == Some("ro-crate-metadata.json") {
            if let Some(conforms) = entry.get_mut("conformsTo").and_then(|c| c.as_array_mut()) {
                if !conforms.is_empty() {
                    conforms.remove(0); // drop one required profile IRI
                }
            }
        }
    }
    std::fs::write(&path, serde_json::to_string_pretty(&meta).expect("ser")).expect("write");
}

// ---------------------------------------------------------------------------
// Boundary (negative-control) mutators
//
// The most valuable cases: each applies a near-miss that MUST stay `Pass` — it
// proves the invariant does NOT over-fire on a legitimately-justified package.
// These are calibration probes (e.g. rationale of EXACTLY 30 chars, a claim
// explicitly marked "pending"). Asserted Pass-stays-Pass against the complete
// fixture.
// ---------------------------------------------------------------------------

/// Boundary — `decision_justification` MUST stay `Pass`.
/// A `set_intake_method` MethodChoice whose `rationale` is EXACTLY 30 chars:
/// the §2 threshold is `length ≥ 30`, so 30 satisfies it (off-by-one guard).
fn boundary_rationale_exactly_30(root: &Path) {
    let rationale = "012345678901234567890123456789"; // exactly 30 chars
    assert_eq!(rationale.chars().count(), 30, "boundary fixture must be 30 chars");
    append_jsonl(
        root,
        "decisions.jsonl",
        &serde_json::json!({
            "schema_version": "0.1.0",
            "timestamp": "2026-05-18T00:00:02Z",
            "session_id": "minimal-session-001",
            "decision": {"kind": "set_intake_method", "stage": "x", "method_prose": "y"},
            "rationale": rationale,
            "actor": "sme"
        }),
    );
}

/// Boundary — `claim_completeness` MUST stay `Pass`.
/// A Claim explicitly `status:"pending"` with empty `supported_by`: §1 carves
/// pending out as a legitimate acknowledged state, so this must NOT warn.
fn boundary_pending_claim(root: &Path) {
    let mut claims = read_json(root, "claim-verification.json");
    let verdicts = claims
        .get_mut("verdicts")
        .and_then(|v| v.as_array_mut())
        .expect("verdicts array");
    verdicts.push(serde_json::json!({
        "claim_id": "claim_pending_001",
        "status": "pending",
        "supported_by": []
    }));
    write_json(root, "claim-verification.json", &claims);
}

// ---------------------------------------------------------------------------
// The matrix test
// ---------------------------------------------------------------------------

fn runcrate_available() -> bool {
    std::process::Command::new("runcrate")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn invariant_utility_specificity_matrix() {
    // 0. Clean baseline.
    let (_g0, clean_root) = fresh_package();
    let baseline = run(&clean_root);

    // Print the header + clean baseline row.
    println!("\n================ INVARIANT-UTILITY DETECTION MATRIX ================");
    println!(
        "{:<26} | {:^6} {:^6} {:^6} {:^6} {:^6} {:^6}",
        "injected fault", "CLAIM", "DECISN", "EVIDNC", "EQUIV", "XGRAPH", "SUBSTR"
    );
    println!("{}", "-".repeat(80));
    let row = |label: &str, r: &AuditProofReport| {
        println!(
            "{:<26} | {:^6} {:^6} {:^6} {:^6} {:^6} {:^6}",
            label,
            status_glyph(verdict(r, InvariantId::ClaimCompleteness).status),
            status_glyph(verdict(r, InvariantId::DecisionJustification).status),
            status_glyph(verdict(r, InvariantId::EvidenceCoverage).status),
            status_glyph(verdict(r, InvariantId::EquivalenceFailure).status),
            status_glyph(verdict(r, InvariantId::CrossGraphIntegrity).status),
            status_glyph(verdict(r, InvariantId::SubstrateValidity).status),
        );
    };
    row("(clean baseline)", &baseline);

    // The complete fixture (spec Appendix C) is the non-degenerate baseline:
    // the five hermetic invariants genuinely `Pass`, so every injection below
    // is a real `Pass → Warn/Fail` flip rather than `Unverified → Warn`.
    // Substrate is `Unverified` under Noop (gated; see SUBSTRATE CAVEAT).
    for id in [
        InvariantId::ClaimCompleteness,
        InvariantId::DecisionJustification,
        InvariantId::EvidenceCoverage,
        InvariantId::EquivalenceFailure,
        InvariantId::CrossGraphIntegrity,
    ] {
        assert_eq!(
            verdict(&baseline, id).status,
            InvariantStatus::Pass,
            "baseline must Pass {id:?} for flips to be genuine (detail: {:?})",
            verdict(&baseline, id).detail
        );
    }

    // Each row: (label, target invariant, mutator, expected non-Pass status).
    struct Case {
        label: &'static str,
        target: InvariantId,
        mutate: fn(&Path),
        expect: InvariantStatus,
    }
    let cases = [
        Case {
            label: "claim_completeness",
            target: InvariantId::ClaimCompleteness,
            mutate: mutate_claim_completeness,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "decision_justification",
            target: InvariantId::DecisionJustification,
            mutate: mutate_decision_justification,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "evidence_coverage",
            target: InvariantId::EvidenceCoverage,
            mutate: mutate_evidence_coverage,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "equivalence_failure",
            target: InvariantId::EquivalenceFailure,
            mutate: mutate_equivalence_failure,
            expect: InvariantStatus::Fail,
        },
        Case {
            label: "cross_graph_integrity",
            target: InvariantId::CrossGraphIntegrity,
            mutate: mutate_cross_graph_integrity,
            expect: InvariantStatus::Fail,
        },
        // Second-branch violations: a structurally-distinct corruption per
        // invariant (different field/shape than branch A) that must still flip
        // the target while leaving the other five unchanged.
        Case {
            label: "claim_completeness #B",
            target: InvariantId::ClaimCompleteness,
            mutate: mutate_claim_completeness_b,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "decision_justification #B",
            target: InvariantId::DecisionJustification,
            mutate: mutate_decision_justification_b,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "evidence_coverage #B",
            target: InvariantId::EvidenceCoverage,
            mutate: mutate_evidence_coverage_b,
            expect: InvariantStatus::Warn,
        },
        Case {
            label: "equivalence_failure #B",
            target: InvariantId::EquivalenceFailure,
            mutate: mutate_equivalence_failure_b,
            expect: InvariantStatus::Fail,
        },
        Case {
            label: "cross_graph_integrity #B",
            target: InvariantId::CrossGraphIntegrity,
            mutate: mutate_cross_graph_integrity_b,
            expect: InvariantStatus::Fail,
        },
    ];

    for case in &cases {
        let (_g, root) = fresh_package();
        (case.mutate)(&root);
        let mutated = run(&root);
        row(case.label, &mutated);

        // (a) target invariant flips to the predicted non-Pass status.
        let got = verdict(&mutated, case.target).status;
        assert_eq!(
            got, case.expect,
            "TARGET {:?}: expected {:?} after its own injection, got {:?} (detail: {:?})",
            case.target,
            case.expect,
            got,
            verdict(&mutated, case.target).detail
        );
        // The flip must be a genuine change away from the clean baseline.
        assert_ne!(
            verdict(&baseline, case.target).status,
            got,
            "{:?} was already {:?} in the clean baseline — injection proves nothing",
            case.target,
            got
        );

        // (b) the other five invariants are unchanged (specificity/isolation).
        assert_others_unchanged(&baseline, &mutated, case.target);
    }

    // Boundary (negative-control) rows: a near-miss per invariant that MUST
    // stay `Pass` — proof the check does not over-fire. The target invariant's
    // verdict must remain exactly `Pass` (not Warn/Fail), and isolation must
    // hold (no OTHER invariant perturbed either).
    println!("{}", "-".repeat(80));
    struct BoundaryCase {
        label: &'static str,
        target: InvariantId,
        mutate: fn(&Path),
    }
    let boundaries = [
        BoundaryCase {
            label: "decision rationale==30",
            target: InvariantId::DecisionJustification,
            mutate: boundary_rationale_exactly_30,
        },
        BoundaryCase {
            label: "claim pending,no support",
            target: InvariantId::ClaimCompleteness,
            mutate: boundary_pending_claim,
        },
    ];
    for bc in &boundaries {
        let (_g, root) = fresh_package();
        (bc.mutate)(&root);
        let probed = run(&root);
        row(bc.label, &probed);
        let got = verdict(&probed, bc.target).status;
        assert_eq!(
            got,
            InvariantStatus::Pass,
            "BOUNDARY {:?}: a legitimate near-miss must stay Pass, got {:?} (detail: {:?})",
            bc.target,
            got,
            verdict(&probed, bc.target).detail
        );
        // The boundary perturbs nothing — every invariant matches the baseline.
        for id in InvariantId::ALL {
            assert_eq!(
                verdict(&baseline, id).status,
                verdict(&probed, id).status,
                "BOUNDARY {} perturbed {id:?} (baseline={:?}, probed={:?})",
                bc.label,
                verdict(&baseline, id).status,
                verdict(&probed, id).status
            );
        }
    }

    // Invariant 6 — substrate_validity. Gated; see SUBSTRATE CAVEAT.
    let conformance_mode = std::env::var("ECAA_CONFORMANCE_MODE")
        .map(|v| v != "0" && !v.is_empty())
        .unwrap_or(false);
    if conformance_mode && runcrate_available() {
        let (_g, root) = fresh_package();
        mutate_substrate_validity(&root);
        // Drive the REAL runcrate-backed validator for this row only.
        let report = run_audit_proof(&root, &PythonRuncrateWrrocValidator, &WallClock)
            .expect("run_audit_proof (runcrate)");
        row("substrate_validity", &report);
        let got = verdict(&report, InvariantId::SubstrateValidity).status;
        assert_eq!(
            got,
            InvariantStatus::Fail,
            "substrate_validity: dropping a required conformsTo IRI must Fail under \
             runcrate (detail: {:?})",
            verdict(&report, InvariantId::SubstrateValidity).detail
        );
        println!(
            "  substrate_validity row: EXERCISED via PythonRuncrateWrrocValidator (ECAA_CONFORMANCE_MODE set, runcrate present)"
        );
    } else {
        println!(
            "{:<26} | {:^6} {:^6} {:^6} {:^6} {:^6} {:^6}",
            "substrate_validity", "-", "-", "-", "-", "-", "SKIP"
        );
        println!(
            "  substrate_validity row: SKIPPED — requires runcrate; not exercised \
             hermetically (NoopWrrocValidator returns Unverified regardless). \
             Set ECAA_CONFORMANCE_MODE=1 with runcrate on PATH to exercise. \
             [conformance_mode={conformance_mode}, runcrate_available={}]",
            runcrate_available()
        );
    }

    println!("{}", "=".repeat(67));
}
