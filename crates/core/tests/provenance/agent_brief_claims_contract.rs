//! The emitted agent brief mandates structured `result.json claims[]`
//! with `{claim, evidence}` for confirmatory stages, and explains that
//! omitting a Required expectation blocks the package.
#[test]
fn brief_mandates_structured_claims_with_evidence() {
    let brief = include_str!("../../templates/AGENT-EXECUTOR.md");
    // Names the required shape.
    assert!(
        brief.contains("evidence"),
        "brief must mention per-claim evidence"
    );
    // Names the confirmatory mandate + the blocking consequence so a
    // non-compliant agent's Required-absent gap is the agent's fault, not
    // a silent pass.
    assert!(
        brief.to_lowercase().contains("confirmatory"),
        "brief must call out the confirmatory-stage claims mandate"
    );
    assert!(
        brief.contains("expected-claim manifest") || brief.contains("ExpectedClaimManifest"),
        "brief must reference the recall manifest so the agent knows what to address"
    );
}
