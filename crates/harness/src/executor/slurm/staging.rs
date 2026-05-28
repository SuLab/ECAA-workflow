//! Staging — push the emitted package onto the cluster's shared
//! filesystem before submitting jobs, and pull the `runtime/` dir back
//! after each task terminates. HPC clusters typically mount a shared
//! `/scratch` (NFS/Lustre/GPFS); we `rsync -a` the package there, let
//! the compute node read/write in place, and `rsync` results home.
//!
//! Paths on the cluster live under `<staging_dir>/<pkg_name>/`, where
//! `pkg_name` is the basename of the local package dir. The assumption
//! of one package per staging dir matches the AWS executor's `/workspace/pkg/`
//! convention.

use super::ssh::{RsyncDirection, SshSession};
use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};

/// Umask applied when staging Anthropic API credentials onto the remote SLURM
/// node. `0o077` strips group + world access from any file we create, so a
/// shared cluster filesystem can't expose the credential to other users.
/// Coupled with [`CREDENTIALS_FILE_PERMISSIONS`] below — both must change
/// together if relaxed.
const CREDENTIALS_FILE_UMASK: u32 = 0o077;

/// Mode bits applied via explicit `chmod` after credential file write.
/// `0o600` = `rw-------` for the owner only. Belt-and-suspenders with
/// [`CREDENTIALS_FILE_UMASK`] — a permissive default umask elsewhere in the
/// Remote shell can't undo this. Required by 02.
const CREDENTIALS_FILE_PERMISSIONS: u32 = 0o600;

/// Thin wrapper around an `SshSession` that adds package-aware path
/// bookkeeping. Doesn't hold the session itself — the executor owns
/// the `Box<dyn SshSession>` and passes it in. This keeps `Staging`
/// trivially testable without having to thread lifetimes.
pub struct Staging {
    staging_dir: PathBuf,
}

impl Staging {
    pub fn new(staging_dir: impl Into<PathBuf>) -> Self {
        Self {
            staging_dir: staging_dir.into(),
        }
    }

    /// Remote directory where the whole package lives.
    pub fn remote_pkg_dir(&self, package: &Path) -> Result<String> {
        let name = package
            .file_name()
            .ok_or_else(|| anyhow!("package path has no final component: {}", package.display()))?
            .to_string_lossy();
        Ok(format!(
            "{}/{name}",
            self.staging_dir.to_string_lossy().trim_end_matches('/')
        ))
    }

    /// Remote path of a file or subdir within the package.
    pub fn remote_path(&self, package: &Path, rel: &str) -> Result<String> {
        let base = self.remote_pkg_dir(package)?;
        let rel = rel.trim_start_matches('/');
        Ok(if rel.is_empty() {
            base
        } else {
            format!("{base}/{rel}")
        })
    }

    /// Push the full package tree to `<staging_dir>/<pkg>/`.
    ///
    /// Uses `--delete` so leftover state from a prior iteration on the
    /// cluster is removed — the rsync'd contents are authoritative.
    /// The trailing slash on the local source + bare remote target is
    /// intentional (rsync "copy directory contents" semantics).
    pub fn push_package(&self, ssh: &dyn SshSession, package: &Path) -> Result<()> {
        let local = format!("{}/", package.to_string_lossy().trim_end_matches('/'));
        let remote = self.remote_pkg_dir(package)?;
        ssh.run(&format!("mkdir -p {remote}"))
            .with_context(|| format!("preparing remote staging dir {remote}"))?;
        let outcome = ssh
            .rsync(RsyncDirection::Push, &local, &remote, &["--delete"])
            .with_context(|| format!("rsync push {local} → {remote}"))?;
        if !outcome.is_success() {
            return Err(anyhow!(
                "rsync push failed (exit {}): {}",
                outcome.exit_code,
                outcome.stderr
            ));
        }
        Ok(())
    }

    /// Pull the `runtime/` dir back from the cluster into the local
    /// package. Called after each task terminates — the harness reads
    /// `runtime/WORKFLOW.json` locally for state updates, so it must
    /// be in sync with what the remote agent wrote.
    ///
    /// `--delete` makes the pull deletion-symmetric with `push_package`
    /// (R-35). The cluster is authoritative for `runtime/` once a job
    /// has run there; a stale local file (e.g. a `runtime/scratch/<task>/`
    /// directory left over from a prior local-executor run, or a
    /// `runtime/.harness-paused` sentinel from a different harness
    /// generation) must not survive a pull. Without `--delete` the
    /// local tree drifts toward a union of every state observed —
    /// hard to reason about and a confusing source of stale fixtures
    /// in regression tests.
    pub fn pull_runtime(&self, ssh: &dyn SshSession, package: &Path) -> Result<()> {
        let remote = self.remote_path(package, "runtime/")?;
        let local = format!(
            "{}/runtime/",
            package.to_string_lossy().trim_end_matches('/')
        );
        // Ensure local runtime/ exists so rsync has a destination.
        std::fs::create_dir_all(package.join("runtime")).with_context(|| {
            format!(
                "creating local runtime dir {}",
                package.join("runtime").display()
            )
        })?;
        let outcome = ssh
            .rsync(RsyncDirection::Pull, &local, &remote, &["--delete"])
            .with_context(|| format!("rsync pull {remote} → {local}"))?;
        if !outcome.is_success() {
            return Err(anyhow!(
                "rsync pull failed (exit {}): {}",
                outcome.exit_code,
                outcome.stderr
            ));
        }
        Ok(())
    }

    /// Push a single auxiliary file (e.g. the agent wrapper script) to
    /// the package's remote dir. Idempotent — if the remote side
    /// already has the file with the same content rsync is a no-op.
    pub fn push_file(&self, ssh: &dyn SshSession, local: &Path, remote_rel: &str) -> Result<()> {
        let remote = self.remote_path(local, remote_rel)?;
        // The remote path is <staging_dir>/<package>/<rel>; but for
        // auxiliary files we often want it at <staging_dir>/<file>, so
        // callers pass the package path context separately. This
        // helper is intentionally conservative — it expects `local` to
        // already be a file inside the package tree.
        let outcome = ssh.rsync(RsyncDirection::Push, &local.to_string_lossy(), &remote, &[])?;
        if !outcome.is_success() {
            return Err(anyhow!(
                "rsync file push failed (exit {}): {}",
                outcome.exit_code,
                outcome.stderr
            ));
        }
        Ok(())
    }
}

/// Stage a per-job credentials file at
/// `<remote_pkg>/runtime/.creds-<job_id>.env` with mode 0600 over SSH.
/// Returns the remote path so the caller can `. <path>` from the sbatch
/// script body and `trap` cleanup on exit.
///
/// Why: putting `ANTHROPIC_API_KEY` (or any secret) into
/// `#SBATCH --export=` makes the value visible to other users on
/// shared clusters via `scontrol show job <id>` / `sacct --json`. The
/// shape `KEY=VALUE` lines in a 0600 file that the sbatch body sources
/// keeps the secret in the file's bytes (readable only to the job's
/// owner) and out of the SLURM control-plane metadata.
///
/// Encoding: each VALUE is base64-encoded and decoded inline on the
/// remote so values containing shell metacharacters (newlines, quotes,
/// `$`) round-trip cleanly without quote-escape gymnastics. KEYS are
/// validated to match `^[A-Z_][A-Z0-9_]*$` (the
/// `ecaa_workflow_core::env_validator::sanitize_lib_env_suffix`
/// rule) so a hostile key can't break out of the env-var assignment.
///
/// Atomic-ish: writes to a `.partial` sibling and renames into place
/// after `chmod 600`. A SIGKILL between rename and chmod leaves the
/// file at default mode briefly but ownership is still the job user;
/// the worst case is a sibling user on the login node reading a key
/// they couldn't otherwise read, which is the same threat the rename
/// is mitigating in the first place.
pub fn stage_credentials_file(
    ssh: &dyn SshSession,
    remote_pkg: &str,
    job_tag: &str,
    creds: &[(&str, &str)],
) -> Result<String> {
    use ecaa_workflow_core::env_validator;

    // Validate key shape STRICTLY (no normalization, unlike the
    // `sanitize_lib_env_suffix` permissive variant): credential keys
    // must already be canonical POSIX env names so we never have to
    // guess at the operator's intent. Empty creds list returns the
    // path anyway (the sbatch sourcer tolerates an empty file).
    // Also validate values — newlines / `=` / `\0` are refused.
    for (k, v) in creds {
        if !env_validator::is_valid_env_name(k) {
            return Err(anyhow!(
                "stage_credentials_file: invalid credential key {k:?} (must match ^[A-Z_][A-Z0-9_]*$)"
            ));
        }
        if !env_validator::is_safe_env_value(v) {
            return Err(anyhow!(
                "stage_credentials_file: credential value for {k} contains unsafe characters"
            ));
        }
    }

    // Build the file body — one `export KEY=$(printf '%s' VAL_B64 | base64 -d)` per cred.
    // base64 -d is present on every Linux cluster we care about (same
    // assumption as `submit_sbatch` in sbatch.rs).
    let mut body = String::new();
    body.push_str("# scripps-workflow per-job credentials. Sourced by sbatch body.\n");
    body.push_str("# DO NOT EDIT — regenerated each iteration.\n");
    for (k, v) in creds {
        let encoded = super::sbatch::base64_encode_public(v.as_bytes());
        body.push_str(&format!(
            "export {k}=\"$(printf '%s' '{encoded}' | base64 -d)\"\n"
        ));
    }

    // Write atomically: stage to `.partial`, chmod, then rename.
    let remote_path = format!("{remote_pkg}/runtime/.creds-{job_tag}.env");
    let staged = format!("{remote_path}.partial");
    let encoded_body = super::sbatch::base64_encode_public(body.as_bytes());
    let write_cmd = format!(
        "mkdir -p $(dirname {remote_path}) && \
         umask {CREDENTIALS_FILE_UMASK:03o} && \
         printf '%s' '{encoded_body}' | base64 -d > {staged} && \
         chmod {CREDENTIALS_FILE_PERMISSIONS:o} {staged} && \
         mv {staged} {remote_path}"
    );
    let outcome = ssh.run(&write_cmd)?;
    if !outcome.is_success() {
        return Err(anyhow!(
            "stage_credentials_file: writing {remote_path} failed (exit {}): {}",
            outcome.exit_code,
            outcome.stderr
        ));
    }
    Ok(remote_path)
}

#[cfg(test)]
mod tests {
    use super::super::ssh::{FakeSshSession, SshOutcome};
    use super::*;
    use tempfile::TempDir;

    fn mk_package() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Stub the minimum subdirs a real package has so the staging
        // code doesn't blow up on "package root must exist".
        std::fs::create_dir_all(dir.path().join("runtime")).unwrap();
        std::fs::write(dir.path().join("runtime/WORKFLOW.json"), b"{}").unwrap();
        dir
    }

    #[test]
    fn remote_pkg_dir_joins_staging_and_basename() {
        let s = Staging::new("/scratch/alan/scripps");
        let pkg = PathBuf::from("/tmp/my-pkg-abc123");
        assert_eq!(
            s.remote_pkg_dir(&pkg).unwrap(),
            "/scratch/alan/scripps/my-pkg-abc123"
        );
    }

    #[test]
    fn remote_pkg_dir_strips_trailing_slash_on_staging() {
        // Operators sometimes set ECAA_SLURM_STAGING_DIR with a
        // trailing slash; the resulting paths must not end up doubled.
        let s = Staging::new("/scratch/alan/scripps/");
        let pkg = PathBuf::from("/tmp/pkg");
        assert_eq!(s.remote_pkg_dir(&pkg).unwrap(), "/scratch/alan/scripps/pkg");
    }

    #[test]
    fn remote_path_handles_empty_and_leading_slash() {
        let s = Staging::new("/staging");
        let pkg = PathBuf::from("/tmp/pkg");
        assert_eq!(s.remote_path(&pkg, "").unwrap(), "/staging/pkg");
        assert_eq!(
            s.remote_path(&pkg, "runtime").unwrap(),
            "/staging/pkg/runtime"
        );
        assert_eq!(
            s.remote_path(&pkg, "/runtime").unwrap(),
            "/staging/pkg/runtime"
        );
        assert_eq!(
            s.remote_path(&pkg, "runtime/WORKFLOW.json").unwrap(),
            "/staging/pkg/runtime/WORKFLOW.json"
        );
    }

    #[test]
    fn remote_pkg_dir_errors_on_pathological_input() {
        let s = Staging::new("/staging");
        // A path with no basename is nonsensical as a package root.
        let bad = PathBuf::from("/");
        assert!(s.remote_pkg_dir(&bad).is_err());
    }

    #[test]
    fn push_package_rsyncs_with_delete_flag() {
        let pkg = mk_package();
        let s = Staging::new("/staging");
        let fake = FakeSshSession::new("cluster");
        // Stub the mkdir + rsync calls so they both succeed.
        fake.expect(
            format!(
                "mkdir -p /staging/{}",
                pkg.path().file_name().unwrap().to_string_lossy()
            ),
            SshOutcome::success(""),
        );
        let local_with_slash = format!("{}/", pkg.path().to_string_lossy().trim_end_matches('/'));
        let remote = format!(
            "/staging/{}",
            pkg.path().file_name().unwrap().to_string_lossy()
        );
        fake.expect_rsync(
            RsyncDirection::Push,
            &local_with_slash,
            &remote,
            SshOutcome::success("sent 1024 bytes"),
        );
        s.push_package(&fake, pkg.path())
            .expect("push must succeed");
        let calls = fake.calls();
        assert!(
            calls.iter().any(|c| c.starts_with("mkdir -p ")),
            "mkdir not called: {calls:?}"
        );
        assert!(
            calls.iter().any(|c| c.contains("--delete")),
            "rsync must pass --delete: {calls:?}"
        );
    }

    #[test]
    fn push_package_surfaces_rsync_nonzero_exit() {
        let pkg = mk_package();
        let s = Staging::new("/staging");
        let fake = FakeSshSession::new("cluster");
        fake.expect(
            format!(
                "mkdir -p /staging/{}",
                pkg.path().file_name().unwrap().to_string_lossy()
            ),
            SshOutcome::success(""),
        );
        let local_with_slash = format!("{}/", pkg.path().to_string_lossy().trim_end_matches('/'));
        let remote = format!(
            "/staging/{}",
            pkg.path().file_name().unwrap().to_string_lossy()
        );
        fake.expect_rsync(
            RsyncDirection::Push,
            &local_with_slash,
            &remote,
            SshOutcome::failure("permission denied", 23),
        );
        let err = s.push_package(&fake, pkg.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("rsync push failed"), "got: {msg}");
    }

    #[test]
    fn pull_runtime_creates_local_runtime_dir() {
        // Package root exists but runtime/ doesn't — pull_runtime must
        // create it so rsync's destination is valid.
        let pkg = TempDir::new().unwrap();
        assert!(!pkg.path().join("runtime").exists());
        let s = Staging::new("/staging");
        let fake = FakeSshSession::new("cluster");
        s.pull_runtime(&fake, pkg.path()).unwrap();
        assert!(pkg.path().join("runtime").is_dir());
    }

    #[test]
    fn pull_runtime_rsyncs_from_remote_runtime() {
        let pkg = mk_package();
        let s = Staging::new("/staging");
        let fake = FakeSshSession::new("cluster");
        s.pull_runtime(&fake, pkg.path()).unwrap();
        let calls = fake.calls();
        // Exactly one rsync call in pull direction.
        let pulls: Vec<_> = calls.iter().filter(|c| c.contains("pull")).collect();
        assert_eq!(pulls.len(), 1, "expected one pull, got {calls:?}");
        assert!(pulls[0].contains("runtime/"), "target must be runtime dir");
    }

    #[test]
    fn pull_runtime_propagates_rsync_failure() {
        let pkg = mk_package();
        let s = Staging::new("/staging");
        let fake = FakeSshSession::new("cluster");
        let local = format!(
            "{}/runtime/",
            pkg.path().to_string_lossy().trim_end_matches('/')
        );
        let remote = format!(
            "/staging/{}/runtime/",
            pkg.path().file_name().unwrap().to_string_lossy()
        );
        fake.expect_rsync(
            RsyncDirection::Pull,
            &local,
            &remote,
            SshOutcome::failure("connection lost", 12),
        );
        let err = s.pull_runtime(&fake, pkg.path()).unwrap_err();
        assert!(err.to_string().contains("rsync pull failed"));
    }

    // ── stage_credentials_file ─────

    #[test]
    fn stage_credentials_writes_to_expected_path() {
        let fake = FakeSshSession::new("cluster");
        let path = stage_credentials_file(
            &fake,
            "/scratch/u/scripps/pkg-abc",
            "task_42",
            &[("ANTHROPIC_API_KEY", "sk-ant-api03-XYZ")],
        )
        .expect("stage_credentials_file must succeed");
        assert_eq!(
            path,
            "/scratch/u/scripps/pkg-abc/runtime/.creds-task_42.env"
        );
    }

    #[test]
    fn stage_credentials_uses_umask_077_chmod_600_and_atomic_rename() {
        let fake = FakeSshSession::new("cluster");
        let _path = stage_credentials_file(
            &fake,
            "/scratch/u/scripps/pkg-xyz",
            "task_a",
            &[("HF_TOKEN", "hf_secret_token_value")],
        )
        .expect("stage_credentials_file must succeed");
        // Inspect the single composite command that was issued.
        let calls = fake.calls();
        assert_eq!(calls.len(), 1, "exactly one ssh.run call: {calls:?}");
        let cmd = &calls[0];
        // The umask, chmod, atomic rename — all required.
        assert!(cmd.contains("umask 077"), "missing umask 077: {cmd}");
        assert!(cmd.contains("chmod 600 "), "missing chmod 600: {cmd}");
        assert!(
            cmd.contains(".creds-task_a.env.partial"),
            "missing .partial staging: {cmd}"
        );
        assert!(
            cmd.contains("mv ") && cmd.contains(".creds-task_a.env"),
            "missing atomic rename: {cmd}"
        );
    }

    #[test]
    fn stage_credentials_does_not_echo_value_literally_in_command() {
        // The whole point of the helper: the cleartext key MUST NOT
        // appear in the shell command we send over the wire (a process
        // listing on the cluster login node could read ps output).
        // The base64-encoding pattern hides it.
        let fake = FakeSshSession::new("cluster");
        let secret = "sk-ant-api03-VERYSECRETLITERALSTRING";
        let _path = stage_credentials_file(
            &fake,
            "/scratch/u/scripps/pkg",
            "t1",
            &[("ANTHROPIC_API_KEY", secret)],
        )
        .expect("stage_credentials_file must succeed");
        let calls = fake.calls();
        let cmd = &calls[0];
        assert!(
            !cmd.contains(secret),
            "literal secret leaked into ssh command: {cmd}"
        );
        // Sanity: the base64 encoding of "sk-ant-api03-..." DOES appear
        // (so we know the helper is doing something).
        assert!(cmd.contains("base64 -d"), "base64 decode missing: {cmd}");
    }

    #[test]
    fn stage_credentials_rejects_invalid_key_shape() {
        let fake = FakeSshSession::new("cluster");
        let err = stage_credentials_file(
            &fake,
            "/scratch/u/scripps/pkg",
            "t1",
            // Lower-case + dash — not a valid env-var name.
            &[("my-secret-key", "value")],
        )
        .expect_err("invalid key shape must be rejected");
        assert!(
            err.to_string().contains("invalid credential key"),
            "wrong error: {err}"
        );
    }

    #[test]
    fn stage_credentials_surfaces_ssh_failure() {
        let mut fake = FakeSshSession::new("cluster");
        fake.set_default(SshOutcome::failure("disk full", 28));
        let err = stage_credentials_file(
            &fake,
            "/scratch/u/scripps/pkg",
            "t1",
            &[("ANTHROPIC_API_KEY", "sk-ant-api03-X")],
        )
        .expect_err("ssh failure must propagate");
        assert!(
            err.to_string().contains("stage_credentials_file"),
            "error must identify the helper: {err}"
        );
    }

    #[test]
    fn stage_credentials_with_empty_creds_still_returns_path() {
        // An empty cred list shouldn't error; the helper writes an
        // empty (header-only) file that the sbatch body sources
        // harmlessly.
        let fake = FakeSshSession::new("cluster");
        let path = stage_credentials_file(&fake, "/scratch/u/scripps/pkg", "t1", &[])
            .expect("empty creds list must succeed");
        assert_eq!(path, "/scratch/u/scripps/pkg/runtime/.creds-t1.env");
    }

    #[test]
    fn credential_file_mode_constants_are_locked_in() {
        // These values are security-load-bearing.
        // Changing them requires a security review.
        assert_eq!(CREDENTIALS_FILE_UMASK, 0o077);
        assert_eq!(CREDENTIALS_FILE_PERMISSIONS, 0o600);
    }
}
