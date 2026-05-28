//! Integration test guarding byte-reproducibility of the Clock-driven
//! emit pipeline. Runs without modifying ro_crate.rs / emitter (that
//! migration happens in a downstream task); this test only verifies the
//! Clock + deterministic_emit_time contract is intact.

use scripps_workflow_core::clock::{deterministic_emit_time, Clock, FrozenClock, WallClock};

#[test]
fn frozen_clock_emit_path_is_deterministic_under_identical_hashes() {
    let hash = [0x42u8; 32];
    let clock1 = FrozenClock {
        at: deterministic_emit_time(&hash),
    };
    let clock2 = FrozenClock {
        at: deterministic_emit_time(&hash),
    };
    assert_eq!(clock1.now(), clock2.now());
    assert_eq!(clock1.now_rfc3339(), clock2.now_rfc3339());
}

#[test]
fn wall_clock_and_frozen_clock_are_distinct_types_but_share_trait() {
    fn now_via_trait(c: &dyn Clock) -> chrono::DateTime<chrono::Utc> {
        c.now()
    }
    let _ = now_via_trait(&WallClock);
    let _ = now_via_trait(&FrozenClock {
        at: chrono::Utc::now(),
    });
}
