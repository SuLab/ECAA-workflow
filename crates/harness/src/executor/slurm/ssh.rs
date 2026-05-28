//! SSH transport for the SLURM executor.
//!
//! The harness is sync (`core`/`cli`/`harness` must not depend on tokio —
//! CLAUDE.md architecture rule), so we shell out to the system `ssh` + `rsync`
//! binaries via `duct`, mirroring the AWS executor's shell-out-to-`aws`-CLI
//! pattern. The plan's original "openssh crate" sketch was adjusted in S-2
//! because that crate is tokio-based.
//!
//! `SshSession` is the test seam: a `SystemSshSession` hits the real ssh
//! binary, a `FakeSshSession` is an in-memory command-response map. Both
//! production and test code program against the trait, so callers in
//! `staging.rs` / `sbatch.rs` / `polling.rs` never branch on which one they
//! hold.
//!
//! ControlMaster is enabled by injecting `ControlMaster=auto` +
//! `ControlPath=<per-session tmp path>` on every invocation, so the first
//! `ssh` call opens the session and subsequent calls reuse it — same
//! multiplexing win as the `openssh` crate without the tokio tax.

use anyhow::{anyhow, Result};
use duct::cmd;
use std::path::PathBuf;

/// Outcome of a single remote command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshOutcome {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

impl SshOutcome {
    pub fn success(stdout: impl Into<String>) -> Self {
        Self {
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: 0,
        }
    }

    pub fn failure(stderr: impl Into<String>, exit_code: i32) -> Self {
        Self {
            stdout: String::new(),
            stderr: stderr.into(),
            exit_code,
        }
    }

    pub fn is_success(&self) -> bool {
        self.exit_code == 0
    }
}

/// Per-rsync direction. `Push` = local → remote, `Pull` = remote → local.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RsyncDirection {
    Push,
    Pull,
}

/// Contract every SSH transport implements. Trait-object safe so the
/// executor can hold `Box<dyn SshSession>`.
pub trait SshSession: Send + Sync {
    /// Run `command` on the remote host, returning its outcome. stdin is
    /// empty. Non-zero exit codes are reported via `SshOutcome.exit_code`
    /// rather than `Err`; `Err` is reserved for transport failures (ssh
    /// couldn't connect, network partition, timeout).
    fn run(&self, command: &str) -> Result<SshOutcome>;

    /// Rsync a local path to the remote host or vice versa.
    ///
    /// `extra_flags` is appended after the built-in `-a` (archive) flag
    /// so callers can add `--delete`, `--exclude`, etc. Paths must be
    /// bare filesystem paths — `rsync` sees the remote target wrapped
    /// as `<user>@<host>:<path>` under the hood.
    fn rsync(
        &self,
        direction: RsyncDirection,
        local: &str,
        remote: &str,
        extra_flags: &[&str],
    ) -> Result<SshOutcome>;

    /// Host identifier for logging / error messages.
    fn host(&self) -> &str;
}

/// Production `SshSession` that shells out to system `ssh` + `rsync` via
/// `duct`. Each `new()` allocates a per-session `ControlPath` so
/// successive calls reuse the same OpenSSH ControlMaster socket. The
/// socket directory is created under `std::env::temp_dir()` and removed
/// on `Drop`.
pub struct SystemSshSession {
    host: String,
    user: Option<String>,
    key_path: Option<PathBuf>,
    proxy_jump: Option<String>,
    /// Unique per-session path. ControlMaster writes its socket here
    /// and subsequent calls reuse it. Removed on Drop; if cleanup
    /// fails the socket path is ephemeral (under /tmp) so it eventually
    /// gets reaped by the OS.
    control_dir: PathBuf,
}

impl SystemSshSession {
    pub fn new(
        host: impl Into<String>,
        user: Option<String>,
        key_path: Option<PathBuf>,
        proxy_jump: Option<String>,
    ) -> Result<Self> {
        // uuid is already a harness dep (used for session IDs); reuse
        // it here so we don't add tempfile to production deps.
        let unique = uuid::Uuid::new_v4();
        let control_dir = std::env::temp_dir().join(format!("scripps-slurm-ssh-{unique}"));
        std::fs::create_dir_all(&control_dir).map_err(|e| {
            anyhow!(
                "failed to create ssh control dir {}: {e}",
                control_dir.display()
            )
        })?;
        Ok(Self {
            host: host.into(),
            user,
            key_path,
            proxy_jump,
            control_dir,
        })
    }

    fn control_path_arg(&self) -> String {
        // Fixed filename inside the session's unique dir — ControlMaster
        // reuses this socket across invocations within this session.
        format!("ControlPath={}", self.control_dir.join("cm.sock").display())
    }

    /// Assemble the ssh argv for `command`. `target` is the host spec
    /// (user@host or host).
    fn ssh_args(&self, command: &str) -> Vec<String> {
        let mut args = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            self.control_path_arg(),
            "-o".into(),
            "ControlPersist=10m".into(),
        ];
        if let Some(jump) = &self.proxy_jump {
            args.push("-J".into());
            args.push(jump.clone());
        }
        if let Some(key) = &self.key_path {
            args.push("-i".into());
            args.push(key.display().to_string());
        }
        args.push(self.target());
        args.push(command.into());
        args
    }

    fn target(&self) -> String {
        match &self.user {
            Some(u) => format!("{u}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

impl Drop for SystemSshSession {
    fn drop(&mut self) {
        // Best-effort cleanup. Failure is ignored — the path lives
        // under /tmp and will eventually be reaped by the OS.
        let _ = std::fs::remove_dir_all(&self.control_dir);
    }
}

impl SshSession for SystemSshSession {
    fn run(&self, command: &str) -> Result<SshOutcome> {
        let args = self.ssh_args(command);
        let expr = cmd("ssh", args)
            .stdout_capture()
            .stderr_capture()
            .unchecked();
        let output = expr
            .run()
            .map_err(|e| anyhow!("ssh transport failure against {}: {e}", self.host))?;
        Ok(SshOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn rsync(
        &self,
        direction: RsyncDirection,
        local: &str,
        remote: &str,
        extra_flags: &[&str],
    ) -> Result<SshOutcome> {
        // Build the ssh subcommand rsync uses for its transport. Must
        // carry the same ControlMaster hints so rsync reuses the
        // multiplexed session rather than opening its own TCP conn.
        let mut ssh_parts = vec![
            "ssh".to_string(),
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            self.control_path_arg(),
            "-o".into(),
            "ControlPersist=10m".into(),
        ];
        if let Some(jump) = &self.proxy_jump {
            ssh_parts.push("-J".into());
            ssh_parts.push(jump.clone());
        }
        if let Some(key) = &self.key_path {
            ssh_parts.push("-i".into());
            ssh_parts.push(key.display().to_string());
        }
        let ssh_command = ssh_parts.join(" ");

        let remote_spec = format!("{}:{remote}", self.target());
        let (src, dst) = match direction {
            RsyncDirection::Push => (local.to_string(), remote_spec),
            RsyncDirection::Pull => (remote_spec, local.to_string()),
        };

        let mut args: Vec<String> = vec!["-a".into(), "-e".into(), ssh_command];
        for flag in extra_flags {
            args.push((*flag).to_string());
        }
        args.push(src);
        args.push(dst);

        let expr = cmd("rsync", args)
            .stdout_capture()
            .stderr_capture()
            .unchecked();
        let output = expr
            .run()
            .map_err(|e| anyhow!("rsync transport failure against {}: {e}", self.host))?;
        Ok(SshOutcome {
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            exit_code: output.status.code().unwrap_or(-1),
        })
    }

    fn host(&self) -> &str {
        &self.host
    }
}

/// In-memory `SshSession` for tests. Callers populate a command-matcher
/// → response map; `run()` matches by exact command string, and rsync
/// calls look up on a synthesized key `rsync:<direction>:<local>:<remote>`.
/// Every invocation is recorded on an internal Vec so tests can assert
/// the order + count + arguments.
pub struct FakeSshSession {
    host: String,
    responses: std::sync::Mutex<std::collections::BTreeMap<String, SshOutcome>>,
    calls: std::sync::Mutex<Vec<String>>,
    default_response: SshOutcome,
}

impl FakeSshSession {
    pub fn new(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            responses: std::sync::Mutex::new(std::collections::BTreeMap::new()),
            calls: std::sync::Mutex::new(Vec::new()),
            default_response: SshOutcome::success(""),
        }
    }

    pub fn expect(&self, command: impl Into<String>, outcome: SshOutcome) {
        self.responses
            .lock()
            .unwrap()
            .insert(command.into(), outcome);
    }

    pub fn expect_rsync(
        &self,
        direction: RsyncDirection,
        local: &str,
        remote: &str,
        outcome: SshOutcome,
    ) {
        let key = rsync_key(direction, local, remote);
        self.responses.lock().unwrap().insert(key, outcome);
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub fn set_default(&mut self, outcome: SshOutcome) {
        self.default_response = outcome;
    }
}

fn rsync_key(direction: RsyncDirection, local: &str, remote: &str) -> String {
    let dir = match direction {
        RsyncDirection::Push => "push",
        RsyncDirection::Pull => "pull",
    };
    format!("rsync:{dir}:{local}:{remote}")
}

impl SshSession for FakeSshSession {
    fn run(&self, command: &str) -> Result<SshOutcome> {
        self.calls.lock().unwrap().push(command.to_string());
        let responses = self.responses.lock().unwrap();
        // Exact match first, then prefix match so tests can stub by
        // command family (e.g. "sbatch " matches "sbatch /path/script.sh").
        if let Some(out) = responses.get(command) {
            return Ok(out.clone());
        }
        for (pattern, out) in responses.iter() {
            if !pattern.starts_with("rsync:") && command.starts_with(pattern) {
                return Ok(out.clone());
            }
        }
        Ok(self.default_response.clone())
    }

    fn rsync(
        &self,
        direction: RsyncDirection,
        local: &str,
        remote: &str,
        extra_flags: &[&str],
    ) -> Result<SshOutcome> {
        let key = rsync_key(direction, local, remote);
        let trace = format!("{key} flags={}", extra_flags.join(","));
        self.calls.lock().unwrap().push(trace);
        let responses = self.responses.lock().unwrap();
        Ok(responses
            .get(&key)
            .cloned()
            .unwrap_or_else(|| self.default_response.clone()))
    }

    fn host(&self) -> &str {
        &self.host
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_success_and_failure_helpers() {
        let ok = SshOutcome::success("hello");
        assert!(ok.is_success());
        assert_eq!(ok.stdout, "hello");
        assert_eq!(ok.stderr, "");

        let fail = SshOutcome::failure("boom", 1);
        assert!(!fail.is_success());
        assert_eq!(fail.stderr, "boom");
        assert_eq!(fail.exit_code, 1);
    }

    #[test]
    fn fake_returns_exact_match_before_prefix_match() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("sbatch", SshOutcome::success("prefix-hit"));
        fake.expect("sbatch --parsable /x", SshOutcome::success("exact-hit"));
        let out = fake.run("sbatch --parsable /x").unwrap();
        assert_eq!(out.stdout, "exact-hit");
        let other = fake.run("sbatch --parsable /y").unwrap();
        assert_eq!(other.stdout, "prefix-hit");
    }

    #[test]
    fn fake_default_response_fires_on_no_match() {
        let fake = FakeSshSession::new("cluster");
        let out = fake.run("unknown").unwrap();
        assert!(out.is_success());
        assert_eq!(out.stdout, "");
    }

    #[test]
    fn fake_records_calls_in_order() {
        let fake = FakeSshSession::new("cluster");
        fake.expect("a", SshOutcome::success(""));
        fake.expect("b", SshOutcome::success(""));
        let _ = fake.run("a");
        let _ = fake.run("b");
        let _ = fake.run("b");
        assert_eq!(fake.calls(), vec!["a", "b", "b"]);
    }

    #[test]
    fn fake_rsync_matches_by_direction_and_paths() {
        let fake = FakeSshSession::new("cluster");
        fake.expect_rsync(
            RsyncDirection::Push,
            "/local/pkg",
            "/scratch/pkg",
            SshOutcome::success("1 file sent"),
        );
        let out = fake
            .rsync(
                RsyncDirection::Push,
                "/local/pkg",
                "/scratch/pkg",
                &["--delete"],
            )
            .unwrap();
        assert_eq!(out.stdout, "1 file sent");
        let call = &fake.calls()[0];
        assert!(call.contains("push"));
        assert!(call.contains("/local/pkg"));
        assert!(call.contains("/scratch/pkg"));
        assert!(call.contains("--delete"));
    }

    #[test]
    fn fake_pull_not_matched_by_push_expectation() {
        let fake = FakeSshSession::new("cluster");
        fake.expect_rsync(
            RsyncDirection::Push,
            "/local",
            "/remote",
            SshOutcome::success("push-only"),
        );
        let out = fake
            .rsync(RsyncDirection::Pull, "/local", "/remote", &[])
            .unwrap();
        // Pull falls through to default — direction is part of the key.
        assert_eq!(out.stdout, "");
    }

    #[test]
    fn system_session_builds_target_with_user() {
        let s =
            SystemSshSession::new("cluster.example.org", Some("alan".into()), None, None).unwrap();
        assert_eq!(s.target(), "alan@cluster.example.org");
        assert_eq!(s.host(), "cluster.example.org");
    }

    #[test]
    fn system_session_builds_target_without_user_defaults_to_host() {
        let s = SystemSshSession::new("cluster.example.org", None, None, None).unwrap();
        assert_eq!(s.target(), "cluster.example.org");
    }

    #[test]
    fn system_session_control_path_is_unique_per_session() {
        // Two sessions must not share a ControlMaster socket — otherwise
        // parallel executors in the same process step on each other.
        let s1 = SystemSshSession::new("h", None, None, None).unwrap();
        let s2 = SystemSshSession::new("h", None, None, None).unwrap();
        assert_ne!(s1.control_path_arg(), s2.control_path_arg());
        assert!(s1.control_path_arg().starts_with("ControlPath="));
    }

    #[test]
    fn system_session_ssh_args_carry_controlmaster_hints() {
        let s = SystemSshSession::new("h", Some("u".into()), None, None).unwrap();
        let args = s.ssh_args("sbatch /x");
        // Spot-check required options. ControlMaster, ControlPath, and
        // ControlPersist must all appear so the session actually multiplexes.
        let joined = args.join(" ");
        assert!(joined.contains("ControlMaster=auto"));
        assert!(joined.contains("ControlPath="));
        assert!(joined.contains("ControlPersist"));
        assert!(joined.contains("BatchMode=yes"));
        assert!(joined.contains("u@h"));
        assert!(args.last().unwrap() == "sbatch /x");
    }

    #[test]
    fn system_session_ssh_args_include_proxy_jump_when_configured() {
        let s = SystemSshSession::new("h", None, None, Some("bastion.example.org".into())).unwrap();
        let args = s.ssh_args("id");
        let joined = args.join(" ");
        assert!(joined.contains("-J bastion.example.org"));
    }

    #[test]
    fn system_session_ssh_args_include_key_when_configured() {
        let s = SystemSshSession::new("h", None, Some("/tmp/id_rsa".into()), None).unwrap();
        let args = s.ssh_args("id");
        let joined = args.join(" ");
        assert!(joined.contains("-i /tmp/id_rsa"));
    }

    #[test]
    fn trait_is_object_safe() {
        // Must be usable as Box<dyn SshSession> so staging.rs /
        // sbatch.rs / polling.rs can hold it uniformly.
        let _fake: Box<dyn SshSession> = Box::new(FakeSshSession::new("h"));
        let _sys: Box<dyn SshSession> =
            Box::new(SystemSshSession::new("h", None, None, None).unwrap());
    }
}
