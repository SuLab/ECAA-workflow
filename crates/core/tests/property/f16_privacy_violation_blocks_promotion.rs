//! Tier F property tests for F16: a privacy-class violation along
//! an edge blocks promotion of the consuming node and refuses to
//! emit the package — the privacy ratchet is one-directional.
//!
//! v3 P5 closes this stub. The property: every synthetic SSN-like
//! payload that lands inside a JSONL stream is detected by
//! `detect_phi_leak` at the `RedactedAudit` / `ExportablePublic` /
//! `Suppressed` tier (i.e. any non-`Private` tier), so the emit
//! pipeline refuses to ship the package.
//!
//! The `Private` tier deliberately short-circuits — it's
//! access-controlled by definition, so PHI-pattern scanning is
//! redundant. The property covers that as well: a `Private` scan of
//! the same payload returns an empty leak set so the emit proceeds.

use proptest::prelude::*;
use scripps_workflow_core::provenance_tiers::{detect_phi_leak, ProvenanceTier};

proptest! {
    /// F16 property — any SSN-shaped triple inside a JSONL field is
    /// detected as a leak under the three non-`Private` tiers.
    #[test]
    fn synthetic_phi_pattern_in_non_private_blocks_emit(
        s in "[0-9]{3}-[0-9]{2}-[0-9]{4}",
    ) {
        let jsonl = format!(r#"{{"id":"x","body":"{}"}}"#, s);
        for tier in [
            ProvenanceTier::RedactedAudit,
            ProvenanceTier::ExportablePublic,
            ProvenanceTier::Suppressed,
        ] {
            let leaks = detect_phi_leak(&jsonl, tier);
            prop_assert!(
                !leaks.is_empty(),
                "F16 violation: PHI pattern {} in tier {:?} not detected",
                s, tier,
            );
        }
    }

    /// F16 property — `Private` tier short-circuits (no leaks reported)
    /// because Private is access-controlled by definition.
    #[test]
    fn synthetic_phi_pattern_in_private_short_circuits(
        s in "[0-9]{3}-[0-9]{2}-[0-9]{4}",
    ) {
        let jsonl = format!(r#"{{"id":"x","body":"{}"}}"#, s);
        let leaks = detect_phi_leak(&jsonl, ProvenanceTier::Private);
        prop_assert!(
            leaks.is_empty(),
            "F16 expectation: Private tier must skip the PHI scan; got leaks for {}",
            s,
        );
    }
}
