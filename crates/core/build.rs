//! Resolves the source git commit that produced this build and bakes it in
//! as the `ECAA_SOURCE_COMMIT` compile-time env var (read via
//! `env!("ECAA_SOURCE_COMMIT")`). Because it is a compile-time constant, every
//! package emitted by a given binary records the *same* commit — the emitted
//! RO-Crate stays byte-reproducible across repeated emits from that binary.
//!
//! Dependency-free (std + the `git` CLI only). Resolution priority:
//!   1. env `ECAA_SOURCE_COMMIT` (explicit override — e.g. a release pipeline),
//!   2. `git rev-parse --short=12 HEAD` (+ `-dirty` when the tree has
//!      uncommitted changes),
//!   3. `"unknown"` when git is unavailable or this is not a checkout.

use std::path::Path;
use std::process::Command;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".to_string());

    // Re-run when the explicit override changes.
    println!("cargo:rerun-if-env-changed=ECAA_SOURCE_COMMIT");

    // Best-effort freshness: re-run when the checked-out commit moves. Resolve
    // the HEAD file (worktree-aware via `--git-path`) and the ref HEAD points
    // at, so both a checkout switch and a new commit on the current branch
    // invalidate the cached value.
    if let Some(head) = git(&manifest_dir, &["rev-parse", "--git-path", "HEAD"]) {
        emit_rerun_if_exists(&manifest_dir, &head);
        if let Some(refname) = git(&manifest_dir, &["symbolic-ref", "--quiet", "HEAD"]) {
            if let Some(ref_path) = git(&manifest_dir, &["rev-parse", "--git-path", &refname]) {
                emit_rerun_if_exists(&manifest_dir, &ref_path);
            }
        }
    }

    let commit = resolve_commit(&manifest_dir);
    println!("cargo:rustc-env=ECAA_SOURCE_COMMIT={commit}");
}

/// Run `git -C <manifest_dir> <args>`; return trimmed stdout, or `None` on
/// non-zero exit, empty output, or a missing git binary.
fn git(manifest_dir: &str, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(manifest_dir)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Emit a `cargo:rerun-if-changed` line for `p` (resolved absolute against the
/// manifest dir when git returned a relative path) when the file exists.
fn emit_rerun_if_exists(manifest_dir: &str, p: &str) {
    let path = Path::new(p);
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        Path::new(manifest_dir).join(path)
    };
    if abs.exists() {
        println!("cargo:rerun-if-changed={}", abs.display());
    }
}

/// Resolve the source commit per the documented priority order.
fn resolve_commit(manifest_dir: &str) -> String {
    if let Ok(v) = std::env::var("ECAA_SOURCE_COMMIT") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return v;
        }
    }
    if let Some(sha) = git(manifest_dir, &["rev-parse", "--short=12", "HEAD"]) {
        let dirty = git(manifest_dir, &["status", "--porcelain"])
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);
        return if dirty { format!("{sha}-dirty") } else { sha };
    }
    "unknown".to_string()
}
