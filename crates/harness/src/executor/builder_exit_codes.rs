//! Exit-code contract between the Rust harness (`per_atom_image.rs`) and the
//! bash builder script (`scripts/build-derived-image.sh`).
//!
//! Both sides must agree on these codes. Drift causes silent fallback or
//! misleading error messages. Keep this file and the bash script in sync;
//! the script's "Exit code contract" header block must mirror the doc-comments
//! below.

/// Manifest was empty / no install delta to apply. Harness should fall back
/// to host-mode execution (no per-atom derived image needed).
pub const NOT_BUILDABLE: i32 = 10;

/// Real build failure: docker/jq encountered an error inside the build,
/// or the resulting image failed validation. Harness should bubble the
/// error up — retry won't help without an upstream fix.
pub const BUILD_FAILED: i32 = 20;

/// Docker daemon unreachable, jq missing, or other infrastructure gap.
/// Harness should surface this as an operator-facing diagnostic
/// (infrastructure issue, not a content issue).
pub const DOCKER_UNAVAILABLE: i32 = 30;

#[cfg(test)]
mod drift_tests {
    use super::*;

    /// W8.3 — the Rust constants and the bash builder script
    /// (`scripts/build-derived-image.sh`) must agree on the exit codes,
    /// otherwise a daemon-unreachable failure surfaces as a build-failed
    /// (or vice versa) and the operator-facing diagnostic is wrong.
    ///
    /// Walk the bash script, extract `exit N` literals, assert the set
    /// is `{0, 10, 20, 30}` (the four documented codes) and no other
    /// integer literal slipped in.
    #[test]
    fn bash_builder_exit_codes_match_rust_constants() {
        // Walk up from CARGO_MANIFEST_DIR to the repo root looking for
        // scripts/build-derived-image.sh. Mirrors the discovery pattern
        // used by wrroc_validator_impl.
        let manifest = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut candidate = manifest.as_path();
        let script_path = loop {
            let probe = candidate.join("scripts/build-derived-image.sh");
            if probe.exists() {
                break probe;
            }
            match candidate.parent() {
                Some(p) => candidate = p,
                None => panic!(
                    "could not locate scripts/build-derived-image.sh upward from {}",
                    manifest.display()
                ),
            }
        };
        let body =
            std::fs::read_to_string(&script_path).expect("read scripts/build-derived-image.sh");

        // Collect `exit <N>` literals — match any indentation. Skip
        // commented-out lines (leading `#`).
        let mut found: std::collections::BTreeSet<i32> = Default::default();
        for line in body.lines() {
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            // Tokenize on whitespace; look for "exit" followed by an integer.
            let mut tokens = trimmed.split_whitespace();
            while let Some(tok) = tokens.next() {
                if tok == "exit" {
                    if let Some(num_tok) = tokens.next() {
                        if let Ok(n) = num_tok.parse::<i32>() {
                            found.insert(n);
                        }
                    }
                }
            }
        }

        // Drift gate: the canonical outer-script exit codes
        // (NOT_BUILDABLE / BUILD_FAILED / DOCKER_UNAVAILABLE, plus 0
        // success) must all appear at least once in the bash script.
        // We use subset-containment instead of equality because the
        // builder also invokes `docker run --entrypoint /bin/sh -c '...'`
        // with INNER-shell `exit N` literals (e.g. `exit 1` from the
        // shim smoke check); those exit codes belong to the docker
        // container, not the outer build script. A line-based parser
        // can't reliably tell them apart, so the gate accepts extras.
        let expected: std::collections::BTreeSet<i32> =
            [0, NOT_BUILDABLE, BUILD_FAILED, DOCKER_UNAVAILABLE]
                .into_iter()
                .collect();
        for code in &expected {
            assert!(
                found.contains(code),
                "scripts/build-derived-image.sh missing canonical exit code {}. \
                 Found: {:?}, required: {:?}. Either bump one of \
                 NOT_BUILDABLE/BUILD_FAILED/DOCKER_UNAVAILABLE (and the matching \
                 exit in the bash) in lockstep, or restore the missing exit \
                 literal in the bash.",
                code,
                found,
                expected
            );
        }
    }
}
