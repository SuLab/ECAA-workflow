//! Validation for env-var names and values that flow into shell-
//! interpolated commands.
//!
//! Security audit (closes C-8 SSM envelope-key RCE and C-9
//! SLURM `--export=` injection). The harness's AWS SSM and SLURM
//! executors compose env passthrough envelopes from caller-supplied
//! library identifiers and version strings. Without these validators a
//! library name like `foo; rm -rf /` becomes a literal shell statement
//! inside the run-command body or sbatch `--export=` directive; the
//! version value can break out via `,` (SLURM directive separator),
//! `\n` (sbatch parser), or `=` (rebind the parent env). The helpers
//! here reduce every caller value to a canonical, shell-safe form OR
//! refuse the call.
//!
//! The validators are deliberately strict — POSIX env-var names match
//! `^[A-Z_][A-Z0-9_]*$` and we mirror that exactly. Anything outside
//! this set risks either silent breakage (some runtimes ignore mixed-
//! case names) or active injection.
//!
//! These functions are sync, allocation-light, and have no I/O — they
//! live in `core` so both `harness` (sync) and `server` (async) can
//! reach them without dragging tokio across the boundary.

/// Returns true when `name` matches `^[A-Z_][A-Z0-9_]*$`. Mirrors POSIX
/// portable env-var naming + the constraint that bash `export NAME=val`
/// imposes on the left-hand side. An empty name is rejected because the
/// downstream interpolation would produce `=val` (which bash silently
/// drops, but downstream parsers may not).
pub fn is_valid_env_name(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    // `unwrap` is safe — we just checked `is_empty`.
    let first = chars.next().unwrap();
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Returns true when `value` contains no characters that would break
/// shell quoting, SLURM `--export=` directive parsing, or RFC-822-style
/// envelope encoding.
///
/// Refused characters:
/// - `\n` / `\r` — sbatch directive terminator
/// - `,` — sbatch `--export=` field separator
/// - `=` — the parent KEY=VALUE syntax (rebinds the env)
/// - `\0` — C-string terminator; no legitimate value should embed it
pub fn is_safe_env_value(value: &str) -> bool {
    !value
        .chars()
        .any(|c| matches!(c, '\n' | '\r' | ',' | '=' | '\0'))
}

/// Sanitize a library-name suffix into a valid env-var fragment, OR
/// return None if it cannot be safely represented.
///
/// Library identifiers in the wild (e.g. `scipy-1`, `numpy.linalg`)
/// already contain hyphens / dots which are not valid in env names; we
/// normalize those to underscores, uppercase the result, and then
/// re-validate via `is_valid_env_name`. Anything that still fails the
/// re-validation (shell metachars, whitespace, leading digits) yields
/// `None` — the caller MUST treat that as a hard refusal and skip the
/// passthrough entry.
pub fn sanitize_lib_env_suffix(library: &str) -> Option<String> {
    let upper = library.to_ascii_uppercase().replace(['-', '.'], "_");
    if is_valid_env_name(&upper) {
        Some(upper)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_shell_metacharacters_in_name() {
        assert!(!is_valid_env_name("FOO; rm -rf /"));
        assert!(!is_valid_env_name("FOO BAR"));
        assert!(!is_valid_env_name("FOO=BAR"));
        assert!(!is_valid_env_name("FOO`id`"));
        assert!(!is_valid_env_name("FOO$(id)"));
        assert!(!is_valid_env_name("FOO|sh"));
    }

    #[test]
    fn rejects_lowercase_and_leading_digit() {
        // POSIX names are uppercase; lowercase names work in bash but
        // break other shells AND look anomalous in env dumps. Reject.
        assert!(!is_valid_env_name("foo"));
        assert!(!is_valid_env_name("FooBar"));
        // Leading digit is illegal in every shell.
        assert!(!is_valid_env_name("1FOO"));
    }

    #[test]
    fn rejects_empty_name() {
        assert!(!is_valid_env_name(""));
    }

    #[test]
    fn accepts_canonical_names() {
        assert!(is_valid_env_name("FOO"));
        assert!(is_valid_env_name("FOO_BAR_42"));
        assert!(is_valid_env_name("_PRIVATE"));
        assert!(is_valid_env_name("SWFC_LIB_PIN_BLAS"));
    }

    #[test]
    fn rejects_newlines_and_commas_in_value() {
        assert!(!is_safe_env_value("foo\nbar"));
        assert!(!is_safe_env_value("foo\rbar"));
        assert!(!is_safe_env_value("foo,LD_PRELOAD=/x"));
        assert!(!is_safe_env_value("foo=bar"));
        assert!(!is_safe_env_value("foo\0bar"));
    }

    #[test]
    fn accepts_canonical_values() {
        // Versions, hashes, paths, semver — none of these include the
        // refused characters.
        assert!(is_safe_env_value("1.2.3"));
        assert!(is_safe_env_value("1.2.3-rc.4+build5"));
        assert!(is_safe_env_value("sha256:abcdef"));
        assert!(is_safe_env_value("/opt/conda/envs/bio"));
        assert!(is_safe_env_value("v4.6"));
    }

    #[test]
    fn library_sanitize_blocks_injection() {
        assert_eq!(sanitize_lib_env_suffix("blas"), Some("BLAS".to_string()));
        assert_eq!(
            sanitize_lib_env_suffix("scipy-1"),
            Some("SCIPY_1".to_string())
        );
        assert_eq!(
            sanitize_lib_env_suffix("numpy.linalg"),
            Some("NUMPY_LINALG".to_string())
        );
        assert_eq!(sanitize_lib_env_suffix("foo; curl evil"), None);
        assert_eq!(sanitize_lib_env_suffix("x\nrm"), None);
        assert_eq!(sanitize_lib_env_suffix("$(curl evil)"), None);
        assert_eq!(sanitize_lib_env_suffix(""), None);
        // Leading digit after uppercasing is still illegal.
        assert_eq!(sanitize_lib_env_suffix("3lib"), None);
    }
}
