//! Readiness-gating table for the Aim 3A benchmark. An invariant is
//! `Ready` only once the phase that makes it non-vacuous has landed;
//! scoring a still-vacuous invariant would let the A-vs-B' contrast move on
//! an empty-set certification. The gate is keyed on a structural probe of a
//! reference package, NOT on a hardcoded "phase N done" boolean, so it tracks
//! reality rather than intent.
//!
//! `InvariantId` is a wire type (`#[derive(TS)]`) and intentionally does NOT
//! derive `Ord`/`Hash`, so the per-invariant inspected counts are passed as a
//! `[usize; 6]` array aligned to `InvariantId::ALL` rather than a map keyed by
//! the enum. `index_of` is the alignment helper.

use ecaa_workflow_types::invariants::InvariantId;

/// Why an invariant is (not yet) benchmarkable — surfaced in scorecard meta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    /// Non-vacuous: inspects real content on at least one arm. Benchmarkable.
    Ready,
    /// Still vacuous (empty-set certification on every arm). Excluded.
    Vacuous(&'static str),
}

/// Position of `id` in `InvariantId::ALL` — the alignment index for the
/// `n_inspected` array, since `InvariantId` does not derive `Ord`/`Hash`.
pub fn index_of(id: InvariantId) -> usize {
    InvariantId::ALL
        .iter()
        .position(|&x| x == id)
        .expect("InvariantId::ALL is exhaustive")
}

/// Decide readiness from an evaluated reference report + the presence of the
/// de-vacuifying artifacts. `signed_sink_present` ⇐ Phase 1; `refs_projected`
/// ⇐ Phase 3 (04-C5); `evidence_from_proofs` ⇐ Phase 3 (04-C2).
pub fn readiness_for(
    id: InvariantId,
    n_inspected: usize,
    signed_sink_present: bool,
    refs_projected: bool,
    evidence_from_proofs: bool,
) -> Readiness {
    match id {
        // Inv 1/5: non-vacuous only once the signed sink populates verdicts.
        InvariantId::ClaimCompleteness | InvariantId::CrossGraphIntegrity => {
            if signed_sink_present && n_inspected > 0 {
                Readiness::Ready
            } else {
                Readiness::Vacuous("requires Phase 1 signed verdict sink (F1)")
            }
        }
        // Inv 4: vacuous until ecaa:refs is in the JSON-LD context (04-C5).
        InvariantId::EquivalenceFailure => {
            if refs_projected {
                Readiness::Ready
            } else {
                Readiness::Vacuous("requires Phase 3 ecaa:refs context + refs projection (04-C5)")
            }
        }
        // Inv 3: vacuous until evidence_coverage derives outputs from proofs.jsonl (04-C2).
        InvariantId::EvidenceCoverage => {
            if evidence_from_proofs {
                Readiness::Ready
            } else {
                Readiness::Vacuous("requires Phase 3 evidence_coverage-from-proofs (04-C2/F6)")
            }
        }
        // Inv 2/6: referential, benchmarkable after Phase 0 (already shipping).
        InvariantId::DecisionJustification | InvariantId::SubstrateValidity => Readiness::Ready,
    }
}

/// The set scored by the Aim 3A benchmark at a given readiness state.
/// `n_inspected` is aligned to `InvariantId::ALL` (use `index_of`).
pub fn benchmarkable(
    n_inspected: &[usize; 6],
    signed_sink_present: bool,
    refs_projected: bool,
    evidence_from_proofs: bool,
) -> Vec<InvariantId> {
    InvariantId::ALL
        .into_iter()
        .filter(|&id| {
            let n = n_inspected[index_of(id)];
            matches!(
                readiness_for(
                    id,
                    n,
                    signed_sink_present,
                    refs_projected,
                    evidence_from_proofs
                ),
                Readiness::Ready
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_phase1_excludes_inv1_and_inv5() {
        // Mirror today's live corpus state: no signed sink, no refs, no 04-C2.
        let n = [0usize; 6];
        let set = benchmarkable(&n, false, false, false);
        assert!(!set.contains(&InvariantId::ClaimCompleteness));
        assert!(!set.contains(&InvariantId::CrossGraphIntegrity));
        assert!(!set.contains(&InvariantId::EquivalenceFailure));
        assert!(!set.contains(&InvariantId::EvidenceCoverage));
        // Inv 2/6 are referential — benchmarkable after Phase 0.
        assert!(set.contains(&InvariantId::DecisionJustification));
        assert!(set.contains(&InvariantId::SubstrateValidity));
    }

    #[test]
    fn all_phases_done_benchmarks_all_six() {
        let n = [1usize; 6];
        let set = benchmarkable(&n, true, true, true);
        assert_eq!(set.len(), 6, "all six benchmarkable once Phases 1-3 land");
    }

    #[test]
    fn signed_sink_without_content_stays_vacuous() {
        // Sink present but no verdicts inspected ⇒ Inv 1/5 still vacuous.
        let n = [0usize; 6];
        let set = benchmarkable(&n, true, false, false);
        assert!(!set.contains(&InvariantId::ClaimCompleteness));
        assert!(!set.contains(&InvariantId::CrossGraphIntegrity));
        // But with inspected content, they become Ready.
        let mut n2 = [0usize; 6];
        n2[index_of(InvariantId::ClaimCompleteness)] = 3;
        n2[index_of(InvariantId::CrossGraphIntegrity)] = 3;
        let set2 = benchmarkable(&n2, true, false, false);
        assert!(set2.contains(&InvariantId::ClaimCompleteness));
        assert!(set2.contains(&InvariantId::CrossGraphIntegrity));
    }
}
