//! Tier F property test for F8 — an adapter's safety classification is
//! consistent with its declared transformation kind: purely mechanical
//! classes (compression, indexing, sorting) are `Lossless`;
//! scientifically-impactful classes (normalization, coordinate
//! liftover) are never `Lossless`. Iterates the canonical starter
//! registry — site-local YAML adapters that break the invariant fail
//! this gate.
//!
//! Replaces the prior `prop_assert!(true)` placeholder.

use ecaa_workflow_core::adapter_registry::{AdapterClass, AdapterRegistry, AdapterSafety};

#[test]
fn mechanical_adapter_classes_are_lossless() {
    let reg = AdapterRegistry::with_starters();
    let mut checked = 0;
    for (id, a) in reg.iter() {
        if matches!(
            a.class,
            AdapterClass::Compression | AdapterClass::IndexGeneration | AdapterClass::Sorting
        ) {
            assert_eq!(
                a.safety,
                AdapterSafety::Lossless,
                "F8 violation: mechanical adapter {id} (class {:?}) is not Lossless: {:?}",
                a.class,
                a.safety
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 3,
        "expected ≥3 mechanical starter adapters (compression/index/sort), saw {checked}"
    );
}

#[test]
fn scientific_adapter_classes_are_never_lossless() {
    let reg = AdapterRegistry::with_starters();
    let mut checked = 0;
    for (id, a) in reg.iter() {
        if matches!(
            a.class,
            AdapterClass::Normalization | AdapterClass::CoordinateLiftover
        ) {
            assert_ne!(
                a.safety,
                AdapterSafety::Lossless,
                "F8 violation: scientific adapter {id} (class {:?}) is mislabeled Lossless",
                a.class
            );
            checked += 1;
        }
    }
    assert!(
        checked >= 2,
        "expected ≥2 scientific starter adapters (normalization/liftover), saw {checked}"
    );
}
