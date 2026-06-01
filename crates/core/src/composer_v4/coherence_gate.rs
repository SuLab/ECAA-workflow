//! Coherence gate (Pillar D). Detects semantic incoherence — atoms drawn
//! from unrelated modality pipelines, or analytical atoms orphaned from
//! the goal — and surfaces it as findings (warn/surface, never
//! hard-reject). Phase 1 ships the seam (a no-op pass that runs beside
//! `policy_gate::evaluate`); Phase 3 fills in the detectors.

use crate::workflow_contracts::task_node::WorkflowDag;

#[derive(Debug, Clone, Default)]
pub(crate) struct CoherenceEvaluation {
    pub findings: Vec<String>,
}

/// Phase 1: no-op. The seam exists and is invoked from `plan()` so the
/// wiring is testable and Phase 3 only has to populate `findings`.
pub(crate) fn evaluate(_dag: &WorkflowDag) -> CoherenceEvaluation {
    CoherenceEvaluation::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coherence_gate_is_noop_in_phase1() {
        let dag = WorkflowDag::default();
        assert!(evaluate(&dag).findings.is_empty());
    }
}
