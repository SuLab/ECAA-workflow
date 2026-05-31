//! Invariant 2: decision-justification.
//! Every method-choice decision has `method_prose` OR record-level
//! `rationale` ≥30 chars.
//!
//! "Method choice" maps to the two `DecisionType` variants that carry a
//! durable `{stage, method_prose}`: `set_intake_method` (intake-time
//! selection) and `amend_stage` (post-emission swap). The discriminator
//! lives at `decision.kind` — the serde tag on `DecisionType`, nested
//! under `DecisionRecord.decision` on disk. v0.1 has no per-decision
//! `cites` field on these variants, so the predicate reduces to the
//! rationale-length branch over the longer of
//! `{decision.method_prose, record.rationale}`.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};

const RATIONALE_MIN_CHARS: usize = 30;

/// Check decision justification.
pub fn check_decision_justification(pkg: &LoadedPackage) -> InvariantVerdict {
    let mut n_inspected = 0;
    let mut violators = Vec::new();
    for d in &pkg.decisions {
        let decision = d.get("decision");
        let kind = decision
            .and_then(|x| x.get("kind"))
            .and_then(|s| s.as_str())
            .unwrap_or("");
        if kind != "set_intake_method" && kind != "amend_stage" {
            continue;
        }
        n_inspected += 1;
        let method_prose_len = decision
            .and_then(|x| x.get("method_prose"))
            .and_then(|s| s.as_str())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        let rationale_len = d
            .get("rationale")
            .and_then(|s| s.as_str())
            .map(|s| s.chars().count())
            .unwrap_or(0);
        if method_prose_len.max(rationale_len) < RATIONALE_MIN_CHARS {
            let id = decision
                .and_then(|x| x.get("stage"))
                .and_then(|s| s.as_str())
                .unwrap_or("<unknown>");
            violators.push(format!("{kind}:{id}"));
        }
    }
    if n_inspected == 0 {
        return InvariantVerdict {
            id: InvariantId::DecisionJustification,
            status: InvariantStatus::Unverified,
            detail: Some("no method-choice decisions present".into()),
            n_inspected: 0,
            n_violations: 0,
        };
    }
    let n_violations = violators.len();
    let status = if n_violations == 0 {
        InvariantStatus::Pass
    } else {
        InvariantStatus::Warn
    };
    let detail = if n_violations == 0 {
        None
    } else {
        Some(format!(
            "{} method-choice decision(s) have method_prose/rationale <{} chars: {}",
            n_violations,
            RATIONALE_MIN_CHARS,
            violators.join(", ")
        ))
    };
    InvariantVerdict {
        id: InvariantId::DecisionJustification,
        status,
        detail,
        n_inspected,
        n_violations,
    }
}
