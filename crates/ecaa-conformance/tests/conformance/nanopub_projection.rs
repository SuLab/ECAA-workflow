//! Nanopublication + schema.org Claim projection for the C graph (F7).
//!
//! Each C-graph verdict is wrapped as a `schema:ClaimReview`/`schema:Claim`
//! inside a nanopublication (`np:Nanopublication` with
//! `np:hasAssertion`/`hasProvenance`/`hasPublicationInfo`) so the replaceable
//! contract is expressible on a recognised standard. The authoritative C input
//! is the Phase-1 host-signed verdict sink
//! (`runtime/verification-reports/claim-verification.signed.json`); the fixture
//! supplies one, so this gate is NOT `#[ignore]`d. Probe-skips LOUDLY when the
//! toolchain is absent.

use crate::_shacl_harness::{fixture_dir, loud_skip, run_projection, validators_available};

#[test]
fn projects_nanopublication_and_schema_claim() {
    if !validators_available() {
        loud_skip("projects_nanopublication_and_schema_claim");
        return;
    }
    let (status, stdout, stderr) = run_projection("nanopub-claim");
    eprintln!("--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert!(
        status.success(),
        "nanopub-claim fixture must project + PASS (got {status:?})"
    );

    let ttl = std::fs::read_to_string(fixture_dir("nanopub-claim").join("package.ttl"))
        .expect("package.ttl must have been written by project_package.py");

    // The nanopublication head + its three named graphs (np: may serialize
    // under an auto-assigned prefix, so match the local names / full IRIs).
    assert!(
        ttl.contains("Nanopublication"),
        "package.ttl must carry an np:Nanopublication node:\n{ttl}"
    );
    assert!(
        ttl.contains("hasAssertion")
            && ttl.contains("hasProvenance")
            && ttl.contains("hasPublicationInfo"),
        "package.ttl must link assertion/provenance/pubinfo graphs:\n{ttl}"
    );
    // schema.org ClaimReview/Claim bridge.
    assert!(
        ttl.contains("schema:ClaimReview") || ttl.contains("schema.org/ClaimReview"),
        "package.ttl must carry a schema:ClaimReview node:\n{ttl}"
    );
    assert!(
        ttl.contains("schema:Claim") || ttl.contains("schema.org/Claim"),
        "package.ttl must carry a schema:Claim node:\n{ttl}"
    );
    assert!(
        ttl.contains("reviewRating"),
        "ClaimReview must carry the verdict status as schema:reviewRating:\n{ttl}"
    );
}
