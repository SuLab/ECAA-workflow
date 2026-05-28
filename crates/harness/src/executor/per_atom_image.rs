//! Per-atom isolated derived images.
//!
//! When `ECAA_PER_TASK_IMAGES=1`, each task's atom gets its own image
//! built from JUST that atom's `runtime_packages`. The alternative
//! union-build path (`LocalExecutor::warm_runtime_image`) bakes one
//! image per session from every reachable atom's union.
//!
//! Flow per task:
//!
//! 1. Read `<package>/policies/atom-prereqs/<atom_id>.json`. Missing
//!    file or non-buildable manifest ⇒ `Ok(None)` — caller falls back
//!    to host mode or `atom.preferred_container.image`.
//! 2. Hash the on-disk bytes via `derived_image::content_hash_from_file`
//!    (matches the builder script's `sha256sum`); tag becomes
//!    `<ECAA_DERIVED_IMAGE_TAG_PREFIX>:<hash>`.
//! 3. Stage a temp build dir at `~/.ecaa-workflow/per-atom-builds/
//! <hash>/` containing `policies/runtime-prereqs.json` (the
//!    per-atom manifest under the legacy filename the builder script
//!    already reads) + a rendered `runtime/derived-image.Dockerfile`.
//! 4. Invoke `scripts/build-derived-image.sh` against the build dir.
//!    Cache hits exit 0 fast; cold builds bake the image into the
//!    local registry.
//! 5. Return `Ok(Some(tag))` on success; non-recoverable build
//!    failures bubble up as `Err`.
//!
//! Determinism: identical per-atom manifests yield identical hashes
//! across hosts, so two tasks sharing an atom share the image cache
//! entry (one build, two envelope entries).

use anyhow::{Context, Result};
use ecaa_workflow_core::runtime_prereqs::RuntimePrereqs;
use std::path::{Path, PathBuf};

/// Resolve the per-atom build root from env or the default
/// `~/.ecaa-workflow/per-atom-builds/`. Falls back to `/tmp` when
/// `HOME` is unset (CI containers occasionally).
fn build_root() -> PathBuf {
    if let Ok(v) = std::env::var("ECAA_PER_ATOM_BUILD_ROOT") {
        return PathBuf::from(v);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".ecaa-workflow/per-atom-builds")
}

/// Resolve the per-atom image tag prefix from env (default
/// `scripps-derived`). Mirrors `LocalExecutor::warm_runtime_image`'s
/// behavior so operators see the same prefix in `docker images`
/// regardless of which path produced the tag.
fn tag_prefix() -> String {
    std::env::var("ECAA_DERIVED_IMAGE_TAG_PREFIX").unwrap_or_else(|_| "scripps-derived".into())
}

/// Build (or cache-hit) the per-atom derived image for `atom_id`.
///
/// Returns:
/// - `Ok(Some(tag))` when an image exists or was built; caller stashes
///   the tag in `pending_envelope_additions[task.id]
/// ["ECAA_DEFAULT_CONTAINER_IMAGE"]`.
/// - `Ok(None)` when the atom has no install delta — no
///   `policies/atom-prereqs/<atom_id>.json`, or the manifest is
///   present but `is_buildable()` returns false. Caller falls back
///   to host mode or `atom.preferred_container.image`.
/// - `Err(_)` only on real build failures (docker daemon issues,
///   non-zero exit from the builder script). The harness logs and
///   continues with host mode in that case.
pub fn warm_per_atom_image(package_dir: &Path, atom_id: &str) -> Result<Option<String>> {
    // Atom_ids that flow into file paths must be canonical. The
    // emitter already validates ids at the write site (commit 1), but
    // defense-in-depth applies here too — a malformed atom_id in
    // WORKFLOW.json must refuse to dispatch rather than escape the
    // package dir.
    if atom_id.is_empty()
        || atom_id.contains('/')
        || atom_id.contains('\0')
        || atom_id.split('/').any(|s| s == "..")
    {
        return Err(anyhow::anyhow!(
            "per-atom image: invalid atom_id {:?}",
            atom_id
        ));
    }

    let manifest_path = package_dir
        .join("policies/atom-prereqs")
        .join(format!("{atom_id}.json"));
    if !manifest_path.exists() {
        return Ok(None);
    }
    let raw = ecaa_workflow_core::fs_helpers::read_to_string_ctx(&manifest_path)?;
    let prereqs: RuntimePrereqs = serde_json::from_str(&raw)
        .with_context(|| format!("parsing per-atom manifest at {}", manifest_path.display()))?;
    if !prereqs.is_buildable() {
        // Manifest present but no system delta — fall through to
        // host mode / atom.preferred_container.image.
        return Ok(None);
    }

    let hash = ecaa_workflow_core::derived_image::content_hash_from_file(&manifest_path)
        .with_context(|| format!("hashing per-atom manifest at {}", manifest_path.display()))?;
    let tag = format!("{}:{hash}", tag_prefix());

    // Stage the build directory. Same on-disk shape the builder
    // script expects: <root>/policies/runtime-prereqs.json + <root>/
    // runtime/derived-image.Dockerfile.
    let build_dir = build_root().join(&hash);
    std::fs::create_dir_all(build_dir.join("policies"))
        .with_context(|| format!("creating per-atom build dir at {}", build_dir.display()))?;
    std::fs::create_dir_all(build_dir.join("runtime"))
        .with_context(|| format!("creating per-atom runtime/ at {}", build_dir.display()))?;
    std::fs::write(build_dir.join("policies/runtime-prereqs.json"), &raw).with_context(|| {
        format!(
            "writing per-atom manifest into build dir at {}",
            build_dir.display()
        )
    })?;
    let dockerfile = ecaa_workflow_core::derived_image::render_dockerfile(&prereqs)
        .context("rendering per-atom Dockerfile")?;
    std::fs::write(
        build_dir.join("runtime/derived-image.Dockerfile"),
        dockerfile,
    )
    .with_context(|| {
        format!(
            "writing per-atom Dockerfile into build dir at {}",
            build_dir.display()
        )
    })?;

    // The install-proxy shims live alongside the
    // Dockerfile so its `COPY runtime/install-proxy/...` directives
    // resolve. Reuse what the emitter already staged in the package's
    // runtime/install-proxy/ tree (commit 1 of the safety policy +
    // every regular package emit). If the source tree is absent
    // (legacy package), skip the copy — the Dockerfile's COPY will
    // fail loudly at build time, which is the right surface for
    // the operator since it points at a malformed package.
    let proxy_src = package_dir.join("runtime/install-proxy");
    if proxy_src.is_dir() {
        let proxy_dst = build_dir.join("runtime/install-proxy");
        let _ = std::fs::remove_dir_all(&proxy_dst); // idempotent re-runs
        copy_dir_recursive(&proxy_src, &proxy_dst).with_context(|| {
            format!(
                "copying install-proxy tree into per-atom build dir at {}",
                build_dir.display()
            )
        })?;
    }

    // Invoke the builder. Honor the same env-var passthroughs the
    // session-wide `warm_runtime_image` does so operators don't need
    // a separate knob set for per-atom builds.
    let builder = std::env::var("ECAA_IMAGE_BUILDER_PATH")
        .unwrap_or_else(|_| "scripts/build-derived-image.sh".into());
    let mut cmd = std::process::Command::new(&builder);
    cmd.arg(&build_dir);
    for var in [
        "ECAA_FORCE_IMAGE_REBUILD",
        "ECAA_IMAGE_BUILD_TIMEOUT_SECS",
        "ECAA_BUILDX_CACHE_DIR",
        "ECAA_DERIVED_IMAGE_TAG_PREFIX",
        "ECAA_AGENT_CACHE_DIR",
    ] {
        if let Ok(v) = std::env::var(var) {
            cmd.env(var, v);
        }
    }
    let status = cmd
        .status()
        .with_context(|| format!("invoking image builder {}", builder))?;
    use super::builder_exit_codes as bx;
    match status.code() {
        Some(0) => Ok(Some(tag)),
        // NOT_BUILDABLE: manifest emptied / not buildable. Race-safe
        // with the pre-check above (manifest could mutate between
        // is_buildable and the script's own check). Treat as
        // host-mode fallback.
        Some(bx::NOT_BUILDABLE) => Ok(None),
        Some(bx::BUILD_FAILED) => Err(anyhow::anyhow!(
            "per-atom build failed for atom {atom_id} (tag {tag}) — see builder stderr"
        )),
        Some(bx::DOCKER_UNAVAILABLE) => Err(anyhow::anyhow!(
            "per-atom build skipped: docker daemon unreachable or jq missing \
             (atom {atom_id})"
        )),
        Some(c) => Err(anyhow::anyhow!(
            "per-atom builder exited with unexpected code {c} (atom {atom_id}, tag {tag})"
        )),
        None => Err(anyhow::anyhow!(
            "per-atom builder terminated by signal (atom {atom_id}, tag {tag})"
        )),
    }
}

/// Minimal recursive copy. Mirrors `copy_install_proxy` from the
/// emitter's surface — pulled into the harness here so the per-atom
/// path doesn't need to thread a core helper through.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if file_type.is_file() {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::runtime_prereqs::{RuntimePrereqs, SystemPackages};

    // Shared with `local::tests`. Serializes tests that mutate the
    // per-atom-image env vars across both modules so parallel
    // `cargo test` runs can't observe a stale env var another test
    // left briefly set. See `executor/mod.rs::ECAA_PER_TASK_IMAGE_ENV_LOCK`.
    use crate::executor::ECAA_PER_TASK_IMAGE_ENV_LOCK as ENV_LOCK;

    fn write_atom_manifest(dir: &Path, atom_id: &str, base: &str, apt: &[&str]) {
        let mut m = RuntimePrereqs::new();
        m.base_image = Some(base.into());
        m.system_packages = SystemPackages {
            apt: apt.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        };
        let path = dir
            .join("policies/atom-prereqs")
            .join(format!("{atom_id}.json"));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_string_pretty(&m).unwrap()).unwrap();
    }

    #[test]
    fn returns_none_when_manifest_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let res = warm_per_atom_image(tmp.path(), "missing_atom").unwrap();
        assert!(res.is_none(), "absent manifest must short-circuit to None");
    }

    #[test]
    fn returns_none_when_manifest_unbuildable() {
        // is_buildable() requires base_image AND at least one apt/dnf
        // entry. An empty manifest is on-disk but not buildable —
        // expected fall-through.
        let tmp = tempfile::tempdir().unwrap();
        let m = RuntimePrereqs::new();
        let p = tmp.path().join("policies/atom-prereqs/empty_atom.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, serde_json::to_string_pretty(&m).unwrap()).unwrap();
        let res = warm_per_atom_image(tmp.path(), "empty_atom").unwrap();
        assert!(res.is_none(), "unbuildable manifest must short-circuit");
    }

    #[test]
    fn returns_err_for_malformed_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("policies/atom-prereqs/bad.json");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "not json").unwrap();
        let res = warm_per_atom_image(tmp.path(), "bad");
        assert!(res.is_err(), "malformed manifest must surface an error");
    }

    #[test]
    fn refuses_invalid_atom_id() {
        let tmp = tempfile::tempdir().unwrap();
        for bad in &["", "../escape", "with/slash", "with\0null"] {
            let res = warm_per_atom_image(tmp.path(), bad);
            assert!(res.is_err(), "atom_id {bad:?} must refuse");
        }
    }

    /// Helper: write a mock builder script that:
    /// - asserts the staged build_dir contains the expected layout, and
    /// - exits with `exit_code`.
    ///
    /// Returns the script's path inside the supplied dir so the caller
    /// can drop the dir to clean up. `NamedTempFile` would hold an
    /// open writable handle while the test tries to `exec` it, which
    /// fails on Linux with `Text file busy (ETXTBSY)`. Writing into
    /// the temp dir + dropping the file handle before chmod sidesteps
    /// the issue.
    fn write_mock_builder(dir: &Path, exit_code: i32, check_layout: bool) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script_path = dir.join("mock-builder.sh");
        let body = if check_layout {
            format!(
                r#"#!/bin/sh
set -eu
dir="$1"
test -f "$dir/policies/runtime-prereqs.json" || {{ echo "manifest missing"; exit 1; }}
test -f "$dir/runtime/derived-image.Dockerfile" || {{ echo "Dockerfile missing"; exit 1; }}
exit {exit_code}
"#
            )
        } else {
            format!("#!/bin/sh\nexit {exit_code}\n")
        };
        std::fs::write(&script_path, body).unwrap();
        let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script_path, perms).unwrap();
        script_path
    }

    #[test]
    fn invokes_builder_with_staged_dir_on_buildable_manifest() {
        // End-to-end against a mock builder script that asserts the
        // staged build dir has the expected layout before exiting 0
        // (cache-hit simulation). Returns Ok(Some(tag)) with the
        // expected scripps-derived:<hash> shape.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pkg = tempfile::tempdir().unwrap();
        write_atom_manifest(pkg.path(), "atom_x", "ghcr.io/test/base:1", &["libfoo-dev"]);

        let build_root = tempfile::tempdir().unwrap();
        let scripts_dir = tempfile::tempdir().unwrap();
        let script_path = write_mock_builder(scripts_dir.path(), 0, true);

        std::env::set_var("ECAA_PER_ATOM_BUILD_ROOT", build_root.path());
        std::env::set_var("ECAA_IMAGE_BUILDER_PATH", &script_path);
        let res = warm_per_atom_image(pkg.path(), "atom_x");
        std::env::remove_var("ECAA_PER_ATOM_BUILD_ROOT");
        std::env::remove_var("ECAA_IMAGE_BUILDER_PATH");

        let tag = res.expect("builder should succeed").expect("Some(tag)");
        assert!(
            tag.starts_with("scripps-derived:"),
            "tag should use default prefix; got {tag}"
        );
        assert!(
            tag.len() > "scripps-derived:".len(),
            "tag must include hash; got {tag}"
        );
    }

    #[test]
    fn dedupes_identical_manifests_to_same_tag() {
        // Two atoms with identical runtime-prereqs bytes ⇒ identical
        // file-bytes hash ⇒ identical tag. The builder script handles
        // cache hits internally; this test just pins the tag
        // determinism contract.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let pkg = tempfile::tempdir().unwrap();
        write_atom_manifest(pkg.path(), "atom_a", "ghcr.io/test/base:1", &["pkg-one"]);
        write_atom_manifest(pkg.path(), "atom_b", "ghcr.io/test/base:1", &["pkg-one"]);

        let build_root = tempfile::tempdir().unwrap();
        let scripts_dir = tempfile::tempdir().unwrap();
        let script_path = write_mock_builder(scripts_dir.path(), 0, false);

        std::env::set_var("ECAA_PER_ATOM_BUILD_ROOT", build_root.path());
        std::env::set_var("ECAA_IMAGE_BUILDER_PATH", &script_path);
        let tag_a = warm_per_atom_image(pkg.path(), "atom_a")
            .unwrap()
            .expect("atom_a tag");
        let tag_b = warm_per_atom_image(pkg.path(), "atom_b")
            .unwrap()
            .expect("atom_b tag");
        std::env::remove_var("ECAA_PER_ATOM_BUILD_ROOT");
        std::env::remove_var("ECAA_IMAGE_BUILDER_PATH");

        assert_eq!(
            tag_a, tag_b,
            "two atoms with identical manifest bytes must share an image tag"
        );
    }
}
