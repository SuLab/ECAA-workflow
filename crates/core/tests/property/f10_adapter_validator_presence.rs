//! Tier F property test for F10 — every Lossless adapter brings a
//! downstream validator into the emitted validator set, so contracted
//! output shape is checked rather than trusted.
//!
//! The invariant is enforced at adapter-registration time: every Lossless
//! entry in `starter_adapters()` carries at least one `ValidatorRef`
//! in its `validators` field. Site-local YAML adapters that fail the
//! invariant will fail this gate at registry-build time.

use scripps_workflow_core::adapter_registry::{AdapterRegistry, AdapterSafety};

#[test]
fn every_lossless_starter_carries_at_least_one_validator() {
    let reg = AdapterRegistry::with_starters();
    let mut checked = 0;
    for (id, adapter) in reg.iter() {
        if matches!(adapter.safety, AdapterSafety::Lossless) {
            assert!(
                !adapter.validators.is_empty(),
                "F10 violation: Lossless adapter {id} has no validators"
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 7,
        "expected at least the 7 starter Lossless adapters, saw {checked}"
    );
}

#[test]
fn every_adapter_validator_has_a_stable_id() {
    let reg = AdapterRegistry::with_starters();
    for (adapter_id, adapter) in reg.iter() {
        for v in &adapter.validators {
            assert!(
                !v.id.is_empty(),
                "F10 violation: empty validator id on adapter {adapter_id}"
            );
        }
    }
}

#[test]
fn lossless_adapters_validators_are_at_least_one_total() {
    let reg = AdapterRegistry::with_starters();
    let total: usize = reg
        .iter()
        .filter(|(_, a)| matches!(a.safety, AdapterSafety::Lossless))
        .map(|(_, a)| a.validators.len())
        .sum();
    // 7 Lossless starters x at least 1 validator = 7 minimum.
    assert!(
        total >= 7,
        "F10 violation: expected ≥ 7 validators across Lossless starters, got {total}"
    );
}
