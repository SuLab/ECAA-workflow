//! Deterministic snapshot Dockerfile generation and docker-buildx build wrapper.
//!
//! `render_snapshot_dockerfile` is a pure function (hermetically unit-tested).
//! `build_image` invokes docker and is exercised by the Task 8 integration test.

use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::env_snapshot::SnapshotOpts;

/// Render a deterministic Dockerfile that layers the conda, R, and Python
/// cache paths onto the base image at the same absolute paths they occupy at
/// build time.
///
/// The Dockerfile uses build-context-relative COPY sources (`conda-envs`,
/// `R-libs`, `python`) so the build context root must be `opts.cache_dir` —
/// its direct children are `conda-envs/`, `R-libs/`, and `python/`.
///
/// `base_digest` must be a fully-qualified digest (`sha256:<hex>`) so the
/// layer stack is pinned byte-for-byte.
pub fn render_snapshot_dockerfile(
    base_digest: &str,
    conda_envs_abs: &Path,
    r_libs_abs: &Path,
    python_userbase_abs: &Path,
) -> String {
    format!(
        "FROM {base}\n\
         COPY conda-envs {conda}\n\
         COPY R-libs {rlibs}\n\
         COPY python {python}\n\
         ENV CONDA_ENVS_DIRS={conda}\n\
         ENV R_LIBS_USER={rlibs}\n\
         ENV PYTHONUSERBASE={python}\n",
        base = base_digest,
        conda = conda_envs_abs.display(),
        rlibs = r_libs_abs.display(),
        python = python_userbase_abs.display(),
    )
}

/// Derive the local docker tag for a snapshot image from the base digest.
///
/// The tag is `ecaa-snapshot:<short>` where `<short>` is the first 12
/// characters of the hex portion of `base_digest` (stripping any `sha256:`
/// prefix).  Falls back to `"ecaa-snapshot:unknown"` when the hex portion is
/// shorter than 12 characters.
///
/// Pure (no side-effects, hermetically testable).
pub fn snapshot_image_tag(base_digest: &str) -> String {
    let short = base_digest
        .trim_start_matches("sha256:")
        .get(..12)
        .unwrap_or("unknown");
    format!("ecaa-snapshot:{short}")
}

/// True iff `s` is a bare image content digest (`sha256:<hex>` or a bare
/// `<hex>`), with no repository or tag.
///
/// Such a value is NOT a usable Dockerfile `FROM` reference: buildx resolves it
/// as `docker.io/library/<s>` and tries to pull. A value carrying a `/` (repo)
/// or `@` (repo@digest) is already usable and returns false.
///
/// Pure (no side-effects, hermetically testable).
pub fn is_bare_image_digest(s: &str) -> bool {
    if s.contains('/') || s.contains('@') {
        return false;
    }
    let hex = s.strip_prefix("sha256:").unwrap_or(s);
    !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve a recorded base reference into one usable in a Dockerfile `FROM`.
///
/// A bare `sha256:<hex>` config digest (what the determinism shim records as a
/// task's `task_container_digest`) is not FROM-able. When the base is bare, ask
/// the local daemon for a usable reference, preferring a portable `RepoDigest`
/// (`repo@sha256:<manifest-digest>`) and falling back to a `RepoTag`. The base
/// image is present locally (the agent's compute just ran in it). If nothing
/// resolves, return the input unchanged — the build then fails and the snapshot
/// degrades to the base digest, per the non-fatal contract.
fn resolve_base_from_ref(base: &str) -> String {
    if !is_bare_image_digest(base) {
        return base.to_owned();
    }
    for fmt in [
        "{{if .RepoDigests}}{{index .RepoDigests 0}}{{end}}",
        "{{if .RepoTags}}{{index .RepoTags 0}}{{end}}",
    ] {
        if let Ok(out) = Command::new("docker")
            .args(["image", "inspect", "--format", fmt, base])
            .output()
        {
            if out.status.success() {
                let resolved = String::from_utf8_lossy(&out.stdout).trim().to_owned();
                if !resolved.is_empty() {
                    return resolved;
                }
            }
        }
    }
    base.to_owned()
}

/// Resolve the bounded buildkit cache directory.
///
/// Mirrors the env-var chain used in `scripts/build-bio-min.sh` and
/// `scripts/build-derived-image.sh`:
///   1. `$ECAA_BUILDX_CACHE_DIR`
///   2. `$ECAA_AGENT_CACHE_DIR/buildkit`
///   3. `$HOME/.ecaa-workflow/agent-cache/buildkit`  (else `/tmp/…`)
///
/// Pure in the sense that it reads env vars but does not touch the filesystem.
pub fn resolve_buildx_cache_dir() -> PathBuf {
    std::env::var_os("ECAA_BUILDX_CACHE_DIR")
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
        })
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
    let tag = snapshot_image_tag(&opts.base_digest);

    let ctx = &opts.cache_dir;

    // Write the Dockerfile into the build context root.
    let conda_envs_abs = ctx.join("conda-envs");
    let r_libs_abs = ctx.join("R-libs");
    let python_abs = ctx.join("python");
    let dockerfile_path = ctx.join("Dockerfile.ecaa-snapshot");

    // Defensively ensure all three COPY-source dirs exist in the build context
    // so `COPY` never fails on a run that used only some toolchains (e.g., a
    // run that installed only Python packages and never touched conda or R).
    for d in ["conda-envs", "R-libs", "python"] {
        let _ = std::fs::create_dir_all(ctx.join(d));
    }

    // The recorded base is a bare `sha256:<hex>` config digest, which is NOT a
    // valid Dockerfile FROM: buildx resolves a bare digest as a remote
    // `docker.io/library/...` repo and tries to pull it. Translate it to a
    // FROM-able reference (RepoDigest, else RepoTag) via the local daemon —
    // the base image is present locally because the agent's compute just ran
    // in it.
    let from_ref = resolve_base_from_ref(&opts.base_digest);
    {
        let mut f = std::fs::File::create(&dockerfile_path)?;
        let content = render_snapshot_dockerfile(
            &from_ref,
            &conda_envs_abs,
            &r_libs_abs,
            &python_abs,
        );
        f.write_all(content.as_bytes())?;
    }

    // Resolve the bounded buildkit cache directory (same env-var chain as
    // scripts/build-bio-min.sh and scripts/build-derived-image.sh).
    let cache_dir: PathBuf = resolve_buildx_cache_dir();

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
                // Navigate to config.digest, mirroring the shell script's
                // `d["config"]["digest"]` (scripts/build-bio-min.sh ~line 75).
                // A multi-platform manifest has a "manifests" array whose
                // entries each carry a per-platform "digest"; we must skip
                // those and extract the config-block digest instead.
                if let Some(digest) = extract_config_digest(&text) {
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
    //
    // NOTE: The image Id is a LOCAL content identifier computed by the local
    // daemon.  It is NOT a registry-pullable content digest — a remote
    // `docker pull <image>@<Id>` will fail until the image has been pushed
    // and the registry has issued a proper repo digest.  Callers (Task 4)
    // must treat a tier-3 result as a same-host-only reference and re-resolve
    // the digest after the push completes.
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

/// Extract the `config.digest` string from a `docker manifest inspect` JSON
/// blob, mirroring `d["config"]["digest"]` in `scripts/build-bio-min.sh`.
///
/// A multi-platform manifest carries a `"manifests"` array whose entries each
/// have a `"digest"` field (per-platform manifest digest).  Calling
/// `extract_json_string_field(..., "digest")` on the full blob would return
/// the first such entry — wrong for multi-arch images.  This function instead
/// locates the `"config"` key first and then extracts `"digest"` from the
/// substring that follows, which corresponds to the image config block.
fn extract_config_digest(json: &str) -> Option<String> {
    let config_needle = "\"config\"";
    let config_pos = json.find(config_needle)?;
    let after_config = &json[config_pos + config_needle.len()..];
    extract_json_string_field(after_config, "digest")
}

#[cfg(test)]
mod tests {
    // S5.32: workspace lint is `unsafe_code = "deny"`. env mutations in tests
    // are single-process (nextest isolation); bounded waiver scoped to this mod.
    #![allow(unsafe_code)]
    use super::*;

    /// Verify that `extract_config_digest` returns the config-block digest and
    /// not a per-platform manifest digest from a multi-arch manifest list.
    #[test]
    fn config_digest_skips_manifest_list_entries() {
        // Multi-platform shape: "manifests" array entries each carry a
        // "digest" that is a per-platform manifest digest — NOT what we want.
        // The "config" block further down carries the image config digest.
        let multi_platform_json = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.docker.distribution.manifest.list.v2+json",
  "manifests": [
    {
      "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
      "size": 948,
      "digest": "sha256:MANIFEST_NOT_THIS",
      "platform": { "architecture": "amd64", "os": "linux" }
    },
    {
      "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
      "size": 942,
      "digest": "sha256:MANIFEST_NOT_THIS_EITHER",
      "platform": { "architecture": "arm64", "os": "linux" }
    }
  ],
  "config": {
    "mediaType": "application/vnd.docker.container.image.v1+json",
    "size": 7023,
    "digest": "sha256:CONFIG_THIS"
  }
}"#;
        assert_eq!(
            extract_config_digest(multi_platform_json),
            Some("sha256:CONFIG_THIS".to_owned()),
            "multi-platform: should return config.digest, not a manifests-array digest"
        );

        // Single-platform shape: no "manifests" array; only a "config" block.
        let single_platform_json = r#"{
  "schemaVersion": 2,
  "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
  "config": {
    "mediaType": "application/vnd.docker.container.image.v1+json",
    "size": 6791,
    "digest": "sha256:ONLY"
  },
  "layers": []
}"#;
        assert_eq!(
            extract_config_digest(single_platform_json),
            Some("sha256:ONLY".to_owned()),
            "single-platform: should return config.digest"
        );
    }

    // -----------------------------------------------------------------------
    // resolve_buildx_cache_dir tests
    //
    // Safety: nextest runs each test in its own process, so mutating process
    // env vars is sound — no other thread reads the env concurrently.
    // -----------------------------------------------------------------------

    /// (a) ECAA_BUILDX_CACHE_DIR set → returned verbatim.
    #[test]
    fn resolve_buildx_cache_dir_uses_buildx_env_var_when_set() {
        unsafe {
            std::env::set_var("ECAA_BUILDX_CACHE_DIR", "/explicit/buildx/cache");
        }
        let result = resolve_buildx_cache_dir();
        unsafe {
            std::env::remove_var("ECAA_BUILDX_CACHE_DIR");
        }
        assert_eq!(result, PathBuf::from("/explicit/buildx/cache"));
    }

    /// (b) Both ECAA_BUILDX_CACHE_DIR and ECAA_AGENT_CACHE_DIR unset, HOME set
    ///     → returns <HOME>/.ecaa-workflow/agent-cache/buildkit.
    #[test]
    fn resolve_buildx_cache_dir_falls_back_to_home_when_no_env_vars() {
        unsafe {
            std::env::remove_var("ECAA_BUILDX_CACHE_DIR");
            std::env::remove_var("ECAA_AGENT_CACHE_DIR");
            std::env::set_var("HOME", "/fake/home");
        }
        let result = resolve_buildx_cache_dir();
        unsafe {
            std::env::remove_var("HOME");
        }
        assert_eq!(
            result,
            PathBuf::from("/fake/home/.ecaa-workflow/agent-cache/buildkit")
        );
    }

    /// (c) ECAA_AGENT_CACHE_DIR set (but not BUILDX) → returns <that>/buildkit.
    #[test]
    fn resolve_buildx_cache_dir_uses_agent_cache_dir_with_buildkit_suffix() {
        unsafe {
            std::env::remove_var("ECAA_BUILDX_CACHE_DIR");
            std::env::set_var("ECAA_AGENT_CACHE_DIR", "/agent/cache");
        }
        let result = resolve_buildx_cache_dir();
        unsafe {
            std::env::remove_var("ECAA_AGENT_CACHE_DIR");
        }
        assert_eq!(result, PathBuf::from("/agent/cache/buildkit"));
    }

    #[test]
    fn snapshot_image_tag_strips_prefix_and_takes_12_chars() {
        // Standard sha256: prefix — first 12 hex chars after the prefix.
        assert_eq!(
            snapshot_image_tag("sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"),
            "ecaa-snapshot:abcdef012345"
        );
    }

    #[test]
    fn snapshot_image_tag_no_prefix_takes_12_chars() {
        // No sha256: prefix — first 12 chars of the raw string.
        assert_eq!(
            snapshot_image_tag("abcdef0123456789"),
            "ecaa-snapshot:abcdef012345"
        );
    }

    #[test]
    fn snapshot_image_tag_short_input_falls_back_to_unknown() {
        // Fewer than 12 hex chars after any prefix → "unknown".
        assert_eq!(snapshot_image_tag("sha256:abc"), "ecaa-snapshot:unknown");
        assert_eq!(snapshot_image_tag("short"), "ecaa-snapshot:unknown");
        assert_eq!(snapshot_image_tag(""), "ecaa-snapshot:unknown");
    }

    #[test]
    fn dockerfile_pins_base_copies_envs_at_same_path_and_sets_env() {
        let df = render_snapshot_dockerfile(
            "sha256:abc",
            std::path::Path::new("/cache/s1/conda-envs"),
            std::path::Path::new("/cache/s1/R-libs"),
            std::path::Path::new("/cache/s1/python"),
        );
        assert!(df.contains("FROM sha256:abc"));
        assert!(df.contains("COPY conda-envs /cache/s1/conda-envs"));
        assert!(df.contains("COPY R-libs /cache/s1/R-libs"));
        assert!(df.contains("COPY python /cache/s1/python"));
        assert!(df.contains("ENV CONDA_ENVS_DIRS=/cache/s1/conda-envs"));
        assert!(df.contains("ENV R_LIBS_USER=/cache/s1/R-libs"));
        assert!(df.contains("ENV PYTHONUSERBASE=/cache/s1/python"));
    }

    #[test]
    fn is_bare_image_digest_classifies_from_references() {
        // Bare config/content digests — NOT FROM-able (need resolution).
        assert!(is_bare_image_digest(
            "sha256:0809cab6067dae3fcef66b2d70685e9ba041ec0597f1d534b6981e40d35d0ef5"
        ));
        assert!(is_bare_image_digest(
            "0809cab6067dae3fcef66b2d70685e9ba041ec0597f1d534b6981e40d35d0ef5"
        ));
        // Already-usable FROM references — left untouched.
        assert!(!is_bare_image_digest("bio-min:local"));
        assert!(!is_bare_image_digest("bio-min@sha256:0809cab6"));
        assert!(!is_bare_image_digest(
            "ghcr.io/scripps/bio-min@sha256:0809cab6"
        ));
        assert!(!is_bare_image_digest("ghcr.io/scripps/bio-min:latest"));
        // Degenerate inputs are not bare digests.
        assert!(!is_bare_image_digest(""));
        assert!(!is_bare_image_digest("sha256:"));
    }
}
