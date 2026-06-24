//! Deterministic snapshot Dockerfile generation and docker-buildx build wrapper.
//!
//! `render_snapshot_dockerfile` is a pure function (hermetically unit-tested).
//! `build_image` invokes docker and is exercised by the Task 8 integration test.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::env_snapshot::SnapshotOpts;

/// Render a deterministic Dockerfile that layers the conda + R cache paths
/// onto the base image at the same absolute paths they occupy at build time.
///
/// The Dockerfile uses build-context-relative COPY sources (`conda-envs`,
/// `R-libs`) so the build context root must be `opts.cache_dir` — its
/// direct children are `conda-envs/` and `R-libs/`.
///
/// `base_digest` must be a fully-qualified digest (`sha256:<hex>`) so the
/// layer stack is pinned byte-for-byte.
pub fn render_snapshot_dockerfile(
    base_digest: &str,
    conda_envs_abs: &Path,
    r_libs_abs: &Path,
) -> String {
    format!(
        "FROM {base}\n\
         COPY conda-envs {conda}\n\
         COPY R-libs {rlibs}\n\
         ENV CONDA_ENVS_DIRS={conda}\n\
         ENV R_LIBS_USER={rlibs}\n",
        base = base_digest,
        conda = conda_envs_abs.display(),
        rlibs = r_libs_abs.display(),
    )
}

/// Build a content-addressed snapshot image from the assembled compute
/// environment cache and return its content digest (`sha256:<hex>`).
///
/// The build context is `opts.cache_dir` whose direct children must be:
///   `conda-envs/` — Conda environment trees
///   `R-libs/`     — R library trees
///
/// Mirrors the buildx pattern in `scripts/build-bio-min.sh`:
///   - prefer `docker buildx build --load` with a bounded local cache;
///   - fall back to plain `docker build` when buildx is absent.
///
/// `SOURCE_DATE_EPOCH` is forwarded as a `--build-arg` so OCI layer
/// timestamps are deterministic.
pub fn build_image(opts: &SnapshotOpts) -> io::Result<String> {
    // Derive a short tag from the base digest to label the local image.
    let short = opts
        .base_digest
        .trim_start_matches("sha256:")
        .get(..12)
        .unwrap_or("unknown");
    let tag = format!("ecaa-snapshot:{short}");

    let ctx = &opts.cache_dir;

    // Write the Dockerfile into the build context root.
    let conda_envs_abs = ctx.join("conda-envs");
    let r_libs_abs = ctx.join("R-libs");
    let dockerfile_path = ctx.join("Dockerfile.ecaa-snapshot");
    {
        let mut f = std::fs::File::create(&dockerfile_path)?;
        let content = render_snapshot_dockerfile(
            &opts.base_digest,
            &conda_envs_abs,
            &r_libs_abs,
        );
        f.write_all(content.as_bytes())?;
    }

    // Resolve the bounded buildkit cache directory (same env-var chain as
    // scripts/build-bio-min.sh and scripts/build-derived-image.sh).
    let cache_dir: PathBuf = std::env::var_os("ECAA_BUILDX_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("ECAA_AGENT_CACHE_DIR").map(|p| PathBuf::from(p).join("buildkit"))
        })
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".ecaa-workflow")
                .join("agent-cache")
                .join("buildkit")
        });

    let _ = std::fs::create_dir_all(&cache_dir);

    // Prefer buildx; fall back to plain docker build.
    let has_buildx = Command::new("docker")
        .args(["buildx", "version"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut cmd = Command::new("docker");
    if has_buildx {
        cmd.arg("buildx").arg("build").arg("--load");
        if cache_dir.is_dir() {
            cmd.arg("--cache-from")
                .arg(format!("type=local,src={}", cache_dir.display()))
                .arg("--cache-to")
                .arg(format!(
                    "type=local,dest={},mode=max",
                    cache_dir.display()
                ));
        }
    } else {
        cmd.arg("build");
    }

    cmd.arg("--tag")
        .arg(&tag)
        .arg("--file")
        .arg(&dockerfile_path)
        .arg("--build-arg")
        .arg(format!("SOURCE_DATE_EPOCH={}", opts.source_date_epoch))
        .arg(ctx);

    let status = cmd.status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("docker build failed with status: {status}"),
        ));
    }

    // Resolve the content digest. Prefer registry digest (post-push); fall
    // back to local image Id (pre-push, local-only).
    let digest = resolve_digest(&tag, opts.registry.as_deref())?;

    // Clean up the temporary Dockerfile.
    let _ = std::fs::remove_file(&dockerfile_path);

    Ok(digest)
}

/// Attempt to resolve the content digest for the named image.
///
/// Strategy mirrors `scripts/build-bio-min.sh`:
///  1. `docker manifest inspect` → registry-side config digest (post-push).
///  2. `docker image inspect .RepoDigests[0]` → repo digest (post-push local).
///  3. `docker image inspect .Id` → local sha256 (pre-push fallback).
fn resolve_digest(tag: &str, registry: Option<&str>) -> io::Result<String> {
    // 1. Registry-side manifest (only meaningful after push, and only when a
    //    registry is configured).
    if registry.is_some() {
        if let Ok(out) = Command::new("docker")
            .args(["manifest", "inspect", tag])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout);
                // Extract "digest": "sha256:<hex>" from the config block.
                if let Some(digest) = extract_json_string_field(&text, "digest") {
                    if digest.starts_with("sha256:") {
                        return Ok(digest);
                    }
                }
            }
        }
    }

    // 2. RepoDigests (available locally after push).
    let repo_out = Command::new("docker")
        .args([
            "image",
            "inspect",
            "--format",
            "{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}",
            tag,
        ])
        .output()?;
    if repo_out.status.success() {
        let repo_str = String::from_utf8_lossy(&repo_out.stdout);
        let repo_str = repo_str.trim();
        // Format is `<image>@sha256:<hex>`; extract after `@`.
        if let Some(pos) = repo_str.rfind('@') {
            let candidate = &repo_str[pos + 1..];
            if candidate.starts_with("sha256:") {
                return Ok(candidate.to_owned());
            }
        }
    }

    // 3. Local image Id (pre-push fallback).
    let id_out = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Id}}", tag])
        .output()?;
    if id_out.status.success() {
        let id = String::from_utf8_lossy(&id_out.stdout).trim().to_owned();
        if !id.is_empty() {
            return Ok(id);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("could not resolve content digest for image {tag}"),
    ))
}

/// Minimal JSON string-field extractor — avoids pulling in serde_json for
/// this narrow purpose.  Finds `"<field>": "<value>"` and returns `value`.
fn extract_json_string_field(json: &str, field: &str) -> Option<String> {
    let needle = format!("\"{field}\"");
    let pos = json.find(&needle)?;
    let after_key = &json[pos + needle.len()..];
    let colon_pos = after_key.find(':')?;
    let after_colon = after_key[colon_pos + 1..].trim_start();
    if !after_colon.starts_with('"') {
        return None;
    }
    let inner = &after_colon[1..];
    let end = inner.find('"')?;
    Some(inner[..end].to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockerfile_pins_base_copies_envs_at_same_path_and_sets_env() {
        let df = render_snapshot_dockerfile(
            "sha256:abc",
            std::path::Path::new("/cache/s1/conda-envs"),
            std::path::Path::new("/cache/s1/R-libs"),
        );
        assert!(df.contains("FROM sha256:abc"));
        assert!(df.contains("COPY conda-envs /cache/s1/conda-envs"));
        assert!(df.contains("COPY R-libs /cache/s1/R-libs"));
        assert!(df.contains("ENV CONDA_ENVS_DIRS=/cache/s1/conda-envs"));
        assert!(df.contains("ENV R_LIBS_USER=/cache/s1/R-libs"));
    }
}
