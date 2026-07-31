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
///
/// `r_env_bin`, when present, is the absolute path of a captured conda
/// environment's `bin/` directory that contains an `Rscript` (the agent's
/// bioconductor env). Replay re-runs `.R` scripts via a bare `Rscript`
/// (`script_runner` → `interpreter_for`), NOT the agent's `conda run -n <env>`
/// wrapper, so unless that bare `Rscript` resolves to the env's R the recorded
/// R compute (DESeq2/vst) cannot reproduce. We install small wrappers at
/// `/usr/local/bin/{Rscript,R}` (first on the base PATH) that `exec` the env's
/// binaries by absolute path — so `argv[0]` stays inside the env and R_HOME is
/// resolved correctly. Only R is redirected: `python3` is deliberately left as
/// the base interpreter so the `.py` compute steps keep using `PYTHONUSERBASE`.
pub fn render_snapshot_dockerfile(
    base_digest: &str,
    conda_envs_abs: &Path,
    r_libs_abs: &Path,
    python_userbase_abs: &Path,
    r_env_bin: Option<&Path>,
    base_user: Option<&str>,
) -> String {
    let mut df = format!(
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
    );
    if let Some(bin) = r_env_bin {
        // The base image typically runs as a non-root user (e.g. `bio`) and
        // `/usr/local/bin` is root-owned, so the wrapper write needs root.
        // Replay overrides the user with `docker run --user`, but we still
        // restore the recorded base user so the image is otherwise unchanged.
        df.push_str("USER root\n");
        // Wrapper (not symlink): exec by the env's absolute path so conda R
        // resolves R_HOME relative to its real location, not /usr/local/bin.
        df.push_str(&format!(
            "RUN printf '#!/bin/sh\\nexec \"{bin}/Rscript\" \"$@\"\\n' > /usr/local/bin/Rscript \\\n\
             \x20&& printf '#!/bin/sh\\nexec \"{bin}/R\" \"$@\"\\n' > /usr/local/bin/R \\\n\
             \x20&& chmod +x /usr/local/bin/Rscript /usr/local/bin/R\n",
            bin = bin.display(),
        ));
        // The base image (e.g. bio-min) puts /opt/conda/bin AHEAD of
        // /usr/local/bin on PATH, so a bare `Rscript` would hit the base conda R
        // (no DESeq2) and bypass the wrapper above. Replay invokes the bare
        // interpreter (not a login shell), so prepend /usr/local/bin to PATH so
        // the wrapper wins. python3 is unaffected (no python wrapper installed).
        df.push_str("ENV PATH=/usr/local/bin:${PATH}\n");
        if let Some(user) = base_user.filter(|u| !u.is_empty()) {
            df.push_str(&format!("USER {user}\n"));
        }
    }
    df
}

/// Inspect the base image's configured `User` so the snapshot can restore it
/// after the root-only wrapper `RUN` step. Returns `None` when the daemon has
/// no answer or the field is empty (image defaults to root); the renderer then
/// emits no restoring `USER`.
fn resolve_base_user(base: &str) -> Option<String> {
    let out = Command::new("docker")
        .args(["image", "inspect", "--format", "{{.Config.User}}", base])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let user = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if user.is_empty() {
        None
    } else {
        Some(user)
    }
}

/// Find the `bin/` directory of a captured conda environment that provides an
/// `Rscript`, so the snapshot can route the bare `Rscript` interpreter to the
/// env's R (see [`render_snapshot_dockerfile`]).
///
/// `conda_envs_abs` is the build-context `conda-envs/` directory. We prefer the
/// `ecaa-install` bioconductor convention env (`ecaa-bioc`); failing that we
/// take the lexicographically-first env that contains `bin/Rscript`. Returns
/// `None` when no conda R env was captured (a Python-only run), in which case
/// no R wrapper is emitted.
fn find_r_env_bin(conda_envs_abs: &Path) -> Option<PathBuf> {
    let preferred = conda_envs_abs.join("ecaa-bioc").join("bin");
    if preferred.join("Rscript").is_file() {
        return Some(preferred);
    }
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(conda_envs_abs)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join("bin"))
        .filter(|bin| bin.join("Rscript").is_file())
        .collect();
    candidates.sort();
    candidates.into_iter().next()
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
    let r_env_bin = find_r_env_bin(&conda_envs_abs);
    // Only needed when an R wrapper RUN is emitted; inspect the original base.
    let base_user = if r_env_bin.is_some() {
        resolve_base_user(&opts.base_digest).or_else(|| resolve_base_user(&from_ref))
    } else {
        None
    };
    {
        let mut f = std::fs::File::create(&dockerfile_path)?;
        let content = render_snapshot_dockerfile(
            &from_ref,
            &conda_envs_abs,
            &r_libs_abs,
            &python_abs,
            r_env_bin.as_deref(),
            base_user.as_deref(),
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
                .arg(format!("type=local,dest={},mode=max", cache_dir.display()));
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
        return Err(io::Error::other(format!(
            "docker build failed with status: {status}"
        )));
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
            snapshot_image_tag(
                "sha256:abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
            ),
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
            None,
            None,
        );
        assert!(df.contains("FROM sha256:abc"));
        assert!(df.contains("COPY conda-envs /cache/s1/conda-envs"));
        assert!(df.contains("COPY R-libs /cache/s1/R-libs"));
        assert!(df.contains("COPY python /cache/s1/python"));
        assert!(df.contains("ENV CONDA_ENVS_DIRS=/cache/s1/conda-envs"));
        assert!(df.contains("ENV R_LIBS_USER=/cache/s1/R-libs"));
        assert!(df.contains("ENV PYTHONUSERBASE=/cache/s1/python"));
        // No conda R env → no Rscript wrapper, no USER juggling, python untouched.
        assert!(!df.contains("/usr/local/bin/Rscript"));
        assert!(!df.contains("USER root"));
    }

    #[test]
    fn dockerfile_routes_bare_rscript_to_conda_env_when_present() {
        // Replay runs `.R` scripts via a bare `Rscript`; when a conda R env was
        // captured, the snapshot must route that bare Rscript to the env's R so
        // the recorded DESeq2/vst compute reproduces. python3 must be untouched.
        let df = render_snapshot_dockerfile(
            "sha256:abc",
            std::path::Path::new("/cache/s1/conda-envs"),
            std::path::Path::new("/cache/s1/R-libs"),
            std::path::Path::new("/cache/s1/python"),
            Some(std::path::Path::new("/cache/s1/conda-envs/ecaa-bioc/bin")),
            Some("bio:bio"),
        );
        // Root for the wrapper write into root-owned /usr/local/bin, then the
        // recorded base user is restored.
        assert!(df.contains("USER root"));
        assert!(df.contains("USER bio:bio"));
        // /usr/local/bin must be prepended to PATH so the wrapper beats the
        // base image's /opt/conda/bin Rscript when replay invokes a bare Rscript.
        assert!(df.contains("ENV PATH=/usr/local/bin:${PATH}"));
        // Wrapper execs the env's Rscript/R by absolute path (R_HOME stays in env).
        assert!(df.contains("/usr/local/bin/Rscript"));
        assert!(df.contains("exec \"/cache/s1/conda-envs/ecaa-bioc/bin/Rscript\" \"$@\""));
        assert!(df.contains("exec \"/cache/s1/conda-envs/ecaa-bioc/bin/R\" \"$@\""));
        assert!(df.contains("chmod +x /usr/local/bin/Rscript /usr/local/bin/R"));
        // python3 is deliberately NOT redirected (keeps PYTHONUSERBASE steps working).
        assert!(!df.contains("/usr/local/bin/python"));
    }

    #[test]
    fn find_r_env_bin_prefers_ecaa_bioc_then_falls_back() {
        let t = tempfile::tempdir().unwrap();
        let envs = t.path().join("conda-envs");
        // A non-preferred env with Rscript + the preferred ecaa-bioc env.
        for (name, has_rscript) in [("aaa-env", true), ("ecaa-bioc", true)] {
            let bin = envs.join(name).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            if has_rscript {
                std::fs::write(bin.join("Rscript"), "x").unwrap();
            }
        }
        assert_eq!(
            find_r_env_bin(&envs),
            Some(envs.join("ecaa-bioc").join("bin"))
        );

        // Without ecaa-bioc, take the lexicographically-first env with Rscript.
        std::fs::remove_dir_all(envs.join("ecaa-bioc")).unwrap();
        assert_eq!(
            find_r_env_bin(&envs),
            Some(envs.join("aaa-env").join("bin"))
        );

        // Python-only run (no Rscript anywhere) → None.
        std::fs::remove_file(envs.join("aaa-env").join("bin").join("Rscript")).unwrap();
        assert_eq!(find_r_env_bin(&envs), None);
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
