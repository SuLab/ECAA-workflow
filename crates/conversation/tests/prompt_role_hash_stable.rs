//! R4-LLM-2: Pin prompt_role.txt SHA-256 to catch unreviewed prose drift.
//!
//! The system-prompt role document is included in every chat session and
//! into every emitted package's audit log. Silent edits to its prose can
//! shift the LLM's tool-use behavior in subtle ways that none of the
//! integration fixtures cover. Pinning the SHA-256 forces every edit to
//! be reviewed and intentional: when the test fails, the assertion
//! message prints the new hash so the reviewer can copy it in once they
//! have approved the prose change.

use sha2::{Digest, Sha256};

#[test]
fn prompt_role_hash_pinned() {
    const PROMPT: &str = include_str!("../src/prompt_role.txt");
    let h = format!("{:x}", Sha256::digest(PROMPT.as_bytes()));
    assert_eq!(
        h, "8475e99e2f0cc92b6a822038c9a216c899c2d33f710e3f6a926ae20856d8d13b",
        "prompt_role.txt changed; if intentional, update hash to: {h}"
    );
}
