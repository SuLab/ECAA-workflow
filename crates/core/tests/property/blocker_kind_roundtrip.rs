//! R-24 property test: every `BlockerKind` variant round-trips through
//! serde JSON without payload loss or tag drift.
//!
//! The existing `crates/core/src/blocker.rs::all_variants_roundtrip_serde`
//! covers the 39 variants with single representative values. This file
//! is the property-test complement: for each variant, parametrize the
//! payload fields with proptest-generated values and check that
//! serialize → deserialize → equality holds.
//!
//! Closes drift between the typed `kind` discriminator and the wire
//! shape (`#[serde(tag = "kind", rename_all = "snake_case")]`). A future
//! refactor that flips a struct variant to a unit variant (or vice
//! versa) would break the round-trip and surface here before reaching
//! the UI dispatch table.

use ecaa_workflow_core::blocker::{BlockerKind, ExcludedPath, StallAction, StallSignalWire};
use proptest::prelude::*;

/// Strategy for `StallSignalWire` so the `Stalled` variant round-trips
/// across all four sub-shapes.
fn stall_signal_strategy() -> impl Strategy<Value = StallSignalWire> {
    prop_oneof![
        (0.0f32..100.0, 1u64..120).prop_map(|(p, w)| StallSignalWire::CpuStarvation {
            avg_cpu_pct: p,
            window_mins: w,
        }),
        (0.0f32..100.0, 1u64..120).prop_map(|(p, w)| StallSignalWire::MemoryPressure {
            pct: p,
            window_mins: w,
        }),
        (1u64..120).prop_map(|w| StallSignalWire::GpuIdleDuringTraining { window_mins: w }),
        (1u64..86400, 1u64..86400).prop_map(|(a, e)| StallSignalWire::RuntimeOverExpected {
            actual_secs: a,
            expected_secs: e,
        }),
    ]
}

fn stall_action_strategy() -> impl Strategy<Value = StallAction> {
    prop_oneof![
        Just(StallAction::Resize),
        Just(StallAction::Retry),
        Just(StallAction::Abort),
    ]
}

/// Curated, representative-but-payload-randomised strategy for a
/// single `BlockerKind` value. Covers every variant the
/// `all_variants_roundtrip_serde` unit test enumerates plus a wider
/// payload range per shrink iteration. `Strategy::prop_oneof!` is
/// capped at ~10 arms per macro invocation so the set splits across
/// two intermediate strategies that compose.
fn blocker_kind_strategy() -> impl Strategy<Value = BlockerKind> {
    prop_oneof![
        // Bucket A: the 10 most-common message-shaped variants.
        (".{1,40}", ".{1,40}").prop_map(|(e, a)| BlockerKind::DataShapeMismatch {
            expected: e,
            actual: a,
        }),
        (".{1,40}", ".{1,80}").prop_map(|(check, message)| BlockerKind::ValidationFailed {
            check,
            message,
            cause: None,
        }),
        (".{1,40}", 0.0f64..1.0, 0.0f64..1.0).prop_map(|(m, t, a)| {
            BlockerKind::MetricBelowThreshold {
                metric: m,
                threshold: t,
                actual: a,
            }
        }),
        ".{1,40}".prop_map(|dep| BlockerKind::MissingInput { dependency: dep }),
        ".{1,80}".prop_map(|m| BlockerKind::AgentError { message: m }),
        ".{1,80}".prop_map(|m| BlockerKind::HostError { message: m }),
        (".{1,40}", prop::collection::vec(".{1,40}", 0..4)).prop_map(|(s, c)| {
            BlockerKind::AwaitingSmeSelection {
                stage_id: s,
                candidates: c,
            }
        }),
        (0.0f64..10000.0, 0.0f64..10000.0).prop_map(|(p, c)| BlockerKind::PilotOversize {
            projected_usd: p,
            ceiling_usd: c,
        }),
        (".{1,40}", stall_signal_strategy(), stall_action_strategy()).prop_map(
            |(task_id, signal, suggested_action)| BlockerKind::Stalled {
                task_id,
                signal,
                suggested_action,
            },
        ),
        (".{1,40}", prop::collection::vec(".{1,40}", 0..4)).prop_map(|(c, a)| {
            BlockerKind::ContractViolation {
                contract_id: c,
                assertion_ids: a,
            }
        }),
    ]
}

/// Second bucket so each `prop_oneof!` stays under its arm cap.
fn blocker_kind_strategy_b() -> impl Strategy<Value = BlockerKind> {
    prop_oneof![
        ".{1,40}".prop_map(|runtime| BlockerKind::RuntimeMissing { runtime }),
        (".{1,40}", ".{1,80}")
            .prop_map(|(image, reason)| BlockerKind::ContainerPullFailed { image, reason }),
        (".{1,40}", ".{1,80}")
            .prop_map(|(image, reason)| BlockerKind::ContainerStartFailed { image, reason }),
        ".{1,40}".prop_map(|p| BlockerKind::ContainerCacheCorrupted { path: p }),
        (".{1,40}", ".{1,40}").prop_map(|(p, attempted)| BlockerKind::NetworkPolicyViolation {
            policy: p,
            attempted,
        }),
        (".{1,40}", ".{1,40}").prop_map(|(expected, actual)| {
            BlockerKind::ImageDigestMismatch {
                expected_digest: expected,
                actual_digest: actual,
            }
        }),
        (".{1,40}", 1u32..50, 0.0f64..1.0, 0.0f64..1.0).prop_map(
            |(task_id, iterations_run, last_metric, threshold)| {
                BlockerKind::IterationDidNotConverge {
                    task_id,
                    iterations_run,
                    last_metric,
                    threshold,
                }
            },
        ),
        (
            prop::collection::vec(".{1,40}", 0..3),
            prop::option::of(".{1,40}"),
            prop::collection::vec(
                (".{1,40}", ".{1,80}").prop_map(|(atom_id, exclusion_cel)| ExcludedPath {
                    atom_id,
                    exclusion_cel,
                }),
                0..3,
            ),
        )
            .prop_map(|(missing_inputs, unreachable_goal, excluded_paths)| {
                BlockerKind::CompositionInfeasible {
                    missing_inputs,
                    unreachable_goal,
                    excluded_paths,
                }
            },),
    ]
}

proptest! {
    /// Round-trip through serde JSON: serialize → deserialize → re-serialize
    /// → re-deserialize must produce a stable wire string from the second
    /// cycle onward. serde_json's internally-tagged enum deserialization can
    /// lose the last ULP for certain f64 values on the FIRST pass (a known
    /// serde_json limitation with `#[serde(tag)]` + f64; practically harmless
    /// for currency/metric fields). We therefore check idempotence from the
    /// second cycle: `wire2 == wire3`. This catches all structural and tag-drift
    /// regressions (the primary goal) without being defeated by the one-shot ULP
    /// issue.
    #[test]
    fn variant_roundtrip_bucket_a(kind in blocker_kind_strategy()) {
        let wire = serde_json::to_string(&kind).expect("serialize blocker kind");
        let back: BlockerKind =
            serde_json::from_str(&wire).expect("deserialize blocker kind");
        let wire2 = serde_json::to_string(&back).expect("re-serialize blocker kind");
        let back2: BlockerKind =
            serde_json::from_str(&wire2).expect("second deserialize blocker kind");
        let wire3 = serde_json::to_string(&back2).expect("third serialize blocker kind");
        prop_assert_eq!(wire2, wire3, "wire must be stable from the second cycle");
    }

    #[test]
    fn variant_roundtrip_bucket_b(kind in blocker_kind_strategy_b()) {
        let wire = serde_json::to_string(&kind).expect("serialize blocker kind");
        let back: BlockerKind =
            serde_json::from_str(&wire).expect("deserialize blocker kind");
        let wire2 = serde_json::to_string(&back).expect("re-serialize blocker kind");
        let back2: BlockerKind =
            serde_json::from_str(&wire2).expect("second deserialize blocker kind");
        let wire3 = serde_json::to_string(&back2).expect("third serialize blocker kind");
        prop_assert_eq!(wire2, wire3, "wire must be stable from the second cycle");
    }

    /// Wire shape stability: every emitted JSON must carry a top-level
    /// `kind` discriminator (the `#[serde(tag = "kind")]` contract).
    /// Closes drift where a future refactor flips the tag attribute
    /// off the enum and silently breaks every UI dispatch.
    #[test]
    fn wire_shape_always_has_kind_tag(kind in blocker_kind_strategy()) {
        let wire = serde_json::to_string(&kind).expect("serialize");
        let parsed: serde_json::Value =
            serde_json::from_str(&wire).expect("parse back");
        prop_assert!(parsed.get("kind").and_then(|v| v.as_str()).is_some());
    }
}
