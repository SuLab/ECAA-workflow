//! Shared secret-key allowlists for env-var filtering across executors.
//!
//! W3.1 — Both `local.rs` and `aws/ssm.rs` previously maintained their own
//! independent `SECRET_KEYS = &[...]` constants. Drift between the two
//! (e.g. a new credential added to AWS but missed in local) is the kind
//! of bug that ships silently. This module hosts the canonical lists; the
//! two executors compose them via thin wrappers.
//!
//! Compile-time-asserted counts pin the lists to known shapes so an
//! accidental removal surfaces as a test failure.
//!
//! ## Semantics
//!
//! - `BASE_SECRET_KEYS`: credentials any executor must scrub from the
//!   subprocess env or refuse to ship to a remote envelope. Includes
//!   API keys, AWS access tokens, GitHub tokens, the harness server
//!   auth token, and the NCBI literature key.
//!
//! - `AWS_EXTRA_SECRET_KEYS`: AWS-specific credentials that the local
//!   executor doesn't normally see (GitHub PAT used by remote-build
//!   flows, HuggingFace token used by remote agents). The AWS SSM
//!   envelope filter consumes both lists; local only consumes the
//!   base.

/// W3.1 — credentials shared across every executor path. Both the local
/// executor's `env_clear` allowlist and the AWS SSM secret-filter use
/// this set as the floor. Pure data — adding a new well-known
/// credential here is always correct regardless of executor.
pub(super) const BASE_SECRET_KEYS: &[&str] = &[
    "ECAA_ANTHROPIC_API_KEY",
    "ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "ECAA_SERVER_AUTH_TOKEN",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "ECAA_LIT_NCBI_API_KEY",
];

/// W3.1 — AWS-only credentials that the local executor doesn't normally
/// see. The AWS SSM secret-filter unions this with `BASE_SECRET_KEYS`.
pub(super) const AWS_EXTRA_SECRET_KEYS: &[&str] = &["GITHUB_PERSONAL_ACCESS_TOKEN", "HF_TOKEN"];

/// W3.1 — full AWS list. Iterator chains `BASE_SECRET_KEYS` and
/// `AWS_EXTRA_SECRET_KEYS` so callers see a single virtual list and
/// drift can't silently appear between them.
pub(super) fn aws_secret_keys() -> impl Iterator<Item = &'static &'static str> {
    BASE_SECRET_KEYS.iter().chain(AWS_EXTRA_SECRET_KEYS.iter())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W3.1 — pin the BASE list length so a stealth removal of a
    /// credential (e.g. someone drops AWS_SESSION_TOKEN because "the
    /// instance profile handles it") fails the build instead of
    /// silently widening the leak surface. When a new credential is
    /// added intentionally, this number is bumped in the same commit
    /// — matching the `Tool::COUNT` precedent.
    #[test]
    fn base_secret_keys_count_is_pinned() {
        assert_eq!(
            BASE_SECRET_KEYS.len(),
            9,
            "BASE_SECRET_KEYS length changed; if intentional, bump this assertion in the same commit"
        );
    }

    /// W3.1 — same pin for the AWS-extra list (currently
    /// GITHUB_PERSONAL_ACCESS_TOKEN + HF_TOKEN).
    #[test]
    fn aws_extra_secret_keys_count_is_pinned() {
        assert_eq!(
            AWS_EXTRA_SECRET_KEYS.len(),
            2,
            "AWS_EXTRA_SECRET_KEYS length changed; if intentional, bump this assertion in the same commit"
        );
    }

    /// W3.1 — the AWS chain must include every base key plus the extras
    /// and nothing else. Defends against a future refactor that
    /// accidentally drops `chain(AWS_EXTRA_SECRET_KEYS)`.
    #[test]
    fn aws_secret_keys_chain_is_union() {
        let from_chain: Vec<&str> = aws_secret_keys().copied().collect();
        let mut expected: Vec<&str> = BASE_SECRET_KEYS.iter().copied().collect();
        expected.extend(AWS_EXTRA_SECRET_KEYS.iter().copied());
        assert_eq!(from_chain, expected);
    }
}
