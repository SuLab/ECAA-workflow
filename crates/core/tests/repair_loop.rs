//! End-to-end + anti-gaming integration coverage for the repair loop.
//!
//! These tests construct a realistic fixture package on disk and drive the
//! *real* executor registry ([`driver::default_registry`]) through the loop's
//! state machine ([`driver::run_loop`]) with an injected `assess` closure. The
//! injected closure lets us seed a deterministic, hermetic failure sequence
//! whose detail strings match the production `claim_verifier::compare_count`
//! mismatch shape, so the genuine [`executors::narrative::NarrativeCorrection`]
//! executor runs against genuine files — exercising the executor end-to-end
//! without the heavyweight, non-hermetic `finalize_package` + `audit_proof`
//! pipeline that `assess_package` would otherwise run.
//!
//! Anti-gaming invariant under test: the narrative prose is corrected toward the
//! frozen table value, while the frozen table itself is left byte-for-byte
//! identical. The corrector NEVER edits the source of truth to silence a claim.

use std::cell::Cell;
use std::path::{Path, PathBuf};

use ecaa_workflow_core::repair_loop::driver::{default_registry, run_loop};
use ecaa_workflow_core::repair_loop::failure::{
    Failure, FailureSet, FailureSource, FailureStatus, RepairClass, GLOBAL_ROUND_CAP,
};
use ecaa_workflow_core::repair_loop::runner::{RepairDirective, ReviewRoutingRunner, TaskRunner};
use ecaa_workflow_core::repair_loop::status::RepairVerdict;

/// The single completed task in the fixture package.
const TASK: &str = "reporting";

/// A `compare_count` mismatch detail in the EXACT shape emitted by
/// `claim_verifier::compare_count`: the narrative claims 9, the frozen table has
/// 3. The narrative corrector parses N=9, M=3 from this string and rewrites the
/// prose toward 3 — it never reads the table here, only the detail.
fn count_mismatch_detail() -> String {
    "count claim: narrative says 9, `pw.tsv` has 3 (gene sets significant at padj < 0.05)"
        .to_string()
}

/// A runner that surfaces agentic needs by routing them to review (the offline
/// default). Used wherever a runner is required but no agent is available.
fn review_runner() -> ReviewRoutingRunner {
    ReviewRoutingRunner
}

/// Materialize a realistic fixture package under `root`:
/// - `WORKFLOW.json` with one completed task (`reporting`),
/// - `policies/interpretation-policy.json` copied verbatim from the real
///   downstream default so any policy-driven config load would succeed,
/// - `runtime/outputs/reporting/report.md` whose count claim DISAGREES with the
///   table ("9 gene sets ... (pw.tsv)" while the table has 3 significant rows),
/// - `runtime/outputs/reporting/pw.tsv` a frozen 3-row significant table,
/// - `runtime/outputs/reporting/result.json` carrying the structured claim.
///
/// Returns `(config_dir, report_path, table_path)`.
fn build_fixture(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    // WORKFLOW.json — one completed task. (Non-confirmatory stems so the assess
    // path, if ever exercised, treats this as a routine reporting package.)
    std::fs::write(
        root.join("WORKFLOW.json"),
        r#"{
  "tasks": {
    "reporting": {
      "status": "completed",
      "source_atom_id": "report_interpretation"
    }
  }
}"#,
    )
    .expect("write WORKFLOW.json");

    // policies/interpretation-policy.json — copy the real downstream default so
    // ExtractorConfig::from_policy (verifiableEntities-enabled) would succeed.
    let config_dir = root.join("policies");
    std::fs::create_dir_all(&config_dir).expect("create policies dir");
    let policy_src = repo_root()
        .join("config")
        .join("downstream-policy")
        .join("interpretation-policy.json");
    let policy_bytes = std::fs::read(&policy_src).unwrap_or_else(|e| {
        panic!(
            "the real default interpretation policy must exist at {}: {e}",
            policy_src.display()
        )
    });
    std::fs::write(
        config_dir.join("interpretation-policy.json"),
        &policy_bytes,
    )
    .expect("write interpretation-policy.json");

    // runtime/outputs/reporting/* — narrative, frozen table, structured claim.
    let task_dir = root.join("runtime").join("outputs").join(TASK);
    std::fs::create_dir_all(&task_dir).expect("create task output dir");

    let report = task_dir.join("report.md");
    std::fs::write(
        &report,
        "# Enrichment report\n\n\
         9 gene sets were significantly enriched (padj < 0.05) (pw.tsv).\n\n\
         The strongest signal was GO:0006954 (inflammatory response).\n",
    )
    .expect("write report.md");

    // Frozen 3-row significant table: exactly 3 rows pass padj < 0.05.
    let table = task_dir.join("pw.tsv");
    std::fs::write(
        &table,
        "gene_set_id\tpadj\nGO:0006954\t0.001\nGO:0019221\t0.012\nGO:0071345\t0.044\n",
    )
    .expect("write pw.tsv");

    // result.json — structured claim mirroring the prose count.
    std::fs::write(
        task_dir.join("result.json"),
        r#"{
  "claims": [
    {
      "kind": "count",
      "subject": "gene_sets_significant",
      "value": 9,
      "source_table": "pw.tsv",
      "threshold": "padj < 0.05"
    }
  ]
}"#,
    )
    .expect("write result.json");

    (config_dir, report, table)
}

/// Locate the repository root (the `ecaa-workflow` checkout) from this test's
/// crate dir: `crates/core` -> repo root is two levels up.
fn repo_root() -> PathBuf {
    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    crate_dir
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .unwrap_or(crate_dir)
}

/// Build the realistic Open NarrativeCorrection failure for the fixture's
/// reporting task, with a `compare_count`-shaped detail.
fn narrative_failure(status: FailureStatus) -> Failure {
    let mut f = Failure::new(
        FailureSource::ClaimMismatch,
        RepairClass::NarrativeCorrection,
        TASK,
        "gene_sets_significant",
        &count_mismatch_detail(),
    );
    f.status = status;
    f
}

/// (a) End-to-end: the real NarrativeCorrection executor corrects the prose
/// count toward the frozen table value, leaves the table byte-identical
/// (anti-gaming), and a `runtime/repair-status.json` is written.
#[test]
fn end_to_end_corrects_narrative_and_freezes_table() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let (config_dir, report, table) = build_fixture(root);

    let table_before = std::fs::read(&table).expect("read frozen table before");
    let report_before = std::fs::read_to_string(&report).expect("read narrative before");
    assert!(
        report_before.contains("9 gene sets"),
        "fixture narrative must start with the stale count, got: {report_before}"
    );

    // Injected assess: round 0 yields the Open narrative failure; once the
    // executor has applied (round >= 1) the same failure assesses Resolved.
    // The real executor mutates report.md on round 0; the table is never an
    // executor target, so it must remain frozen throughout.
    let calls = Cell::new(0usize);
    let mut assess = || {
        let n = calls.get();
        calls.set(n + 1);
        if n == 0 {
            FailureSet(vec![narrative_failure(FailureStatus::Open)])
        } else {
            FailureSet(vec![narrative_failure(FailureStatus::Resolved)])
        }
    };

    let registry = default_registry();
    let runner = review_runner();
    let status = run_loop(root, &config_dir, &mut assess, &registry, &runner);

    // The prose count must now agree with the table (9 -> 3); the stale 9 gone.
    let report_after = std::fs::read_to_string(&report).expect("read narrative after");
    assert!(
        report_after.contains("3 gene sets were significantly enriched"),
        "narrative count must be corrected toward the table value (3), got: {report_after}"
    );
    assert!(
        !report_after.contains("9 gene sets"),
        "the stale claimed count (9) must be gone, got: {report_after}"
    );

    // Anti-gaming: the frozen result table is byte-for-byte unchanged.
    let table_after = std::fs::read(&table).expect("read frozen table after");
    assert_eq!(
        table_before, table_after,
        "the frozen result table must be byte-for-byte identical (anti-gaming): \
         the corrector edits prose, never the source-of-truth table"
    );

    // The repair status must have been persisted to the canonical path.
    let status_path = root.join("runtime").join("repair-status.json");
    assert!(
        status_path.is_file(),
        "run_loop must persist runtime/repair-status.json, missing at {}",
        status_path.display()
    );
    let persisted = std::fs::read_to_string(&status_path).expect("read repair-status.json");
    assert!(
        persisted.contains("\"verdict\""),
        "persisted status must carry a verdict field, got: {persisted}"
    );

    // With the only failure resolved after correction, the verdict is a full pass.
    assert_eq!(
        status.verdict,
        RepairVerdict::FullyPassing,
        "a single resolved narrative correction must converge FullyPassing, got {status:?}"
    );
    assert!(
        status.review.is_empty(),
        "no review items expected on a full pass, got {:?}",
        status.review
    );
    assert!(
        status.rounds >= 1,
        "at least one repair round must have executed, got {}",
        status.rounds
    );
}

/// (b) ReviewRequired-class situation: a failure with no automated repair path
/// (budget 0, no executor) must converge MostlyPassing with that exact item on
/// the review list — never silently dropped or auto-"fixed".
#[test]
fn review_required_situation_routes_to_review_list() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let (config_dir, report, table) = build_fixture(root);
    let table_before = std::fs::read(&table).expect("read table before");
    let report_before = std::fs::read_to_string(&report).expect("read report before");

    // A ReviewRequired failure is always Open across rounds; ReviewRequired has
    // budget 0, so it is never eligible for repair and is routed straight to
    // review. (No executor is registered for it in default_registry either.)
    let mut assess = || {
        let mut f = Failure::new(
            FailureSource::InvariantFailure("decision_justification".to_string()),
            RepairClass::ReviewRequired,
            "audit",
            "decision_justification",
            "a decision lacks a justification an automated pass cannot supply",
        );
        f.status = FailureStatus::Open;
        FailureSet(vec![f])
    };

    let registry = default_registry();
    let runner = review_runner();
    let status = run_loop(root, &config_dir, &mut assess, &registry, &runner);

    assert_eq!(
        status.verdict,
        RepairVerdict::MostlyPassing,
        "a single review-required failure must converge MostlyPassing, got {status:?}"
    );
    assert_eq!(
        status.review.len(),
        1,
        "exactly one item must be on the review list, got {:?}",
        status.review
    );
    let item = &status.review[0];
    assert_eq!(
        item.failure.class,
        RepairClass::ReviewRequired,
        "the review item must be the ReviewRequired failure, got {:?}",
        item.failure
    );
    assert_eq!(
        item.failure.subject, "decision_justification",
        "the review item must carry the original subject, got {:?}",
        item.failure
    );
    assert!(
        status.rounds <= GLOBAL_ROUND_CAP,
        "the loop must terminate within the global round cap, ran {}",
        status.rounds
    );

    // Faithful twin / anti-gaming: a review-only situation must touch NOTHING on
    // the writable surface — neither the narrative nor the frozen table.
    let table_after = std::fs::read(&table).expect("read table after");
    assert_eq!(
        table_before, table_after,
        "review-routing must not edit the frozen table"
    );
    let report_after = std::fs::read_to_string(&report).expect("read report after");
    assert_eq!(
        report_before, report_after,
        "review-routing must not edit the narrative"
    );
}

/// Drive a genuine `NeedsAgent` directive through the production registry's
/// agentic executor and confirm the offline `ReviewRoutingRunner` surfaces it
/// as a JSONL repair request — the wiring between executor, driver, and runner.
#[test]
fn agentic_need_is_surfaced_by_review_routing_runner() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path();
    let (config_dir, _report, _table) = build_fixture(root);

    // A CitationFix failure: default_registry wires an agentic executor for it,
    // which yields NeedsAgent. The runner routes the directive to review.
    let mut assess = || {
        let mut f = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            TASK,
            "see [9]",
            "citation [9] is not resolvable in the bibliography",
        );
        f.status = FailureStatus::Open;
        FailureSet(vec![f])
    };

    let registry = default_registry();
    let runner = ReviewRoutingRunner;
    let status = run_loop(root, &config_dir, &mut assess, &registry, &runner);

    // The unresolving citation must terminate as MostlyPassing on the review list.
    assert_eq!(
        status.verdict,
        RepairVerdict::MostlyPassing,
        "an unresolving agentic failure must converge MostlyPassing, got {status:?}"
    );

    // The agentic need must have been surfaced as a repair request line.
    let requests = root.join("runtime").join("repair-requests.jsonl");
    assert!(
        requests.is_file(),
        "the agentic directive must be surfaced to runtime/repair-requests.jsonl, \
         missing at {}",
        requests.display()
    );
    let body = std::fs::read_to_string(&requests).expect("read repair-requests.jsonl");
    let first = body.lines().next().expect("at least one routed directive");
    let directive: RepairDirective =
        serde_json::from_str(first).expect("routed directive must be valid JSONL");
    assert_eq!(
        directive.task, TASK,
        "the routed directive must carry the failing task, got {directive:?}"
    );
}

/// Tiny compile-time guard that the offline runner satisfies `TaskRunner` and is
/// usable through the trait object the driver consumes — keeps the public wiring
/// honest if signatures drift.
#[test]
fn review_routing_runner_is_a_task_runner() {
    let runner: &dyn TaskRunner = &ReviewRoutingRunner;
    let tmp = tempfile::tempdir().expect("tempdir");
    let directive = RepairDirective {
        task: TASK.to_string(),
        instruction: "surface for review".to_string(),
    };
    runner
        .rerun(tmp.path(), &directive)
        .expect("offline runner must accept a directive");
}
