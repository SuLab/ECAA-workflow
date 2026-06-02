//! Invariant 4: equivalence-failure.
//! Every re-execution divergence must be acknowledged by an F Blocker
//! (`unprovable_edge` / `policy_exception`). Per spec §4, the predicate ranges
//! over `Q.RerunOutcomes`: any outcome whose `class` is `failed` or
//! `acknowledged_non_determinism` requires a corresponding Blocker. The
//! reference impl reads the raw `verifier-decisions.jsonl`, which carries BOTH
//! re-execution `RerunOutcome` rows (a flat `class` field, populated post-emit
//! by the harness classifier) AND the compile-time port-unification trace
//! (`event:"prove"` / `outcome:"failed"` rows). Both an unacknowledged diverged
//! `RerunOutcome` and an unacknowledged compile-time prove-failure are silent-
//! corruption cases this invariant catches.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};
use std::collections::BTreeSet;

/// `Q.RerunOutcome.class` values that count as a divergence requiring
/// acknowledgement. These are the two the spec §4 predicate names, drawn from
/// the closed 5-class enum in spec §5.6. (`byte_identical`,
/// `semantic_equivalent` and `unavailable` are non-divergent and need no ack.)
const DIVERGED_CLASSES: [&str; 2] = ["failed", "acknowledged_non_determinism"];

/// Check equivalence failure.
pub fn check_equivalence_failure(pkg: &LoadedPackage) -> InvariantVerdict {
    // Collect every verifier-decision that requires acknowledgement:
    //   (a) a re-execution `RerunOutcome` whose `class` is in the diverged set, or
    //   (b) a compile-time `prove`/`failed` port-unification row.
    // Each is keyed by its outcome/edge id; a prove row carries `edge_id`, a
    // RerunOutcome row carries `id` (falling back to `edge_id`).
    let needs_ack: Vec<String> = pkg
        .verifier_decisions
        .iter()
        .filter_map(|v| {
            let is_prove_failed = v.get("event").and_then(|s| s.as_str()) == Some("prove")
                && v.get("outcome").and_then(|s| s.as_str()) == Some("failed");
            let is_diverged_rerun = v
                .get("class")
                .and_then(|s| s.as_str())
                .is_some_and(|c| DIVERGED_CLASSES.contains(&c));
            if is_prove_failed || is_diverged_rerun {
                v.get("id")
                    .and_then(|s| s.as_str())
                    .or_else(|| v.get("edge_id").and_then(|s| s.as_str()))
                    .map(String::from)
            } else {
                None
            }
        })
        .collect();
    if needs_ack.is_empty() {
        return InvariantVerdict {
            id: InvariantId::EquivalenceFailure,
            status: InvariantStatus::Pass,
            detail: None,
            n_inspected: 0,
            n_violations: 0,
        };
    }
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
    let violators: Vec<String> = needs_ack
        .iter()
        .filter(|e| !ack.iter().any(|a| a == *e || a.contains(e.as_str())))
        .cloned()
        .collect();
    let n_inspected = needs_ack.len();
    let n_violations = violators.len();
    let status = if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Fail
    };
    let detail = if n_violations == 0 {
        None
    } else {
        Some(format!(
            "{} unacknowledged divergence(s): {}",
            n_violations,
            violators.join(", ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::EquivalenceFailure,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
