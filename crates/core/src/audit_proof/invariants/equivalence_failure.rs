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
            let acked_by_shim = *shim_eligible
                && shim.as_ref().and_then(|s| ack_for(s, e)).is_some();
            !(acked_by_blocker || acked_by_shim)
        })
        .map(|(e, _)| e.clone())
        .collect();
    let n_inspected = needs_ack.len();
    let n_violations = violators.len();
    let (status, detail) = if n_violations > 0 {
        (
            InvariantStatus::Fail,
            Some(format!(
                "{} unacknowledged divergence(s): {}",
                n_violations,
                violators.join(", ")
            )),
        )
    } else if rerun_performed {
        // Re-execution ran and every diverged outcome is acknowledged.
        (InvariantStatus::Pass, None)
    } else {
        // No re-execution performed: equivalence cannot be confirmed (spec §4).
        (
            InvariantStatus::Unverified,
            Some("no re-execution performed (Q absent)".to_string()),
        )
    };
    InvariantVerdict {
        id: InvariantId::EquivalenceFailure,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
