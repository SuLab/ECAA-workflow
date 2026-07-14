//! Compile-time + run-time gate keeping the documented `ClaimStatus`
//! variant count in lock-step with the enum.
//!
//! `ClaimStatus` crosses the RO-Crate / audit-proof C-graph boundary
//! (`claim_sink::project_verdict_rows` maps each variant to an external
//! wire string). Pinning the count means a new verdict variant cannot slip
//! past review without also updating the projection and its external label.
//!
//! Bumping rule: when you add a new variant to `ClaimStatus`,
//! 1. add its arm in `crates/core/src/claim_sink.rs::project_verdict_rows`
//!    (and the plaintext-sidecar count switch),
//! 2. bump the constant in this file.

use ecaa_workflow_core::claim_verifier::ClaimStatus;
use strum::EnumCount;

#[test]
fn claim_status_count_matches_documented() {
    // Five variants: Verified, Mismatch, Unverifiable, Pending, Suspicious.
    // Bump this number and the `project_verdict_rows` projection together
    // when adding a variant.
    assert_eq!(
        ClaimStatus::COUNT,
        5,
        "ClaimStatus variant count drifted — update the \
         `project_verdict_rows` projection in crates/core/src/claim_sink.rs \
         (and its external wire label) and this test constant together."
    );
}
