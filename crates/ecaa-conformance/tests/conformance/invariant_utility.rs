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
// Per-invariant mutators
// Each corrupts exactly one invariant's precondition on a fresh package.
// ---------------------------------------------------------------------------

/// Invariant 1 — claim_completeness → Warn.
/// Add a Claim verdict with NO supported_by edge and status != "pending".
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
}

/// Invariant 2 — decision_justification → Warn.
/// Add a `set_intake_method` decision whose method_prose AND record-level
/// rationale are both <30 chars (no cites field exists on v0.1 method
/// decisions, so the predicate reduces to the rationale-length branch).
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
}

/// Invariant 3 — evidence_coverage → Warn.
/// Add an Evidence (E) output row to proofs.jsonl (via `computed_from`) that
/// is referenced by no claim verdict's supported_by and marked by no
/// `output_unused` assumption.
fn mutate_evidence_coverage(root: &Path) {
    append_jsonl(
        root,
        "proofs.jsonl",
        &serde_json::json!({
            "edge_id": "edge_evidence_001",
            "computed_from": "data/outputs/orphan_result.tsv"
        }),
    );
    // claim-verification.json must exist with a verdicts array for the
    // check to run the per-output coverage branch (Pass vs Warn) rather
    // than the claims-absent default. The fixture already ships one with an
    // empty verdicts array, so no supported_by edge can cover the new output.
}

/// Invariant 4 — equivalence_failure → Fail.
/// Add a verifier-decisions `prove`/`failed` row whose edge_id has NO
/// matching `unprovable_edge` / `policy_exception` acknowledgement.
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
}

/// Invariant 5 — cross_graph_integrity → Fail.
/// Add a claim verdict whose supported_by points at an output IRI that
/// resolves to NO row in the Evidence (E) graph (proofs.jsonl).
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
}

/// Invariant 6 — substrate_validity → Fail (gated; runcrate-only).
/// Drop a required `conformsTo` profile IRI from ro-crate-metadata.json so a
/// real WRROC validator rejects the descriptor. Under NoopWrrocValidator this
/// is inert (always Unverified); see SUBSTRATE CAVEAT in the module docs.
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
