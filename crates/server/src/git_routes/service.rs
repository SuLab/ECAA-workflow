//! Wraps the system `git` CLI as a sync service. Every public method
//! shells out, blocks until the subprocess completes, and returns a
//! typed error on non-zero exit. Callers inside Axum handlers must
//! invoke through `tokio::task::spawn_blocking` to stay off the async
//! runtime's worker threads.
//!
//! Per-package shape (since): `GitService` is parameterized
//! over a `package_dir` rather than a single global `repo_path`. Each
//! emitted package gets its own `.git` directory; the service is a
//! thin wrapper that takes a path on every invocation. `hook_commit`
//! is the fire-and-forget commit entry point used by the chat routes;
//! it auto-inits the per-package repo on first use.

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;

use super::config::GitConfig;
use super::{CommitSummary, GenerateSshKeyRequest, GenerateSshKeyResponse};

/// Global mutex serializing all `git` invocations across the server
/// process. Prevents two emit flows from racing on the same working
/// tree (`git add` / `commit` / `push` is not concurrent-safe in a
/// single repo). The mutex is per-process, not per-repo — even with
/// the per-package shape, a single process never has more than a
/// handful of concurrent git invocations in flight, so the simpler
/// single-mutex design is preferred over a per-path registry.
static GIT_LOCK: Mutex<()> = Mutex::new(());

/// Thin wrapper around the system `git` binary scoped to a single package directory.
pub struct GitService {
    repo_path: PathBuf,
    remote_url: Option<String>,
    ssh_key_path: Option<PathBuf>,
    author_name: String,
    author_email: String,
    push_timeout_secs: u64,
}

/// Staged-files + subject for a `commit_and_maybe_push` call.
pub struct CommitInput {
    /// Commit subject line.
    pub subject: String,
    /// When empty ⇒ `git add -A`; otherwise `git add <paths>`.
    pub paths: Vec<String>,
}

/// One entry from `git log`.
#[derive(Debug, Serialize, Clone)]
pub struct LogEntry {
    /// Full commit SHA.
    pub sha: String,
    /// Commit subject line.
    pub subject: String,
    /// Unix seconds committer date.
    pub committed_at: u64,
}

impl GitService {
    /// Build a per-package service from the global GitConfig + the
    /// emitted package directory.
    pub fn for_package(cfg: &GitConfig, package_dir: &Path) -> Self {
        Self {
            repo_path: package_dir.to_path_buf(),
            remote_url: cfg.remote_url.clone(),
            ssh_key_path: cfg.ssh_key_path.as_deref().map(PathBuf::from),
            author_name: cfg.author_name.clone(),
            author_email: cfg.author_email.clone(),
            push_timeout_secs: cfg.push_timeout_secs,
        }
    }

    /// `git init` the package_dir + set user.name/email. Also
    /// adds the remote if one is configured and none exists yet. No-op
    /// on an already-initialized repo.
    pub fn init(&self) -> Result<()> {
        let _g = GIT_LOCK.lock().unwrap();
        std::fs::create_dir_all(&self.repo_path)
            .with_context(|| format!("mkdir {}", self.repo_path.display()))?;
        if !self.repo_path.join(".git").exists() {
            self.run_git(&["init"])?;
        }
        // Best-effort identity setup — harmless no-op when the values
        // match the existing `.git/config`.
        self.run_git(&["config", "user.name", &self.author_name])?;
        self.run_git(&["config", "user.email", &self.author_email])?;
        if let Some(remote) = &self.remote_url {
            self.set_remote_inner(remote)?;
        }
        Ok(())
    }

    /// Set or rewrite the `origin` remote on the per-package repo.
    /// Idempotent — succeeds whether origin already exists or not.
    /// Requires the repo to be initialized.
    pub fn set_remote(&self, remote_url: &str) -> Result<()> {
        let _g = GIT_LOCK.lock().unwrap();
        if !self.repo_path.join(".git").exists() {
            return Err(anyhow!(
                "cannot set remote on uninitialized repo at {}",
                self.repo_path.display()
            ));
        }
        self.set_remote_inner(remote_url)
    }

    /// Clear the `origin` remote on the per-package repo. No-op when
    /// no remote is configured. Caller already holds `GIT_LOCK`? — no,
    /// this acquires the lock itself.
    pub fn clear_remote(&self) -> Result<()> {
        let _g = GIT_LOCK.lock().unwrap();
        if !self.repo_path.join(".git").exists() {
            return Ok(());
        }
        // `remote remove` fails when origin doesn't exist — swallow.
        let _ = self.run_git(&["remote", "remove", "origin"]);
        Ok(())
    }

    /// `_inner` suffix signals: caller MUST already hold `GIT_LOCK`.
    /// Currently used by `init` and `set_remote`, both of which acquire
    /// the lock before delegating here. A future refactor that calls
    /// this without the lock would race two concurrent `init`/
    /// `set_remote` calls writing the same `.git/config` — the
    /// suffix is the only contract preventing it.
    ///
    /// Re-acquiring `GIT_LOCK` here is NOT a valid fix because the
    /// lock is `std::sync::Mutex` (non-reentrant): a second `lock()`
    /// on the same thread already-holding the lock would deadlock the
    /// process. The right answer when refactoring is to expose a
    /// public lock-acquiring shim and reserve `_inner` strictly for
    /// the lock-held path.
    fn set_remote_inner(&self, remote_url: &str) -> Result<()> {
        // Refuse URLs that would be re-parsed as
        // CLI flags by git. `git remote add origin -oProxyCommand=...`
        // hands the rest to the SSH driver as a flag. The downstream
        // `git remote add origin -- <url>` form prevents the flag
        // re-interpretation, but git still refuses a URL that begins
        // with `-`. Belt-and-suspenders: refuse here AND insert `--`.
        reject_dash_leading_url(remote_url)?;
        // `remote add origin` fails when origin exists; try
        // `remote set-url` to rewrite it, then ignore the
        // first-call failure.
        let add_argv = build_remote_add_argv(remote_url);
        let add_refs: Vec<&str> = add_argv.iter().map(String::as_str).collect();
        if self.run_git(&add_refs).is_err() {
            let set_argv = build_remote_set_url_argv(remote_url);
            let set_refs: Vec<&str> = set_argv.iter().map(String::as_str).collect();
            let _ = self.run_git(&set_refs);
        }
        Ok(())
    }

    /// Dry-run `git ls-remote` against the configured origin to verify
    /// auth + reachability. Returns Ok on zero-exit.
    pub fn test_remote(&self) -> Result<()> {
        let _g = GIT_LOCK.lock().unwrap();
        let remote = self
            .remote_url
            .as_deref()
            .ok_or_else(|| anyhow!("remote_url not configured"))?;
        // Refuse URLs that would be re-parsed
        // as CLI flags; insert `--` before the positional URL argument.
        reject_dash_leading_url(remote)?;
        // `ls-remote --exit-code` returns non-zero when the remote has
        // no refs; add `HEAD` as the pattern so empty remotes still
        // pass. Auth errors bubble through naturally.
        let argv = build_ls_remote_argv(remote);
        let argv_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        self.run_git(&argv_refs)
            .or_else(|e| {
                // Empty repo: `ls-remote` returns exit 2 "not found".
                // Treat as reachable — auth worked, the remote just
                // has no commits.
                if format!("{}", e).contains("exit code: 2") {
                    Ok(String::new())
                } else {
                    Err(e)
                }
            })
            .map(|_| ())
    }

    /// `(initialized, last_commit, dirty_count, commit_count)`.
    pub fn status(&self) -> Result<(bool, Option<CommitSummary>, u32, u64)> {
        let _g = GIT_LOCK.lock().unwrap();
        let initialized = self.repo_path.join(".git").exists();
        if !initialized {
            return Ok((false, None, 0, 0));
        }
        let dirty = self.run_git(&["status", "--porcelain"]).unwrap_or_default();
        let dirty_count = dirty.lines().filter(|l| !l.is_empty()).count() as u32;
        let commit_count = self
            .run_git(&["rev-list", "--count", "HEAD"])
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let last_commit = if commit_count > 0 {
            let line = self.run_git(&["log", "-1", "--format=%H%x1f%s%x1f%ct"])?;
            let mut parts = line.trim().splitn(3, '\x1f');
            let sha = parts.next().unwrap_or_default().to_string();
            let subject = parts.next().unwrap_or_default().to_string();
            let committed_at = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            Some(CommitSummary {
                sha,
                subject,
                committed_at,
            })
        } else {
            None
        };
        Ok((initialized, last_commit, dirty_count, commit_count))
    }

    /// Return the most recent `limit` log entries from the package repo.
    pub fn log(&self, limit: usize) -> Result<Vec<LogEntry>> {
        let _g = GIT_LOCK.lock().unwrap();
        let raw = self.run_git(&["log", "--format=%H%x1f%s%x1f%ct", &format!("-n{}", limit)])?;
        let mut out: Vec<LogEntry> = Vec::new();
        for line in raw.lines() {
            let mut parts = line.splitn(3, '\x1f');
            let sha = parts.next().unwrap_or("").to_string();
            let subject = parts.next().unwrap_or("").to_string();
            let committed_at = parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0);
            if !sha.is_empty() {
                out.push(LogEntry {
                    sha,
                    subject,
                    committed_at,
                });
            }
        }
        Ok(out)
    }

    /// Stage + commit + optional push. Returns the new commit sha and
    /// whether push was attempted. Pre-commit hooks are honored; they
    /// block the commit when they fail.
    pub fn commit_and_maybe_push(&self, input: &CommitInput, push: bool) -> Result<(String, bool)> {
        let _g = GIT_LOCK.lock().unwrap();
        // Validate commit paths first: every entry must canonicalize to
        // inside the repository's working tree. Otherwise `..`-bearing
        // paths in the commit body would let a caller stage arbitrary
        // files (the agent process owns the server's privileges).
        let _validated_paths = validate_commit_paths(&self.repo_path, &input.paths)
            .map_err(|e| anyhow!("invalid commit path: {e}"))?;
        if input.paths.is_empty() {
            self.run_git(&["add", "-A"])?;
        } else {
            let mut args = vec!["add", "--"];
            for p in &input.paths {
                args.push(p);
            }
            self.run_git(&args)?;
        }
        // `git diff --cached --quiet` returns exit 1 when there are
        // staged changes, 0 when clean. Flip the semantics.
        let dirty = Command::new("git")
            .arg("-C")
            .arg(&self.repo_path)
            .arg("diff")
            .arg("--cached")
            .arg("--quiet")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| "git diff --cached")?;
        if dirty.success() {
            return Err(anyhow!("no changes to commit"));
        }
        self.run_git(&["commit", "-m", &input.subject])?;
        let sha = self.run_git(&["rev-parse", "HEAD"])?.trim().to_string();
        let mut pushed = false;
        if push && self.remote_url.is_some() {
            self.push_inner()?;
            pushed = true;
        }
        Ok((sha, pushed))
    }

    /// `git push origin HEAD` with the configured SSH key, subject to
    /// `push_timeout_secs`.
    pub fn push(&self) -> Result<()> {
        let _g = GIT_LOCK.lock().unwrap();
        self.push_inner()
    }

    fn push_inner(&self) -> Result<()> {
        if self.remote_url.is_none() {
            return Err(anyhow!("no remote configured"));
        }
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo_path);
        cmd.arg("push").arg("origin").arg("HEAD");
        cmd.stdin(Stdio::null());
        if let Some(key) = &self.ssh_key_path {
            // shlex::quote escapes any shell metacharacter — git
            // invokes GIT_SSH_COMMAND via `sh -c` so unquoted
            // interpolation is a command-injection vector.
            // GitConfig::validate already rejects metacharacters in
            // ssh_key_path, but quoting here is defense-in-depth.
            let key_str = key.to_string_lossy();
            let quoted = shlex::try_quote(&key_str)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| String::new());
            cmd.env(
                "GIT_SSH_COMMAND",
                format!("ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes", quoted),
            );
        } else {
            // BatchMode prevents a hung push from waiting on an
            // interactive prompt. Users who need interactive auth can
            // run `git push` manually from a terminal.
            cmd.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .with_context(|| "spawning git push")?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(mut h) = stdout {
                let _ = h.read_to_string(&mut buf);
            }
            buf
        });
        let stderr_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(mut h) = stderr {
                let _ = h.read_to_string(&mut buf);
            }
            buf
        });
        // Poll + timeout loop. Kill on deadline.
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_secs(self.push_timeout_secs);
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        return Err(anyhow!(
                            "git push timed out after {}s",
                            self.push_timeout_secs
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            }
        };
        let _stdout = stdout_thread.join().unwrap_or_default();
        let stderr = stderr_thread.join().unwrap_or_default();
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("git push exited {}: {}", status, stderr.trim()))
        }
    }

    fn run_git(&self, args: &[&str]) -> Result<String> {
        // The surrounding
        // `_git_hook_pool::spawn` wraps the hook in
        // `tokio::time::timeout`, but a timeout there only stops
        // AWAITing the spawn_blocking JoinHandle — the underlying
        // `cmd.output()` blocking-pool thread continues until the
        // git binary itself returns. A jammed `.git/index.lock` or a
        // hung NFS mount on `package_dir` would pin a blocking-pool
        // slot indefinitely; with the pool's 8-slot cap that's 8
        // accumulating zombies per outage. So we enforce a per-call
        // wallclock budget here AT THE PROCESS LEVEL, killing the
        // git process when the deadline passes.
        //
        // Bounded to GIT_RUN_TIMEOUT (the same 30s the hook pool uses
        // for its outer timeout — when the OS-level kill fires the
        // hook pool's outer timeout also catches up and releases the
        // semaphore slot).
        self.run_git_with_timeout(args, GIT_RUN_TIMEOUT)
    }

    /// Spawn git as a child, poll `try_wait` until
    /// either it exits or `timeout` elapses, and `kill()` on timeout
    /// so the OS-level process actually goes away. Mirrors the
    /// pattern in `push_inner`. Returns the captured stdout on success.
    fn run_git_with_timeout(&self, args: &[&str], timeout: std::time::Duration) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(&self.repo_path);
        for a in args {
            cmd.arg(a);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        if let Some(key) = &self.ssh_key_path {
            let key_str = key.to_string_lossy();
            let quoted = shlex::try_quote(&key_str)
                .map(|c| c.into_owned())
                .unwrap_or_else(|_| String::new());
            cmd.env(
                "GIT_SSH_COMMAND",
                format!("ssh -i {} -o IdentitiesOnly=yes -o BatchMode=yes", quoted),
            );
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning git {}", args.join(" ")))?;

        // Drain pipes on background threads so a large stdout/stderr
        // doesn't fill its pipe buffer and false-deadlock the parent.
        // Same shape as `push_inner`.
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let stdout_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(mut h) = stdout {
                let _ = h.read_to_string(&mut buf);
            }
            buf
        });
        let stderr_thread = std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = String::new();
            if let Some(mut h) = stderr {
                let _ = h.read_to_string(&mut buf);
            }
            buf
        });

        let deadline = std::time::Instant::now() + timeout;
        let status = loop {
            match child.try_wait()? {
                Some(s) => break s,
                None => {
                    if std::time::Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = stdout_thread.join();
                        let _ = stderr_thread.join();
                        return Err(anyhow!(
                            "git {} timed out after {}s",
                            args.join(" "),
                            timeout.as_secs()
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
        };
        let stdout_buf = stdout_thread.join().unwrap_or_default();
        let stderr_buf = stderr_thread.join().unwrap_or_default();
        if !status.success() {
            return Err(anyhow!(
                "git {} (exit code: {}): {}",
                args.join(" "),
                status
                    .code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "?".into()),
                stderr_buf.trim()
            ));
        }
        Ok(stdout_buf)
    }
}

/// Per-`run_git` wallclock budget. 30s matches the
/// upstream `GitHookPool::new` deadline so the OS-level process kill
/// fires just-in-time before the spawn_blocking JoinHandle's outer
/// timeout. Picked to comfortably accommodate a slow remote push +
/// large staged diff while still bounding worst-case slot occupancy.
const GIT_RUN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Refuse remote URLs that start with `-` so
/// the SSH driver / git CLI can't be tricked into re-parsing them as
/// flags (e.g. `ssh://-oProxyCommand=...`). The `--` separator handles
/// most legitimate cases, but the SSH transport in particular can still
/// be coaxed into picking up flags from a URL that begins with a dash;
/// refusing at the edge keeps the SME away from that footgun entirely.
fn reject_dash_leading_url(url: &str) -> Result<()> {
    let trimmed = url.trim_start();
    if trimmed.starts_with('-') {
        return Err(anyhow!(
            "remote URL must not start with '-' (would be re-parsed as a CLI flag): {url:?}"
        ));
    }
    Ok(())
}

/// Build the argv for `git remote add origin <url>` with the
/// `--` separator that pins the URL as a positional argument. Returned
/// as `Vec<String>` so callers that need a `&[&str]` (for `run_git`)
/// can borrow into it.
pub(crate) fn build_remote_add_argv(url: &str) -> Vec<String> {
    vec![
        "remote".into(),
        "add".into(),
        "origin".into(),
        "--".into(),
        url.to_string(),
    ]
}

/// Build the argv for `git remote set-url origin <url>`
/// with the `--` separator. Same shape as `build_remote_add_argv`.
pub(crate) fn build_remote_set_url_argv(url: &str) -> Vec<String> {
    vec![
        "remote".into(),
        "set-url".into(),
        "origin".into(),
        "--".into(),
        url.to_string(),
    ]
}

/// Build the argv for `git ls-remote --exit-code <url> HEAD`
/// with the `--` separator so the URL stays positional.
pub(crate) fn build_ls_remote_argv(url: &str) -> Vec<String> {
    vec![
        "ls-remote".into(),
        "--exit-code".into(),
        "--".into(),
        url.to_string(),
        "HEAD".into(),
    ]
}

/// Is the `git` binary on PATH? Called by the status handler so the UI
/// can show an explicit "git is not installed" state instead of a
/// generic 500.
pub fn git_on_path() -> bool {
    Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Jail commit paths against the package root. Each path must be either
/// a relative path that resolves inside `pkg` after canonicalize, or an
/// absolute path under canonical `pkg`. Reject any `..` traversal in the
/// raw path BEFORE canonicalize (which would otherwise escape if `pkg`
/// is itself a symlink pointing elsewhere).
pub(crate) fn validate_commit_paths(
    pkg: &std::path::Path,
    paths: &[String],
) -> std::result::Result<Vec<PathBuf>, String> {
    let pkg_canon = std::fs::canonicalize(pkg).map_err(|e| format!("pkg canonicalize: {e}"))?;
    let mut out = Vec::with_capacity(paths.len());
    for p in paths {
        let candidate = if std::path::Path::new(p).is_absolute() {
            PathBuf::from(p)
        } else {
            pkg.join(p)
        };
        // Reject any ParentDir component BEFORE canonicalize — even if
        // canonicalize would resolve to a path under pkg, we don't want
        // commit paths embedding `..` segments because future refactors
        // might bypass the canonicalize step.
        if candidate
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("commit path {p} contains traversal"));
        }
        let canon = std::fs::canonicalize(&candidate)
            .map_err(|e| format!("commit path {p} canonicalize: {e}"))?;
        if !canon.starts_with(&pkg_canon) {
            return Err(format!("commit path {p} escapes package root"));
        }
        out.push(canon);
    }
    Ok(out)
}

/// Canonicalize the SSH-key target's parent against $HOME so a path like
/// `$HOME/../../etc/cron.hourly/pwn` is rejected. The starts_with check
/// must run on the canonicalized path; a `..` segment under $HOME would
/// otherwise escape.
pub(crate) fn validate_ssh_key_target(target: &str) -> std::result::Result<PathBuf, String> {
    let target = PathBuf::from(target);
    let home = PathBuf::from(std::env::var("HOME").map_err(|_| "HOME unset".to_string())?);
    let home_canon = std::fs::canonicalize(&home).map_err(|e| format!("HOME canonicalize: {e}"))?;
    let parent = target
        .parent()
        .ok_or_else(|| "target has no parent".to_string())?;
    // Parent must already exist for canonicalize; we create only after
    // confirming the canonicalized parent sits under canonical $HOME.
    let parent_canon = std::fs::canonicalize(parent)
        .map_err(|e| format!("parent must exist and be canonicalizable: {e}"))?;
    if !parent_canon.starts_with(&home_canon) {
        return Err(format!(
            "target parent {} escapes HOME",
            parent_canon.display()
        ));
    }
    let name = target
        .file_name()
        .ok_or_else(|| "target has no filename".to_string())?;
    let final_path = parent_canon.join(name);
    if final_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("traversal segment in final path".to_string());
    }
    Ok(final_path)
}

/// Security audit refuse ssh-keygen comments
/// that would be re-parsed as a CLI flag or contain control bytes that
/// break ssh-keygen's argument lexer.
///
/// `Command::arg` already prevents shell metacharacters from running,
/// but ssh-keygen's own getopt loop happily picks up a leading `-` as
/// a new option, and embedded NULs / newlines confuse downstream
/// parsers (e.g. the public-key file ends up multi-line or truncated).
///
/// Returns Err with an actionable message; callers should bubble it up
/// to the HTTP layer as a `400 Bad Request`.
fn validate_ssh_keygen_comment(comment: &str) -> std::result::Result<(), String> {
    if comment.is_empty() {
        return Err("ssh-keygen comment cannot be empty".to_string());
    }
    if comment.starts_with('-') {
        return Err(
            "ssh-keygen comment may not start with '-' (would be re-parsed as a flag)".to_string(),
        );
    }
    if comment.chars().any(|c| c == '\0' || c == '\n' || c == '\r') {
        return Err("ssh-keygen comment may not contain NUL or newline characters".to_string());
    }
    // Defense in depth: refuse comments longer than 256 bytes. Longer
    // values would force ssh-keygen to truncate or fail; 256 is plenty
    // for `scripps-workflow-<hostname>` style markers.
    if comment.len() > 256 {
        return Err(format!(
            "ssh-keygen comment too long ({} bytes; max 256)",
            comment.len()
        ));
    }
    Ok(())
}

/// `ssh-keygen -t ed25519 -f <path> -N "" -C "<comment>"`. Validates
/// that `<path>` is inside the user's $HOME before shelling out.
/// Returns the public key contents so the UI can display it.
pub fn generate_ssh_key(req: &GenerateSshKeyRequest) -> Result<GenerateSshKeyResponse> {
    let target = validate_ssh_key_target(&req.path).map_err(|e| anyhow!(e))?;
    if target.exists() {
        return Err(anyhow!(
            "file {} already exists; choose a different path",
            target.display()
        ));
    }
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let comment = req
        .comment
        .clone()
        .unwrap_or_else(|| format!("scripps-workflow-{}", hostname_or_unknown()));
    // Refuse comments that ssh-keygen would
    // re-parse as a flag (leading `-`) or that embed control bytes
    // which break ssh-keygen's argument parser. The `-C <comment>`
    // pair already uses `Command::arg` (no shell), but a value like
    // `-oProxyCommand=evil` or a value containing NUL still smuggles
    // commands through ssh-keygen's own getopt loop.
    validate_ssh_keygen_comment(&comment).map_err(|e| anyhow!(e))?;
    let out = Command::new("ssh-keygen")
        .arg("-t")
        .arg("ed25519")
        .arg("-f")
        .arg(&target)
        .arg("-N")
        .arg("")
        .arg("-C")
        .arg(comment)
        .stdin(Stdio::null())
        .output()
        .with_context(|| "spawning ssh-keygen")?;
    if !out.status.success() {
        return Err(anyhow!(
            "ssh-keygen exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let pub_path: PathBuf = {
        let mut p = target.clone().into_os_string();
        p.push(".pub");
        PathBuf::from(p)
    };
    let public_key = ecaa_workflow_core::fs_helpers::read_to_string_ctx(&pub_path)?;
    Ok(GenerateSshKeyResponse {
        private_key_path: target.to_string_lossy().to_string(),
        public_key: public_key.trim().to_string(),
    })
}

fn hostname_or_unknown() -> String {
    Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| "unknown".into())
}

/// Shared commit hook invoked from emit / amend / branch paths in the
/// chat routes. Respects `GitConfig.effective_enabled` and the
/// per-trigger flags. Fire-and-forget — never propagates errors, only
/// logs, so a git failure doesn't roll back an already-successful
/// emit.
///
/// `trigger` is one of "emit" | "amend" | "branch" | "task" — it
/// selects the per-trigger checkbox and prefixes the commit subject.
///
/// Per-package shape: `package_dir` is the emitted package directory.
/// On first invocation per package the hook auto-`git init`s the
/// directory; subsequent calls reuse the existing `.git`.
pub fn hook_commit(
    cfg: &GitConfig,
    package_dir: &Path,
    trigger: &str,
    subject_detail: &str,
    session_id: &str,
) {
    if !cfg.effective_enabled() {
        return;
    }
    let allowed = match trigger {
        "emit" => cfg.commit_on_emit,
        "amend" => cfg.commit_on_amend,
        "task" => cfg.commit_on_task_completed,
        "execution" => cfg.commit_on_task_completed,
        "branch" => true, // explicit user action
        _ => false,
    };
    if !allowed {
        return;
    }
    let subject = format!(
        "{}: {} [{}]",
        trigger,
        subject_detail,
        session_id.get(..8).unwrap_or(session_id)
    );
    let svc = GitService::for_package(cfg, package_dir);
    // Auto-init the per-package repo on first use. `init` is idempotent
    // when `.git` already exists; safe to call on every commit.
    if let Err(e) = svc.init() {
        eprintln!("[git] {} commit skipped: init failed: {}", trigger, e);
        return;
    }
    let input = CommitInput {
        subject,
        paths: vec![],
    };
    match svc.commit_and_maybe_push(&input, cfg.auto_push) {
        Ok((sha, pushed)) => {
            eprintln!(
                "[git] {} commit {} ({})",
                trigger,
                &sha[..sha.len().min(10)],
                if pushed { "pushed" } else { "local-only" }
            );
        }
        Err(e) => {
            // Likely "no changes to commit" (idempotent re-emit) or a
            // real git error. Either way: log and move on.
            eprintln!("[git] {} commit skipped: {}", trigger, e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enabled_cfg() -> GitConfig {
        GitConfig {
            enabled: true,
            author_name: "Test".into(),
            author_email: "test@example.com".into(),
            commit_on_emit: true,
            commit_on_amend: true,
            commit_on_task_completed: false,
            auto_push: false,
            push_timeout_secs: 5,
            ..GitConfig::default()
        }
    }

    #[test]
    fn init_and_commit_in_fresh_repo() {
        // git is required for the git-routes test surface — silently
        // skipping here would let a misconfigured environment ship
        // green tests that didn't run. Fail loudly instead.
        assert!(
            git_on_path(),
            "git must be on PATH to run git_routes::service tests"
        );
        let tmp = tempfile::tempdir().unwrap();
        let cfg = enabled_cfg();
        let svc = GitService::for_package(&cfg, tmp.path());
        svc.init().unwrap();
        std::fs::write(tmp.path().join("README"), b"hello\n").unwrap();
        let (sha, pushed) = svc
            .commit_and_maybe_push(
                &CommitInput {
                    subject: "initial".into(),
                    paths: vec![],
                },
                false,
            )
            .unwrap();
        assert!(!sha.is_empty());
        assert!(!pushed);
        let (init, last, dirty, count) = svc.status().unwrap();
        assert!(init);
        assert_eq!(count, 1);
        assert_eq!(dirty, 0);
        assert_eq!(last.unwrap().subject, "initial");
    }

    #[test]
    fn commit_fails_with_no_changes() {
        assert!(
            git_on_path(),
            "git must be on PATH to run git_routes::service tests"
        );
        let tmp = tempfile::tempdir().unwrap();
        let cfg = enabled_cfg();
        let svc = GitService::for_package(&cfg, tmp.path());
        svc.init().unwrap();
        let err = svc
            .commit_and_maybe_push(
                &CommitInput {
                    subject: "empty".into(),
                    paths: vec![],
                },
                false,
            )
            .unwrap_err();
        assert!(err.to_string().contains("no changes"));
    }

    /// Per-package shape regression: a fresh package directory with no
    /// `.git` yet must get one auto-initialized on the first
    /// `hook_commit` invocation. Plus an actual commit must land so the
    /// caller's "every emit produces a commit" contract holds.
    #[test]
    fn hook_commit_init_creates_git_repo() {
        if !git_on_path() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        // Seed a file so there's something to commit.
        std::fs::write(tmp.path().join("ro-crate-metadata.json"), b"{}\n").unwrap();
        let cfg = enabled_cfg();

        assert!(
            !tmp.path().join(".git").exists(),
            "pre-condition: .git must not yet exist"
        );
        hook_commit(&cfg, tmp.path(), "emit", "test package", "session-abcdef00");
        assert!(
            tmp.path().join(".git").exists(),
            ".git must be created on first hook_commit"
        );

        // Verify a commit.
        let svc = GitService::for_package(&cfg, tmp.path());
        let (initialized, last, _dirty, count) = svc.status().unwrap();
        assert!(initialized);
        assert_eq!(count, 1, "exactly one commit must have landed");
        let summary = last.expect("HEAD commit must exist");
        assert!(
            summary.subject.starts_with("emit: test package"),
            "commit subject must carry the trigger + detail; got {}",
            summary.subject
        );
        assert!(
            summary.subject.contains("session-"),
            "commit subject must include the truncated session id; got {}",
            summary.subject
        );
    }

    /// Second invocation on the same package_dir must not re-init —
    /// the repo's commit history must accumulate across emit hooks.
    #[test]
    fn hook_commit_uses_existing_git_repo() {
        if !git_on_path() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("file-1"), b"first\n").unwrap();
        let cfg = enabled_cfg();

        hook_commit(&cfg, tmp.path(), "emit", "first commit", "session-aaaaaaaa");
        let svc = GitService::for_package(&cfg, tmp.path());
        let (_, _, _, c1) = svc.status().unwrap();
        assert_eq!(c1, 1);

        // New file, second commit — must append to the existing repo.
        std::fs::write(tmp.path().join("file-2"), b"second\n").unwrap();
        hook_commit(
            &cfg,
            tmp.path(),
            "amend",
            "amend stage X",
            "session-aaaaaaaa",
        );
        let (_, last, _, c2) = svc.status().unwrap();
        assert_eq!(c2, 2, "second commit must accumulate on the existing repo");
        let summary = last.expect("HEAD commit must exist");
        assert!(
            summary.subject.starts_with("amend:"),
            "second commit must carry the amend trigger; got {}",
            summary.subject
        );
    }

    /// Fire-and-forget contract for `hook_commit`: when the underlying
    /// git operation fails (bad package_dir, missing git binary, anything
    /// else), the hook MUST log + return — never panic, never propagate.
    /// The trigger sites in turns.rs / branches.rs / tasks.rs all
    /// schedule this via `tokio::task::spawn_blocking`, so a panic here
    /// would also corrupt the trigger task's executor.
    #[test]
    fn hook_commit_swallows_failure_on_invalid_path() {
        let cfg = enabled_cfg();
        // Path under /proc/1/root that the test user cannot mkdir into —
        // init will fail. Hook must still return cleanly.
        let bad = std::path::PathBuf::from("/proc/1/root/cannot/mkdir/here");
        hook_commit(&cfg, &bad, "emit", "test subject", "session-abc-12345678");
    }

    /// Same fire-and-forget contract for the `branch` trigger, which
    /// always tries to commit (no opt-out checkbox). Lock in that
    /// "explicit user action" path also absorbs git failures.
    #[test]
    fn hook_commit_for_branch_trigger_swallows_failure() {
        let cfg = enabled_cfg();
        let bad = std::path::PathBuf::from("/proc/1/root/another/nonexistent/path");
        hook_commit(
            &cfg,
            &bad,
            "branch",
            "branched session",
            "session-def-87654321",
        );
    }

    /// When `enabled = false` the hook is a no-op — and that no-op
    /// must also never panic. Trivial but worth pinning so a future
    /// refactor doesn't accidentally make the disabled path fall
    /// through to the git invocation.
    #[test]
    fn hook_commit_swallows_failure_on_disabled_config() {
        let cfg = GitConfig {
            enabled: false,
            ..GitConfig::default()
        };
        hook_commit(
            &cfg,
            std::path::Path::new("/whatever"),
            "emit",
            "subject",
            "session-id",
        );
    }

    #[test]
    fn push_drains_large_stderr_without_false_timeout() {
        let _g = GIT_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let fake_git = bin_dir.join("git");
        std::fs::write(
            &fake_git,
            "#!/bin/sh\nhead -c 262144 /dev/zero >&2\nsleep 0.1\nexit 0\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_git).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_git, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        if let Some(existing) = old_path.clone() {
            paths.extend(std::env::split_paths(&existing));
        }
        let joined = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", &joined);

        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let cfg = GitConfig {
            enabled: true,
            remote_url: Some("origin".into()),
            push_timeout_secs: 5,
            ..enabled_cfg()
        };
        let svc = GitService::for_package(&cfg, &repo);
        let result = svc.push_inner();

        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }

        result.expect("large stderr output must not fill the pipe and false-timeout");
    }

    /// `run_git` must kill the underlying git process when its
    /// wallclock budget runs out. Without `child.kill()` on deadline,
    /// a hung git operation would pin a tokio blocking-pool slot
    /// indefinitely: the outer `GitHookPool::spawn` timeout only
    /// stops awaiting the JoinHandle, leaving the git child + the
    /// blocking thread alive.
    #[test]
    fn run_git_with_timeout_kills_hung_subprocess() {
        let _g = GIT_LOCK.lock();
        // Drop a fake `git` on PATH that sleeps for a minute so we can
        // observe the timeout actually killing it. If the kill path
        // doesn't fire, the test wall-clock would hit `cargo test`'s
        // own per-test timeout and the assertion below would never
        // even reach the `Err(..)` arm.
        let tmp = tempfile::tempdir().unwrap();
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir(&bin_dir).unwrap();
        let fake_git = bin_dir.join("git");
        // `exec sleep 60` so the shell process replaces itself with
        // sleep — when we `child.kill()`, the SIGKILL lands directly
        // on sleep (otherwise sh keeps a child running and the pipe
        // FDs stay open, deadlocking the stdout/stderr drain threads).
        std::fs::write(&fake_git, "#!/bin/sh\nexec sleep 60\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&fake_git).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&fake_git, perms).unwrap();
        }

        let old_path = std::env::var_os("PATH");
        let mut paths = vec![bin_dir];
        if let Some(existing) = old_path.clone() {
            paths.extend(std::env::split_paths(&existing));
        }
        let joined = std::env::join_paths(paths).unwrap();
        std::env::set_var("PATH", &joined);

        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        let svc = GitService::for_package(&enabled_cfg(), &repo);
        let start = std::time::Instant::now();
        let result = svc.run_git_with_timeout(&["status"], std::time::Duration::from_secs(1));
        let elapsed = start.elapsed();

        match old_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }

        let err = result.expect_err("hung git must time out, not succeed");
        assert!(
            err.to_string().contains("timed out"),
            "error must mention timeout, got: {err}"
        );
        // Comfortable upper bound — the fake binary sleeps 60s, so any
        // elapsed >5s means we waited on the subprocess past the kill
        // (or never killed it at all).
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "run_git_with_timeout must return promptly after kill; \
             waited {:?}",
            elapsed
        );
    }

    // Canonicalizes HOME and any path under test before comparing so
    // `$HOME/../../etc/cron.hourly/pwn` cannot bypass the path-jail
    // (a starts_with check on the un-canonicalized path is insufficient).
    //
    // Tests share `super::super::GIT_TEST_ENV_LOCK` with `git_routes::config`
    // because both modules mutate `HOME` and `std::env::set_var` is
    // process-global.
    fn with_pinned_home<F: FnOnce(&std::path::Path)>(f: F) {
        let _guard = super::super::GIT_TEST_ENV_LOCK.lock();
        let tmp_home = tempfile::tempdir().unwrap();
        let prior = std::env::var_os("HOME");
        std::env::set_var("HOME", tmp_home.path());
        f(tmp_home.path());
        match prior {
            Some(p) => std::env::set_var("HOME", p),
            None => std::env::remove_var("HOME"),
        }
    }

    #[test]
    fn ssh_keygen_validator_rejects_dotdot_escape_via_parent() {
        with_pinned_home(|home| {
            // home/../../etc — the parent component traverses out of $HOME.
            // canonicalize on the parent will resolve to /tmp or similar
            // and starts_with(canonical_home) returns false.
            let bad = home.join("..").join("..").join("etc").join("pwn");
            let err =
                validate_ssh_key_target(bad.to_str().unwrap()).expect_err("must reject escape");
            assert!(
                err.contains("escapes HOME") || err.contains("canonicaliz"),
                "expected escape rejection, got: {err}"
            );
        });
    }

    #[test]
    fn ssh_keygen_validator_accepts_legitimate_home_subpath() {
        with_pinned_home(|home| {
            // $HOME/.ssh must exist for canonicalize on the parent.
            std::fs::create_dir_all(home.join(".ssh")).unwrap();
            let good = home.join(".ssh/scripps_test_only");
            let resolved = validate_ssh_key_target(good.to_str().unwrap())
                .expect("legitimate path must validate");
            let canon_parent = home.join(".ssh").canonicalize().unwrap();
            assert!(
                resolved.starts_with(&canon_parent),
                "resolved {} should sit under canonical parent {}",
                resolved.display(),
                canon_parent.display(),
            );
        });
    }

    #[test]
    fn ssh_keygen_validator_rejects_target_with_no_existing_parent() {
        with_pinned_home(|home| {
            // Parent must already exist for canonicalize to succeed.
            let bad = home.join("nonexistent_dir").join("key");
            assert!(validate_ssh_key_target(bad.to_str().unwrap()).is_err());
        });
    }

    // `validate_commit_paths` blocks `..`-bearing commit paths and any
    // path that canonicalizes outside the package root.
    #[test]
    fn validate_commit_paths_rejects_dotdot_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let paths = vec!["../../../etc/passwd".to_string()];
        assert!(validate_commit_paths(pkg, &paths).is_err());
    }

    #[test]
    fn validate_commit_paths_accepts_in_pkg_relative_path() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        std::fs::write(pkg.join("runtime/x.json"), "{}").unwrap();
        let paths = vec!["runtime/x.json".to_string()];
        let ok = validate_commit_paths(pkg, &paths).unwrap();
        assert_eq!(ok.len(), 1);
        assert!(ok[0].ends_with("runtime/x.json"));
    }

    #[test]
    fn validate_commit_paths_rejects_absolute_outside_pkg() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        std::fs::create_dir_all(pkg.join("runtime")).unwrap();
        // Absolute path to a file outside pkg.
        let paths = vec!["/etc/hostname".to_string()];
        assert!(validate_commit_paths(pkg, &paths).is_err());
    }

    #[test]
    fn validate_commit_paths_accepts_empty_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let pkg = tmp.path();
        let ok = validate_commit_paths(pkg, &[]).unwrap();
        assert!(ok.is_empty());
    }

    // ── -- separator in remote-URL CLIs ──────────────────────

    #[test]
    fn ls_remote_argv_inserts_dashdash_before_url() {
        let argv = build_ls_remote_argv("ssh://example.com/repo.git");
        let dashdash = argv
            .iter()
            .position(|a| a == "--")
            .expect("argv must contain --");
        let url_idx = argv
            .iter()
            .position(|a| a == "ssh://example.com/repo.git")
            .expect("argv must contain url");
        assert!(url_idx > dashdash, "URL must come AFTER --; got {argv:?}");
    }

    #[test]
    fn remote_add_argv_inserts_dashdash_before_url() {
        let argv = build_remote_add_argv("ssh://example.com/repo.git");
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        let url_idx = argv
            .iter()
            .position(|a| a == "ssh://example.com/repo.git")
            .unwrap();
        assert!(url_idx > dashdash);
    }

    #[test]
    fn remote_set_url_argv_inserts_dashdash_before_url() {
        let argv = build_remote_set_url_argv("https://example.com/repo.git");
        let dashdash = argv.iter().position(|a| a == "--").unwrap();
        let url_idx = argv
            .iter()
            .position(|a| a == "https://example.com/repo.git")
            .unwrap();
        assert!(url_idx > dashdash);
    }
}
