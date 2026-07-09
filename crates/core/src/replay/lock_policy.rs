// crates/core/src/replay/lock_policy.rs
//
// Registry-allowlist gating for the replay install-from-lock tier.
//
// Materializing `ExecEnv::InstallFromLock` runs `conda create --file <lock>`,
// which FETCHES every package URL listed in the recorded EXPLICIT lock. An
// imported (untrusted) package could ship an attacker-authored lock whose
// `@EXPLICIT` URLs point at arbitrary hosts, turning a "reproduce" click into
// a fetch from attacker-controlled infrastructure. This module parses those
// URLs and refuses any whose HOST is not on a registry allowlist, BEFORE any
// install (and therefore any network fetch) is attempted.
//
// The check is enforced regardless of package trust (defense-in-depth): even a
// locally-authored package's lock must resolve only to known conda registries.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::Path;

/// Default set of allowlisted registry HOSTS for install-from-lock.
///
/// conda-forge, bioconda, and the `defaults` channel all serve packages from
/// `conda.anaconda.org` (e.g. `https://conda.anaconda.org/conda-forge/…`);
/// `repo.anaconda.com` serves the `defaults` channel's `pkgs/main` tree;
/// `anaconda.org` is the parent host. Matching is on the URL host (exact or a
/// dotted subdomain suffix), so all three channel families resolve here.
pub const DEFAULT_LOCK_ALLOWLIST: &[&str] =
    &["conda.anaconda.org", "repo.anaconda.com", "anaconda.org"];

/// Why lock-registry validation refused (or could not run).
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockPolicyError {
    /// A package URL in the lock resolves to a host that is not allowlisted.
    OffAllowlist { url: String },
    /// The lock file could not be read.
    Unreadable(String),
}

impl fmt::Display for LockPolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockPolicyError::OffAllowlist { url } => write!(
                f,
                "lock package URL {url:?} resolves to a host that is not on the \
                 replay registry allowlist; refusing install-from-lock"
            ),
            LockPolicyError::Unreadable(msg) => {
                write!(f, "could not read conda lock for registry validation: {msg}")
            }
        }
    }
}

impl Error for LockPolicyError {}

/// Parse the package-download URLs from a conda EXPLICIT lock.
///
/// A conda `@EXPLICIT` lock is a header (`# platform:` comment + the
/// `@EXPLICIT` marker) followed by one fully-pinned `scheme://…#md5` URL per
/// package. This returns those URL lines (trimmed, with the `#md5` fragment
/// preserved — it is part of the recorded URL); comment / marker / blank lines
/// are ignored.
pub fn parse_lock_urls(lock: &Path) -> io::Result<Vec<String>> {
    let text = std::fs::read_to_string(lock)?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("https://") || l.starts_with("http://"))
        .map(|l| l.to_string())
        .collect())
}

/// Resolve the effective allowlist from a raw comma-separated env value,
/// falling back to [`DEFAULT_LOCK_ALLOWLIST`] when unset / empty. Shared by
/// [`crate::config::Config`] (the typed catalog entry) and `run_replay`
/// (runtime enforcement) so the two never diverge.
pub fn resolve_allowlist_from_env_value(raw: Option<&str>) -> Vec<String> {
    match raw {
        Some(s) if !s.trim().is_empty() => s
            .split(',')
            .map(|h| h.trim().to_string())
            .filter(|h| !h.is_empty())
            .collect(),
        _ => DEFAULT_LOCK_ALLOWLIST.iter().map(|s| s.to_string()).collect(),
    }
}

/// Extract the lowercased host of a package URL, or `None` when the string
/// does not parse as a `scheme://host/…` URL with a host component.
fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()?
        .host_str()
        .map(|h| h.to_ascii_lowercase())
}

/// `true` when `host` exactly equals an allowlist entry or is a dotted
/// subdomain of one (`conda.anaconda.org` is allowed by an `anaconda.org`
/// entry, but `anaconda.org.attacker.com` is not — the leading dot prevents
/// suffix-spoofing).
fn host_allowed(host: &str, allowlist: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    allowlist.iter().any(|a| {
        let a = a.trim().to_ascii_lowercase();
        !a.is_empty() && (host == a || host.ends_with(&format!(".{a}")))
    })
}

/// Validate that every package URL in `lock` resolves to an allowlisted host.
///
/// Fail-closed: a URL whose host cannot be parsed is treated as off-allowlist.
/// Returns on the FIRST violation so the caller can refuse the install without
/// scanning the whole file.
pub fn validate_lock_registries(lock: &Path, allowlist: &[String]) -> Result<(), LockPolicyError> {
    let urls = parse_lock_urls(lock)
        .map_err(|e| LockPolicyError::Unreadable(format!("{}: {e}", lock.display())))?;
    for url in urls {
        match host_of(&url) {
            Some(host) if host_allowed(&host, allowlist) => {}
            _ => return Err(LockPolicyError::OffAllowlist { url }),
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_lock(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("env.explicit.lock");
        fs::write(&p, body).unwrap();
        p
    }

    fn default_allowlist() -> Vec<String> {
        DEFAULT_LOCK_ALLOWLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn accepts_conda_forge_and_bioconda_urls() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = write_lock(
            tmp.path(),
            "# platform: linux-64\n@EXPLICIT\n\
             https://conda.anaconda.org/conda-forge/linux-64/numpy-1.26-py311.tar.bz2#aaaa\n\
             https://conda.anaconda.org/bioconda/linux-64/samtools-1.19.tar.bz2#bbbb\n\
             https://repo.anaconda.com/pkgs/main/linux-64/zlib-1.2.13.conda#cccc\n",
        );
        assert!(
            validate_lock_registries(&lock, &default_allowlist()).is_ok(),
            "conda-forge / bioconda / defaults URLs must pass the allowlist"
        );
    }

    #[test]
    fn rejects_off_allowlist_url() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = write_lock(
            tmp.path(),
            "@EXPLICIT\n\
             https://conda.anaconda.org/conda-forge/linux-64/numpy-1.26-py311.tar.bz2#aaaa\n\
             http://evil.example/x.tar.bz2#dead\n",
        );
        match validate_lock_registries(&lock, &default_allowlist()) {
            Err(LockPolicyError::OffAllowlist { url }) => {
                assert!(url.contains("evil.example"), "must name the offending URL: {url}");
            }
            other => panic!("expected OffAllowlist, got {other:?}"),
        }
    }

    #[test]
    fn unreadable_lock_is_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist.lock");
        assert!(matches!(
            validate_lock_registries(&missing, &default_allowlist()),
            Err(LockPolicyError::Unreadable(_))
        ));
    }

    #[test]
    fn parse_lock_urls_ignores_comments_and_markers() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = write_lock(
            tmp.path(),
            "# platform: linux-64\n@EXPLICIT\n\n\
             https://conda.anaconda.org/conda-forge/linux-64/a.tar.bz2#1\n",
        );
        let urls = parse_lock_urls(&lock).unwrap();
        assert_eq!(urls.len(), 1);
        assert!(urls[0].starts_with("https://conda.anaconda.org/"));
    }

    #[test]
    fn subdomain_suffix_match_does_not_spoof() {
        // `anaconda.org` entry allows a real subdomain but not a spoof host
        // that merely ends with the string "anaconda.org".
        let allow = vec!["anaconda.org".to_string()];
        assert!(host_allowed("conda.anaconda.org", &allow));
        assert!(host_allowed("anaconda.org", &allow));
        assert!(!host_allowed("anaconda.org.attacker.com", &allow));
        assert!(!host_allowed("evilanaconda.org", &allow));
    }

    #[test]
    fn resolve_allowlist_defaults_and_overrides() {
        assert_eq!(resolve_allowlist_from_env_value(None), default_allowlist());
        assert_eq!(resolve_allowlist_from_env_value(Some("   ")), default_allowlist());
        assert_eq!(
            resolve_allowlist_from_env_value(Some("a.example, b.example ,, c.example")),
            vec![
                "a.example".to_string(),
                "b.example".to_string(),
                "c.example".to_string()
            ]
        );
    }

    #[test]
    fn empty_lock_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = write_lock(tmp.path(), "# platform: linux-64\n@EXPLICIT\n");
        assert!(validate_lock_registries(&lock, &default_allowlist()).is_ok());
    }

    #[test]
    fn unparseable_url_is_rejected_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        // A line that passes the http:// prefix filter but has no host.
        let lock = write_lock(tmp.path(), "@EXPLICIT\nhttps:///no-host-here\n");
        assert!(matches!(
            validate_lock_registries(&lock, &default_allowlist()),
            Err(LockPolicyError::OffAllowlist { .. })
        ));
    }
}
