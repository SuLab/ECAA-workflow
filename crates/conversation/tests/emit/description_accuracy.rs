//! D2 — the security-policy RO-Crate description must not overstate
//! what the emit path writes. It must NOT claim unqualified
//! "container image SHA-256 digests"; it MUST qualify digests as
//! content-hash / pinned-image only.

const RO_CRATE_SRC: &str = include_str!("../../src/emit/ro_crate.rs");

#[test]
fn security_policy_description_does_not_overstate_digests() {
    // The overstating phrase is removed.
    assert!(
        !RO_CRATE_SRC
            .contains("plus container image SHA-256 digests and any vulnerability-scan summary"),
        "security-policy description still overstates digest population"
    );
    // The softened, qualified phrasing is present.
    assert!(
        RO_CRATE_SRC.contains("content-hash digests"),
        "security-policy description must qualify digests as content-hash"
    );
}
