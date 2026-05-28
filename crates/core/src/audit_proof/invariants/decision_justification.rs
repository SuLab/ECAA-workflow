//! Invariant 2: decision-justification.
//! Every MethodChoice has `cites` non-empty OR `rationale` ≥30 chars.

use crate::audit_proof::loader::LoadedPackage;
use crate::audit_proof::{InvariantId, InvariantStatus, InvariantVerdict};

const RATIONALE_MIN_CHARS: usize = 30;

/// Check decision justification.
pub fn check_decision_justification(pkg: &LoadedPackage) -> InvariantVerdict {
    let mut n_inspected = 0;
    let mut violators = Vec::new();
    for d in &pkg.decisions {
        let kind = d.get("kind").and_then(|s| s.as_str()).unwrap_or("");
        if kind != "method_choice" {
            continue;
        }
        n_inspected += 1;
        let cites_ok = d
            .get("cites")
            .and_then(|s| s.as_array())
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        let rationale_ok = d
            .get("rationale")
            .and_then(|s| s.as_str())
            .map(|s| s.chars().count() >= RATIONALE_MIN_CHARS)
            .unwrap_or(false);
        if !cites_ok && !rationale_ok {
            let id = d
                .get("decision_id")
                .and_then(|s| s.as_str())
                .unwrap_or("<unknown>");
            violators.push(id.to_string());
        }
    }
    if n_inspected == 0 {
        return InvariantVerdict {
            id: InvariantId::DecisionJustification,
            status: InvariantStatus::Unverified,
            detail: Some("no method_choice decisions present".into()),
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
            "{} method_choice decision(s) lack citation and have rationale <{} chars: {}",
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
