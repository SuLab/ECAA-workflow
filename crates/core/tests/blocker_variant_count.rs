//! Compile-time + run-time gate keeping the documented `BlockerKind`
//! variant count in lock-step with the enum.
//!
//! R-14 — the `recovery_hint_for_blocker` dispatch in
//! `crates/conversation/src/session/transitions.rs` enumerates every
//! current variant explicitly so adding a new variant can't slip past
//! code review under the `#[non_exhaustive]` wildcard. This test pins
//! the count documented in CLAUDE.md, the `all_variants_roundtrip_serde`
//! fixture in `blocker.rs`, and the per-variant arm count in the
//! conversation crate's recovery-hint dispatch — bump them together.
//!
//! Bumping rule: when you add a new variant to `BlockerKind`,
//! 1. add an explicit match arm in
//!    `crates/conversation/src/session/transitions.rs::recovery_hint_for_blocker`,
//! 2. add a fixture row in `blocker.rs::tests::all_variants_roundtrip_serde`,
//! 3. update CLAUDE.md to reflect the new variant count,
//! 4. bump the constant in this file.

use scripps_workflow_core::blocker::BlockerKind;
use strum::EnumCount;

#[test]
fn blocker_kind_count_matches_documented() {
    // CLAUDE.md asserts 47 variants. Bump this number, the CLAUDE.md
    // doc, the `all_variants_roundtrip_serde` fixture, and the
    // recovery-hint dispatch together when adding a variant.
    assert_eq!(
        BlockerKind::COUNT,
        47,
        "BlockerKind variant count drifted from documented total — \
         update CLAUDE.md, `all_variants_roundtrip_serde`, the \
         exhaustive `recovery_hint_for_blocker` arms in \
         crates/conversation/src/session/transitions.rs, and this test \
         constant together (R-14)."
    );
}
