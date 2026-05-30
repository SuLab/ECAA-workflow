//! Tier F property test for F9 — a scientifically-impactful adapter is
//! never marked `Lossless`, the one safety class the planner
//! auto-inserts with *no* assumption-ledger entry (see
//! `AdapterSafety::Lossless` docs: "no assumption ledger entry
//! required"). A risky adapter therefore can never be silently spliced
//! into an edge: it is `LossyDeclared` (ledger entry), `ScientificallyRisky`
//! (SME confirmation), or `PolicyRestricted` (policy approval). This
//! verifies the registry partition the planner relies on, and that the
//! surfacing-required set is non-empty.
//!
//! Replaces the prior `prop_assert!(true)` placeholder.

use ecaa_workflow_core::adapter_registry::{AdapterClass, AdapterRegistry, AdapterSafety};

#[test]
fn no_scientific_adapter_is_silently_insertable() {
    let reg = AdapterRegistry::with_starters();
    for (id, a) in reg.iter() {
        let scientifically_impactful = matches!(
            a.class,
            AdapterClass::Normalization
                | AdapterClass::CoordinateLiftover
                | AdapterClass::IdentifierMapping
        );
        if scientifically_impactful {
            assert_ne!(
                a.safety,
                AdapterSafety::Lossless,
                "F9 violation: scientific adapter {id} (class {:?}) is Lossless → the planner \
                 would auto-insert it with no assumption-ledger entry",
                a.class
            );
        }
    }
}

#[test]
fn surfacing_required_adapters_exist() {
    let reg = AdapterRegistry::with_starters();
    let needs_surfacing = reg
        .iter()
        .filter(|(_, a)| !matches!(a.safety, AdapterSafety::Lossless))
        .count();
    let risky = reg
        .iter()
        .filter(|(_, a)| {
            matches!(
                a.safety,
                AdapterSafety::ScientificallyRisky | AdapterSafety::PolicyRestricted
            )
        })
        .count();
    assert!(
        needs_surfacing >= 1,
        "expected ≥1 non-Lossless starter adapter (would require surfacing), saw {needs_surfacing}"
    );
    assert!(
        risky >= 1,
        "expected ≥1 ScientificallyRisky/PolicyRestricted starter adapter, saw {risky}"
    );
}
