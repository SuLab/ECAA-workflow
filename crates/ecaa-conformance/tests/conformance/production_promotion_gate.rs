//! WG3 promotion gate.
//!
//! Couples the `production_ready: true` maturity flag on the cross-omics
//! archetypes to the strict-mode evidence: every cross-omics archetype that
//! has been promoted MUST compose CLEAN under `RiskMode::Production`
//! (`ECAA_COMPOSE_STRICT`) — i.e. with zero `OrderingOnly`/`Unproven` edges.
//! The strict scorer routes any such edge to a `PartialDag`, which
//! `compose_with_modalities_full_pref_strict` surfaces as `Err`, so a clean
//! compose is exactly `Ok`.
//!
//! This is the mechanical guard that prevents a future cross-omics archetype
//! from being marked production-ready without the WG3 edge-typing passes
//! (validate / aggregator / companion / residual) making its package
//! strict-composable. The full blinded DAG-correctness corpus already
//! verifies all 76 scenarios compose clean under `ECAA_COMPOSE_STRICT`; this
//! Rust gate locks the cross-omics promotion specifically, inside the
//! conformance crate.

use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

#[test]
fn promoted_cross_omics_archetypes_compose_clean_under_production() {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::composer::compose_with_modalities_full_pref_strict;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use ecaa_workflow_core::preferred_methods::PreferredMethods;

    let config = config_root();
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");

    let mut checked = 0usize;
    for (id, arch) in archetypes.iter() {
        if !id.starts_with("cross_omics_") || !arch.production_ready {
            continue;
        }
        assert!(
            !arch.cross_omics_modalities.is_empty(),
            "cross_omics archetype {id} must declare cross_omics_modalities"
        );
        let goal = GoalSpec {
            edam_data: arch.goal_data.clone(),
            edam_format: arch.goal_format.clone(),
            modifiers: Default::default(),
            source_prose: Some(format!("WG3 promotion-gate fixture for {id}")),
            confidence: 0.0,
        };
        let modalities: Vec<&str> = arch
            .cross_omics_modalities
            .iter()
            .map(|s| s.as_str())
            .collect();
        // compose_strict = true -> RiskMode::Production. A residual
        // OrderingOnly/Unproven edge scores Reject -> PartialDag -> Err.
        let out = compose_with_modalities_full_pref_strict(
            &goal,
            &arch.project_class,
            &atoms,
            &archetypes,
            &modalities,
            None,
            None,
            None,
            &PreferredMethods::new(),
            true,
            // Ensemble gate off for this promotion-gate check.
            None,
            false,
        );
        assert!(
            out.is_ok(),
            "promoted cross_omics archetype {id} must compose clean under \
             RiskMode::Production (zero OrderingOnly/Unproven edges); got {:?}",
            out.err()
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "expected >= 5 promoted cross_omics archetypes (production_ready: true), checked {checked}"
    );
}
