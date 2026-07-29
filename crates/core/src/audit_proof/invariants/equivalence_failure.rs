//! Invariant 4: equivalence-failure.
//! Every re-execution divergence must be acknowledged. Per spec §4, the
//! predicate ranges over `Q.RerunOutcomes`: any outcome whose `class` is
//! `failed` or `acknowledged_non_determinism` requires an acknowledgment from
//! EITHER an `assumptions.jsonl` F Blocker (`unprovable_edge` /
//! `policy_exception`, the historical source) OR — for an
//! `acknowledged_non_determinism` outcome — a matching `NonDetAck` in
//! `runtime/determinism-shim.json::non_deterministic_artifacts`. The shim ack
//! is the SAME declaration the re-execution comparator consulted when it
//! assigned the `acknowledged_non_determinism` bucket, so the comparator and
//! this invariant are unified on one source. A `failed` outcome is NOT
//! satisfiable by a bare shim match (the comparator emits `failed` when the ack
//! did not cover the divergence), so it still requires an F Blocker.
//!
//! The five-class `RerunOutcome` typing
//! (`byte_identical` / `semantic_equivalent` / `acknowledged_non_determinism` /
//! `unavailable` / `failed`) is materialized by the harness re-execution
//! classifier into `runtime/reexecution.json` and surfaced as
//! [`LoadedPackage::reexecution`]. Each `per_artifact[]` row carries the class
//! in its `bucket` field and the outcome id in `artifact_path`. This invariant
//! ranges over that file — NOT over `verifier-decisions.jsonl`, which carries
//! only the compile-time port-unification trace.
//!
//! Two silent-corruption shapes flip the verdict to `Fail`:
//!   (a) a diverged `RerunOutcome` (`bucket ∈ {failed, acknowledged_non_determinism}`)
//!       from `reexecution.json` with no acknowledging F.Blocker, and
//!   (b) an unacknowledged compile-time `prove`/`failed` port-unification row
//!       from `verifier-decisions.jsonl`.
//!
//! A third shape is NOT a `Fail` but must never read as a `Pass`: a
//! re-execution in which nothing was comparable. `unavailable` rows carry no
//! equivalence information at all (the comparator could not find the artifact on
//! one side), and they require no acknowledgement — so a run whose rows are ALL
//! `unavailable` has zero divergences and would otherwise land on the same
//! `Pass`, with the same `n_violations: 0`, as a flawless replay. A real deposit
//! hit exactly that: 3 `byte_identical` + 1 `semantic_equivalent` +
//! 23 `unavailable` reported a bare `pass` with no numbers attached. The verdict
//! therefore requires at least one artifact to have actually been put through
//! the comparator, and [`RerunTally`] is reported in the detail so "everything
//! reproduced" and "almost nothing was comparable" are never the same string.
//! The two shapes are additionally separated by `n_inspected`, which counts the
//! predicate's ∀-domain (see [`check_equivalence_failure`]) and is therefore 0
//! for the nothing-comparable run and non-zero for the flawless one.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use crate::determinism_shim::{ack_for, DeterminismShimSidecar};
use serde_json::Value;
use std::collections::BTreeSet;

/// `Q.RerunOutcome.class` values that count as a divergence requiring
/// acknowledgement. These are the two the spec §4 predicate names, drawn from
/// the closed 5-class enum in spec §5.6. (`byte_identical`,
/// `semantic_equivalent` and `unavailable` are non-divergent and need no ack.)
const DIVERGED_CLASSES: [&str; 2] = ["failed", "acknowledged_non_determinism"];

/// `Q.RerunOutcome.class` values certifying the artifact was compared against
/// the parent AND reproduced. This — not the row count — is what makes a `Pass`
/// mean "the analysis reproduced".
const REPRODUCED_CLASSES: [&str; 2] = ["byte_identical", "semantic_equivalent"];

/// The `Q.RerunOutcome.class` the comparator assigns when it could not compare
/// an artifact at all (absent on one side). Carries no equivalence information.
const UNAVAILABLE_CLASS: &str = "unavailable";

/// Census of the `reexecution.json::per_artifact[]` rows by outcome class.
///
/// The census and the acknowledgement scan answer DIFFERENT questions:
/// `n_diverged` counts the divergence CANDIDATES that needed an
/// acknowledgement, `n_compared` counts the artifacts that were compared and
/// reproduced. Reporting only the former makes a `0` ambiguous between "nothing
/// diverged" and "nothing was compared", which is why the verdict's
/// `n_inspected` sums both (see [`check_equivalence_failure`]) and the detail
/// carries the full census.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RerunTally {
    /// Rows the comparator compared and found equivalent
    /// (`byte_identical` + `semantic_equivalent`).
    pub n_compared: usize,
    /// Rows that diverged and therefore require acknowledgement
    /// (`failed` + `acknowledged_non_determinism`).
    pub n_diverged: usize,
    /// Rows the comparator could not compare (`unavailable`).
    pub n_unavailable: usize,
    /// Rows carrying no `bucket`/`class`, or one outside the closed 5-class
    /// enum. Not evidence of anything; counted so they cannot be mistaken for
    /// a comparison that happened.
    pub n_unclassified: usize,
}

impl RerunTally {
    /// Total `per_artifact[]` rows the tally ranged over.
    pub fn n_rows(&self) -> usize {
        self.n_compared + self.n_diverged + self.n_unavailable + self.n_unclassified
    }

    /// True when at least one artifact actually went through the comparator —
    /// either reproducing or diverging. False means the re-execution produced
    /// no equivalence evidence whatsoever, however many rows it wrote.
    pub fn any_comparison(&self) -> bool {
        self.n_compared > 0 || self.n_diverged > 0
    }
}

/// Census the `per_artifact[]` rows by outcome class. `bucket` is the canonical
/// `ReexecutionBucket` field; `class` is accepted as a forward-compatible alias
/// for hand-authored rows (the same aliasing the ack scan below uses).
pub fn tally_rerun_outcomes(outcomes: &[Value]) -> RerunTally {
    let mut t = RerunTally::default();
    for o in outcomes {
        let class = o
            .get("bucket")
            .and_then(Value::as_str)
            .or_else(|| o.get("class").and_then(Value::as_str));
        match class {
            Some(c) if REPRODUCED_CLASSES.contains(&c) => t.n_compared += 1,
            Some(c) if DIVERGED_CLASSES.contains(&c) => t.n_diverged += 1,
            Some(c) if c == UNAVAILABLE_CLASS => t.n_unavailable += 1,
            _ => t.n_unclassified += 1,
        }
    }
    t
}

/// Extract the `per_artifact[]` rows from the loaded `reexecution.json`
/// document. Returns an empty slice-equivalent when the file is absent or
/// carries no `per_artifact` array (present-but-empty first-emit shape).
fn rerun_outcomes(pkg: &LoadedPackage) -> &[Value] {
    pkg.reexecution
        .as_ref()
        .and_then(|doc| doc.get("per_artifact"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// Check equivalence failure.
pub fn check_equivalence_failure(pkg: &LoadedPackage) -> InvariantVerdict {
    let outcomes = rerun_outcomes(pkg);

    // Collect every Q.RerunOutcome that requires acknowledgement: a diverged
    // re-execution outcome read from `reexecution.json::per_artifact[]`. The
    // class lives in `bucket` (canonical `ReexecutionBucket` serialization),
    // and the outcome id is `artifact_path`. (`class`/`id` are accepted as
    // forward-compatible aliases for hand-authored rows.)
    //
    // Each entry carries a `shim_eligible` flag: an
    // `acknowledged_non_determinism` row may be satisfied by a shim `NonDetAck`
    // (the comparator already certified the ack COVERS the divergence when it
    // assigned that bucket — see `reexecution::classify_single_artifact`). A
    // `failed` row must NOT be satisfiable by a bare artifact-level shim match
    // (the comparator emits `failed` precisely when the ack did NOT cover the
    // divergence, e.g. an un-acked column), so `failed` requires an explicit
    // F.Blocker. This keeps the comparator bucket and the invariant unified on
    // the SAME `NonDetAck` source without reintroducing column-level masking.
    let mut needs_ack: Vec<(String, bool)> = outcomes
        .iter()
        .filter_map(|o| {
            let class = o
                .get("bucket")
                .and_then(Value::as_str)
                .or_else(|| o.get("class").and_then(Value::as_str))?;
            if !DIVERGED_CLASSES.contains(&class) {
                return None;
            }
            let id = o
                .get("artifact_path")
                .and_then(Value::as_str)
                .or_else(|| o.get("id").and_then(Value::as_str))?;
            Some((id.to_string(), class == "acknowledged_non_determinism"))
        })
        .collect();
    // The compile-time port-unification trace (`event:"prove"`/`outcome:"failed"`)
    // stays in `verifier-decisions.jsonl`. An unacknowledged prove-failure is the
    // second silent-corruption shape this invariant escalates to `Fail`. It is
    // NOT re-execution evidence — it cannot, on its own, make Q "present". A
    // prove-failure is never shim-eligible (it is not a re-execution divergence).
    needs_ack.extend(pkg.verifier_decisions.iter().filter_map(|v| {
        let is_prove_failed = v.get("event").and_then(Value::as_str) == Some("prove")
            && v.get("outcome").and_then(Value::as_str) == Some("failed");
        if is_prove_failed {
            v.get("edge_id")
                .and_then(Value::as_str)
                .or_else(|| v.get("id").and_then(Value::as_str))
                .map(|id| (id.to_string(), false))
        } else {
            None
        }
    }));
    // Re-execution evidence: the spec's Q sub-graph is the set of RerunOutcomes
    // in `reexecution.json::per_artifact[]`. A non-empty list means re-execution
    // was performed; an absent file OR a present-but-empty `per_artifact` means
    // it was not — in which case equivalence cannot be confirmed and the verdict
    // is `Unverified` (spec §4 verdict table: "Q absent (no re-execution
    // performed) → Unverified"). A compile-time `prove`/`failed` row is NOT
    // re-execution evidence; it can only escalate to `Fail` when unacknowledged.
    let rerun_performed = !outcomes.is_empty();
    // Real v0.1 assumptions carry `{assumption_id, kind, detail, stage_id}`
    // and no `edge_id`. Key the ack set on `edge_id` when present
    // (forward-compatible for when the harness threads it) but fall back
    // to the free-text `detail`, then match by containment so an ack
    // whose detail mentions the diverged id still satisfies the predicate.
    let ack: BTreeSet<String> = pkg
        .assumptions
        .iter()
        .filter(|a| {
            matches!(
                a.get("kind").and_then(|s| s.as_str()),
                Some("unprovable_edge" | "policy_exception")
            )
        })
        .filter_map(|a| {
            a.get("edge_id")
                .and_then(|s| s.as_str())
                .or_else(|| a.get("detail").and_then(|s| s.as_str()))
                .map(String::from)
        })
        .collect();
    // The determinism shim is the SECOND acknowledgment source (unification):
    // an `acknowledged_non_determinism` divergence is satisfied by a matching
    // `NonDetAck` even when no `assumptions.jsonl` F.Blocker exists. Read via
    // the already-loaded `pkg.determinism_shim` (loader reads
    // `runtime/determinism-shim.json`); a partial/absent shim → no shim acks.
    let shim: Option<DeterminismShimSidecar> = pkg
        .determinism_shim
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    let violators: Vec<String> = needs_ack
        .iter()
        .filter(|(e, shim_eligible)| {
            let acked_by_blocker = ack.iter().any(|a| a == e || a.contains(e.as_str()));
            let acked_by_shim =
                *shim_eligible && shim.as_ref().and_then(|s| ack_for(s, e)).is_some();
            !(acked_by_blocker || acked_by_shim)
        })
        .map(|(e, _)| e.clone())
        .collect();
    let n_violations = violators.len();
    // Census of the Q rows, independent of the acknowledgement scan: it counts
    // what the COMPARATOR did, while `needs_ack`/`violators` count what still
    // needs a human declaration (and additionally fold in compile-time
    // prove-failures, which are not re-execution rows at all).
    let tally = tally_rerun_outcomes(outcomes);
    // `n_inspected` is the predicate's ∀-domain: every item this invariant
    // actually put under inspection. That is each artifact the comparator
    // compared — reproduced (`tally.n_compared`) or diverged — plus each
    // compile-time prove-failure that required an acknowledgement
    // (`needs_ack`, which holds the diverged rows and the prove-failures and is
    // disjoint from `n_compared`). `unavailable`/unclassified rows are excluded:
    // nothing about them was inspected.
    //
    // Reporting the divergence-CANDIDATE count here instead (`needs_ack.len()`
    // alone) understated a clean replay as `n_inspected: 0` — a deposit whose 6
    // artifacts all reproduced published `pass, n_inspected: 0`, which the wire
    // contract defines as "Items inspected" and which this repo's own
    // benchmark-readiness gate equates with vacuity. Every sibling invariant
    // reports the same ∀-domain shape (`claim_completeness` → `verdicts.len()`,
    // `evidence_coverage` → `outputs.len()`). Summing rather than reporting
    // `n_compared` alone also keeps `n_violations <= n_inspected`: a run of
    // nothing but unacknowledged divergences compares zero artifacts yet
    // inspects — and violates — every one of them.
    let n_inspected = tally.n_compared + needs_ack.len();
    let census = format!(
        "{} of {} artifact(s) reproduced ({} unavailable, {} acknowledged divergence(s), \
         {} unclassified)",
        tally.n_compared,
        tally.n_rows(),
        tally.n_unavailable,
        tally.n_diverged,
        tally.n_unclassified
    );
    let (status, detail) = if n_violations > 0 {
        (
            InvariantStatus::Fail,
            Some(format!(
                "{} unacknowledged divergence(s): {}",
                n_violations,
                violators.join(", ")
            )),
        )
    } else if !rerun_performed {
        // No re-execution performed: equivalence cannot be confirmed (spec §4).
        (
            InvariantStatus::Unverified,
            Some("no re-execution performed (Q absent)".to_string()),
        )
    } else if !tally.any_comparison() {
        // Rows exist, but not one artifact was actually compared — every row is
        // `unavailable` (or carries no recognizable class). There is no
        // divergence to acknowledge and equally no equivalence to certify, so
        // the honest verdict is Unverified. Coercing this to `Pass` would make a
        // re-execution that compared NOTHING indistinguishable from one in which
        // everything reproduced.
        (
            InvariantStatus::Unverified,
            Some(format!("re-execution compared no artifact: {census}")),
        )
    } else {
        // Re-execution ran, at least one artifact was compared, and every
        // diverged outcome is acknowledged. The census rides along so the Pass
        // states HOW MUCH reproduced rather than asserting a bare "pass".
        (InvariantStatus::Pass, Some(census))
    };
    InvariantVerdict {
        id: InvariantId::EquivalenceFailure,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Wrap `per_artifact` rows in the real `reexecution.json` document shape.
    fn reexecution_doc(per_artifact: Vec<Value>) -> Value {
        json!({
            "schema_version": "0.1",
            "bucket_counts": {},
            "per_artifact": per_artifact,
        })
    }

    fn row(path: &str, bucket: &str) -> Value {
        json!({"artifact_path": path, "bucket": bucket})
    }

    fn pkg_with_rows(per_artifact: Vec<Value>) -> LoadedPackage {
        LoadedPackage {
            reexecution: Some(reexecution_doc(per_artifact)),
            ..Default::default()
        }
    }

    /// A re-execution in which NOTHING was comparable must not read as a clean
    /// reproduction. `unavailable` rows need no acknowledgement, so the
    /// divergence count is 0 and the pre-fix code took the `Pass` branch purely
    /// because `per_artifact` was non-empty.
    #[test]
    fn all_unavailable_is_unverified_not_pass() {
        let pkg = pkg_with_rows(vec![
            row("runtime/outputs/de/de_results.tsv", "unavailable"),
            row("runtime/outputs/de/normalized_counts.tsv", "unavailable"),
            row("runtime/outputs/de/figures/volcano.png", "unavailable"),
        ]);
        let tally = tally_rerun_outcomes(&[
            row("a", "unavailable"),
            row("b", "unavailable"),
            row("c", "unavailable"),
        ]);
        assert_eq!(tally.n_compared, 0, "nothing was reproduced");
        assert_eq!(tally.n_unavailable, 3, "all three rows were incomparable");
        assert!(
            !tally.any_comparison(),
            "no artifact reached the comparator"
        );

        let v = check_equivalence_failure(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Unverified,
            "0-of-3 comparable must be Unverified, not Pass: {:?}",
            v.detail
        );
        assert_eq!(v.n_violations, 0, "an unavailable row is not a divergence");
        assert_eq!(
            v.n_inspected, 0,
            "nothing reached the comparator, so nothing was inspected — this is \
             the one shape in which a 0 is the honest answer"
        );
        let detail = v.detail.expect("Unverified must explain itself");
        assert!(
            detail.contains("0 of 3 artifact(s) reproduced"),
            "the census must make the emptiness legible: {detail}"
        );
    }

    /// The reference deposit's exact shape: 2 `byte_identical` +
    /// 4 `semantic_equivalent` + 22 `unavailable` = 6 of 28 compared, nothing
    /// diverged. Six artifacts went through the comparator, so `n_inspected` is
    /// 6 — not the divergence-candidate count of 0 that made a clean replay
    /// publish `pass, n_inspected: 0` and read as vacuous.
    #[test]
    fn compared_artifacts_are_counted_as_inspected() {
        let mut rows: Vec<Value> = (0..2)
            .map(|i| {
                row(
                    &format!("runtime/outputs/normalisation/b{i}.tsv"),
                    "byte_identical",
                )
            })
            .collect();
        rows.extend((0..4).map(|i| {
            row(
                &format!("runtime/outputs/normalisation/s{i}.tsv"),
                "semantic_equivalent",
            )
        }));
        rows.extend((0..22).map(|i| row(&format!("runtime/outputs/de/u{i}.tsv"), "unavailable")));

        let v = check_equivalence_failure(&pkg_with_rows(rows));
        assert_eq!(
            v.status,
            InvariantStatus::Pass,
            "6 reproduced / 0 diverged is a Pass: {:?}",
            v.detail
        );
        assert_eq!(
            v.n_inspected, 6,
            "both reproduced classes are inspections: byte_identical and \
             semantic_equivalent each went through the comparator"
        );
        assert_eq!(
            v.n_violations, 0,
            "nothing diverged, so nothing is unacknowledged"
        );
        let detail = v.detail.expect("a Pass must report how much reproduced");
        assert!(
            detail.contains("6 of 28 artifact(s) reproduced"),
            "the census must agree with n_inspected: {detail}"
        );
    }

    /// `n_violations` can never exceed `n_inspected`: an item that violates the
    /// predicate was, by definition, inspected. A run of nothing but
    /// unacknowledged divergences compares zero artifacts, so a `n_compared`-only
    /// `n_inspected` would report 0 inspected / 2 violating.
    #[test]
    fn violations_are_a_subset_of_inspected() {
        let pkg = pkg_with_rows(vec![
            row("runtime/outputs/de/de_results.tsv", "failed"),
            row("runtime/outputs/de/normalized_counts.tsv", "failed"),
        ]);
        let v = check_equivalence_failure(&pkg);
        assert_eq!(v.status, InvariantStatus::Fail, "{:?}", v.detail);
        assert_eq!(v.n_violations, 2, "both divergences are unacknowledged");
        assert!(
            v.n_inspected >= v.n_violations,
            "a violating item was inspected: n_inspected={} < n_violations={}",
            v.n_inspected,
            v.n_violations
        );
    }

    /// The real-deposit shape: a minority of artifacts reproduced, the rest
    /// unavailable, nothing diverged. That IS a Pass — but the report must carry
    /// the compared count so it cannot be read as "everything reproduced".
    #[test]
    fn some_reproduced_none_diverged_is_pass() {
        let mut rows: Vec<Value> = (0..4)
            .map(|i| row(&format!("runtime/outputs/de/t{i}.tsv"), "byte_identical"))
            .collect();
        rows.extend((0..17).map(|i| row(&format!("runtime/outputs/de/u{i}.tsv"), "unavailable")));
        let tally = tally_rerun_outcomes(&rows);
        assert_eq!(tally.n_compared, 4, "four artifacts reproduced");
        assert_eq!(tally.n_unavailable, 17, "seventeen were incomparable");
        assert_eq!(tally.n_rows(), 21, "census must cover every row");

        let v = check_equivalence_failure(&pkg_with_rows(rows));
        assert_eq!(
            v.status,
            InvariantStatus::Pass,
            "a genuine reproduction is still a Pass: {:?}",
            v.detail
        );
        assert_eq!(
            v.n_inspected, 4,
            "the four compared artifacts are what was inspected; the 17 \
             unavailable rows are not"
        );
        assert_eq!(v.n_violations, 0, "nothing unacknowledged");
        let detail = v.detail.expect("a Pass must report how much reproduced");
        assert!(
            detail.contains("4 of 21 artifact(s) reproduced"),
            "compared count must be distinguishable from the divergence count: {detail}"
        );
    }

    /// Regression: the census must not soften a genuine unacknowledged
    /// divergence. `Fail` outranks every other branch.
    #[test]
    fn diverged_unacknowledged_still_fails() {
        let pkg = pkg_with_rows(vec![
            row("runtime/outputs/de/de_results.tsv", "failed"),
            row("runtime/outputs/de/counts.tsv", "byte_identical"),
            row("runtime/outputs/de/qc.tsv", "unavailable"),
        ]);
        let v = check_equivalence_failure(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Fail,
            "an unacknowledged divergence must still Fail: {:?}",
            v.detail
        );
        assert_eq!(
            v.n_inspected, 2,
            "two artifacts reached the comparator — the `failed` divergence and \
             the `byte_identical` reproduction; the `unavailable` row did not"
        );
        assert_eq!(
            v.n_violations, 1,
            "and the divergence had no acknowledgement"
        );
    }

    /// An `unavailable`-only run whose rows are ALSO all unacknowledged-free
    /// still Fails when the compile-time port-unification trace carries an
    /// unacknowledged `prove`/`failed` row: `Fail` is evaluated before the
    /// no-comparison check.
    #[test]
    fn prove_failure_outranks_no_comparison() {
        let pkg = LoadedPackage {
            reexecution: Some(reexecution_doc(vec![row("a.tsv", "unavailable")])),
            verifier_decisions: vec![json!({"event":"prove","outcome":"failed","edge_id":"e-1"})],
            ..Default::default()
        };
        let v = check_equivalence_failure(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Fail,
            "an unacknowledged prove-failure outranks the no-comparison branch"
        );
    }

    /// An acknowledged divergence IS a comparison the comparator performed, so
    /// a run of nothing but acknowledged divergences stays a `Pass` — the
    /// no-comparison branch keys on "no artifact reached the comparator", not
    /// on "nothing reproduced byte-for-byte".
    #[test]
    fn acknowledged_divergence_alone_is_still_a_comparison() {
        let pkg = LoadedPackage {
            reexecution: Some(reexecution_doc(vec![row(
                "runtime/outputs/de/de_results.tsv",
                "acknowledged_non_determinism",
            )])),
            assumptions: vec![
                json!({"kind":"policy_exception","edge_id":"runtime/outputs/de/de_results.tsv"}),
            ],
            ..Default::default()
        };
        let v = check_equivalence_failure(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Pass,
            "an acknowledged divergence is evidence the comparator ran: {:?}",
            v.detail
        );
    }

    /// A row with an unrecognized (or missing) bucket is not evidence of a
    /// comparison; a document made only of those is Unverified.
    #[test]
    fn unclassified_rows_are_not_a_comparison() {
        let pkg = pkg_with_rows(vec![
            json!({"artifact_path": "a.tsv"}),
            row("b.tsv", "not_a_real_bucket"),
        ]);
        let v = check_equivalence_failure(&pkg);
        assert_eq!(
            v.status,
            InvariantStatus::Unverified,
            "unrecognized buckets certify nothing: {:?}",
            v.detail
        );
        let detail = v.detail.expect("Unverified must explain itself");
        assert!(
            detail.contains("2 unclassified"),
            "unclassified rows must be counted, not ignored: {detail}"
        );
    }
}
