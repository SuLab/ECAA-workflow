//! Store selection and image storage for env-snapshot.
//!
//! `select_store` is a pure function that decides where a built image should
//! go.  `store_image` carries out the actual docker operations.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::StoreLocation;

/// Decision produced by [`select_store`] — where the image will be sent.
#[derive(Debug, Clone, PartialEq)]
pub enum StorePlan {
    /// Push to an OCI registry; the caller supplies the registry base ref.
    Push { registry: String },
    /// Keep in a local content-addressed store under the buildx cache dir.
    LocalCas { dir: PathBuf },
}

/// Decide where to store the built image.
///
/// Pure (no side-effects, hermetically testable).
///
/// * `Some(reg)` → [`StorePlan::Push`] targeting `reg`.
/// * `None`      → [`StorePlan::LocalCas`] rooted at `<buildx_cache_dir>/cas`.
pub fn select_store(registry: Option<&str>, buildx_cache_dir: &Path) -> StorePlan {
    match registry {
        Some(reg) => StorePlan::Push {
            registry: reg.to_owned(),
        },
        None => StorePlan::LocalCas {
            dir: buildx_cache_dir.join("cas"),
        },
    }
}

/// Execute the store operation decided by `plan`.
///
/// # Registry path
///
/// Tags `local_tag` to `<registry>:<full-digest-hex>` (all 64 hex chars after
/// `sha256:`, or the full token if the prefix is absent), then pushes.
/// Returns `StoreLocation::Registry("<registry>@<full-digest>")` so that
/// replay can pull by digest (`docker pull <registry>@sha256:…`).
///
/// # LocalCas path  (durability rationale)
///
/// `docker save` exports the image to a self-contained tar archive at
/// `<dir>/<digest>.tar` (replacing `sha256:` prefix so the filename is
/// filesystem-safe).  This survives daemon prune and host reboots; replay
/// need only run `docker load -i <path>` before executing the container.
/// The alternative — relying on the daemon's in-memory image store keyed by
/// tag — is not durable across prune cycles, so tar is preferred here.
pub fn store_image(local_tag: &str, digest: &str, plan: &StorePlan) -> io::Result<StoreLocation> {
    match plan {
        StorePlan::Push { registry } => push_to_registry(local_tag, digest, registry),
        StorePlan::LocalCas { dir } => save_to_cas(local_tag, digest, dir),
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn push_to_registry(local_tag: &str, digest: &str, registry: &str) -> io::Result<StoreLocation> {
    // Derive a push tag from the full digest hex (all 64 chars after "sha256:").
    // Using the full hex avoids any collision between digests sharing a short prefix.
    let full_hex = full_digest_hex(digest);
    let remote_ref = format!("{}:{}", registry, full_hex);

    // Tag local image to the remote ref.
    let out = Command::new("docker")
        .args(["tag", local_tag, &remote_ref])
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker tag failed (exit {:?}): {} -> {}: {}",
            out.status.code(),
            local_tag,
            remote_ref,
            String::from_utf8_lossy(&out.stderr).trim_end()
        )));
    }

    // Push the remote ref.
    let status = Command::new("docker")
        .args(["push", &remote_ref])
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "docker push failed (exit {:?}): {}",
            status.code(),
            remote_ref
        )));
    }

    // Return the pull-by-digest reference so replay can do `docker pull
    // <registry>@sha256:…` unambiguously.
    let pull_ref = format!("{}@{}", registry, digest);
    Ok(StoreLocation::Registry(pull_ref))
}

fn save_to_cas(local_tag: &str, digest: &str, dir: &Path) -> io::Result<StoreLocation> {
    // Create the CAS directory if it does not exist.
    std::fs::create_dir_all(dir)?;

    // Build a filesystem-safe filename from the digest.
    // "sha256:abc123…" → "sha256-abc123….tar"
    let filename = digest.replace(':', "-") + ".tar";
    let tar_path = dir.join(&filename);

    // Export the image to a tar archive.  This is durable across daemon prune.
    // Pass &tar_path directly (AsRef<OsStr>) to avoid corrupting non-UTF-8 paths.
    let out = Command::new("docker")
        .args(["save", local_tag, "-o"])
        .arg(&tar_path)
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other(format!(
            "docker save failed (exit {:?}): {} -> {}: {}",
            out.status.code(),
            local_tag,
            tar_path.display(),
            String::from_utf8_lossy(&out.stderr).trim_end()
        )));
    }

    Ok(StoreLocation::LocalCas(tar_path))
}

/// Return the full hex portion of a digest, stripping any `sha256:` prefix.
/// All 64 hex chars are preserved so that push tags are globally unique.
fn full_digest_hex(digest: &str) -> &str {
    digest.strip_prefix("sha256:").unwrap_or(digest)
}

// ---------------------------------------------------------------------------
// Tests — pure select_store only (hermetic)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn registry_some_yields_push() {
        let cache_dir = PathBuf::from("/tmp/buildx-cache");
        let plan = select_store(Some("ghcr.io/x"), &cache_dir);
        assert_eq!(
            plan,
            StorePlan::Push {
                registry: "ghcr.io/x".to_owned()
            }
        );
    }

    #[test]
    fn registry_none_yields_local_cas_under_buildx_cache() {
        let cache_dir = PathBuf::from("/tmp/buildx-cache");
        let plan = select_store(None, &cache_dir);
        assert_eq!(
            plan,
            StorePlan::LocalCas {
                dir: PathBuf::from("/tmp/buildx-cache/cas")
            }
        );
    }

    #[test]
    fn full_digest_hex_strips_prefix_and_returns_all_64_chars() {
        let digest = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
        let hex = full_digest_hex(digest);
        assert_eq!(
            hex,
            "abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890"
        );
        assert_eq!(hex.len(), 64);
    }
}
