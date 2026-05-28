//! Persistent `GitConfig` living at `~/.scripps-workflow/git-config.json`
//! (override via `SWFC_GIT_CONFIG_PATH`). Atomic saves via `.tmp`
//! rename. Secrets policy: `ssh_key_path` is a path, never key
//! contents.
//!
//! Per-package shape: the legacy global `repo_path` was
//! removed. Provenance is per-package — each emitted package gets its
//! own `.git` directory. Loading a legacy file that still carries
//! `repo_path` emits a deprecation warning and proceeds with the
//! remaining fields; the next `save()` drops the field.
//!
//! Unknown fields are tolerated at load time (`#[serde(default)]` on
//! every field + permissive default deserialize) so a config file
//! written by an older binary keeps working across upgrades.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// UI-facing git configuration. Every field has a sensible default so
/// the UI can render a usable form before the user has edited
/// anything.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GitConfig {
    /// Top-level kill switch. When false, no git subprocess runs
    /// regardless of the per-trigger checkboxes below. `SWFC_GIT_
    /// ENABLED=0` as an env var forces this to false at read time.
    #[serde(default)]
    pub enabled: bool,
    /// `origin` remote URL (SSH or HTTPS). Absent = local-only repo
    /// with no push target.
    #[serde(default)]
    pub remote_url: Option<String>,
    /// Absolute path to the SSH private key to use for `git push`.
    /// Absent = rely on the system's ssh-agent / `~/.ssh/config`. When
    /// present, the service sets `GIT_SSH_COMMAND="ssh -i <path> -o
    /// IdentitiesOnly=yes"` per git invocation.
    #[serde(default)]
    pub ssh_key_path: Option<String>,
    /// `user.name` written into every commit. Defaults to "Scripps
    /// Workflow".
    #[serde(default = "default_author_name")]
    pub author_name: String,
    /// `user.email` written into every commit.
    #[serde(default = "default_author_email")]
    pub author_email: String,
    /// When true, auto-commit after every `emit_package` call.
    #[serde(default = "default_true")]
    pub commit_on_emit: bool,
    /// When true, auto-commit after every `amend_stage_method` call.
    #[serde(default = "default_true")]
    pub commit_on_amend: bool,
    /// On by default — every successful task completion creates a recovery
    /// point for git-aware DAG recovery. Set to false in
    /// `~/.scripps-workflow/git-config.json` to opt out on systems where
    /// filesystem-write-heavy git activity adds unacceptable overhead.
    #[serde(default = "default_true")]
    pub commit_on_task_completed: bool,
    /// `git push origin HEAD` after every auto-commit. Off by default
    /// so the SME runs the push explicitly from the Settings page
    /// until they're happy with the generated history.
    #[serde(default)]
    pub auto_push: bool,
    /// Wall-clock timeout on `git push` (seconds). Prevents a hung
    /// auth prompt from blocking the whole session.
    #[serde(default = "default_push_timeout")]
    pub push_timeout_secs: u64,
}

fn default_author_name() -> String {
    "Scripps Workflow".to_string()
}

fn default_author_email() -> String {
    "noreply@scripps-workflow.local".to_string()
}

fn default_true() -> bool {
    true
}

fn default_push_timeout() -> u64 {
    30
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            remote_url: None,
            ssh_key_path: None,
            author_name: default_author_name(),
            author_email: default_author_email(),
            commit_on_emit: true,
            commit_on_amend: true,
            commit_on_task_completed: true,
            auto_push: false,
            push_timeout_secs: default_push_timeout(),
        }
    }
}

impl GitConfig {
    /// Read the config file at `path`. Missing file or parse failure
    /// ⇒ default config (the UI's blank form state). Legacy files
    /// carrying the removed `repo_path` field emit a one-line warning
    /// and proceed — the field is dropped on next save.
    pub fn load_or_default(path: &Path) -> Self {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(_) => return Self::default(),
        };
        // Load via a permissive serde_json::Value first so we can
        // emit the legacy-field diagnostic before the structured
        // deserialize drops the data silently.
        if let Ok(raw) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            if let Some(old) = raw.get("repo_path").and_then(|v| v.as_str()) {
                tracing::warn!(
                    legacy_repo_path = old,
                    "legacy repo_path={} in git-config.json ignored — \
                     git provenance is per-package since 2026-05-12",
                    old
                );
            }
        }
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!(
                "[git-config] failed to parse {}: {} — using defaults",
                path.display(),
                e
            );
            Self::default()
        })
    }

    /// Atomic write: serialize → `.tmp` → fsync → rename → fsync parent.
    pub fn save(&self, path: &Path) -> Result<()> {
        let body = serde_json::to_vec_pretty(self)?;
        ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync(path, &body)?;
        Ok(())
    }

    /// Validate paths before a save. `ssh_key_path` (when set) must be
    /// a file inside `$HOME`; the path-content check rejects shell
    /// metacharacters before any path resolution. Author name/email
    /// must be non-empty. `remote_url`, when set, must be non-empty —
    /// we don't try to parse it for SSH-vs-HTTPS since the user might
    /// use a self-hosted git + custom port.
    ///
    /// No `repo_path` validation: provenance is per-package now;
    /// each emitted package's directory comes from
    /// `session.emitted_package_path`, not the config.
    pub fn validate(&self) -> Result<()> {
        if self.author_name.trim().is_empty() {
            return Err(anyhow!("author_name is required"));
        }
        if self.author_email.trim().is_empty() {
            return Err(anyhow!("author_email is required"));
        }
        if let Some(remote) = &self.remote_url {
            if remote.trim().is_empty() {
                return Err(anyhow!("remote_url, when set, must be non-empty"));
            }
        }
        if let Some(key) = &self.ssh_key_path {
            // Reject shell metacharacters before any path resolution.
            // `key` ends up inside `GIT_SSH_COMMAND` which the system
            // invokes via `sh -c`; even a $HOME-jailed path with a
            // backtick or `$(...)` segment would let an attacker who
            // can write the config file (e.g., compromised UI) execute
            // arbitrary commands. The shlex::quote() at the call site
            // is defense-in-depth; this check is the first line.
            for ch in key.chars() {
                if ch.is_whitespace()
                    || matches!(
                        ch,
                        ';' | '&'
                            | '|'
                            | '$'
                            | '`'
                            | '\\'
                            | '"'
                            | '\''
                            | '\n'
                            | '\r'
                            | '<'
                            | '>'
                            | '('
                            | ')'
                    )
                {
                    return Err(anyhow!(
                        "ssh_key_path contains a disallowed character ({:?}); \
                         use only alphanumerics, '/', '.', '-', '_'",
                        ch
                    ));
                }
            }
            let path = PathBuf::from(key);
            if let Some(home) = dirs_home() {
                if !path.starts_with(&home) {
                    return Err(anyhow!(
                        "ssh_key_path must live inside your home directory (got {})",
                        path.display()
                    ));
                }
                // Defense-in-depth: when the path exists, canonicalize
                // it and re-check against the canonical $HOME. Catches
                // symlinks aimed outside the tree and embedded `..`
                // segments the syntactic `starts_with` check above
                // accepts (e.g. `$HOME/.ssh/../../../etc/passwd`).
                if let (Ok(canon_path), Ok(canon_home)) = (path.canonicalize(), home.canonicalize())
                {
                    if !canon_path.starts_with(&canon_home) {
                        return Err(anyhow!(
                            "ssh_key_path resolves outside your home directory \
                             (got {} → {})",
                            path.display(),
                            canon_path.display()
                        ));
                    }
                }
            }
            // File presence check is deferred — the SME may have just
            // started `Generate new key` and we save the config with
            // the target path before the key exists. `test_remote` +
            // `post_push` report the actual auth error when the file
            // isn't there.
        }
        Ok(())
    }

    /// Evaluated enabled flag taking `SWFC_GIT_ENABLED=0` into account.
    /// Handlers consult this when deciding whether to run the commit.
    pub fn effective_enabled(&self) -> bool {
        if std::env::var("SWFC_GIT_ENABLED").ok().as_deref() == Some("0") {
            return false;
        }
        self.enabled
    }
}

/// Default config path. Respects `SWFC_GIT_CONFIG_PATH`.
pub fn git_config_path() -> PathBuf {
    if let Ok(p) = std::env::var("SWFC_GIT_CONFIG_PATH") {
        return PathBuf::from(p);
    }
    let home = dirs_home().unwrap_or_else(|| PathBuf::from("."));
    home.join(".scripps-workflow/git-config.json")
}

/// Portable `$HOME` lookup without pulling in the `dirs` crate.
fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Re-alias the shared `super::super::GIT_TEST_ENV_LOCK` so the
    /// rest of this module's tests don't change. Both `config.rs` and
    /// `service.rs` HOME-mutating tests now serialize on the same lock.
    use super::super::GIT_TEST_ENV_LOCK as ENV_LOCK;

    #[test]
    fn defaults_are_opt_in() {
        let c = GitConfig::default();
        assert!(!c.enabled);
        assert!(c.commit_on_emit);
        assert!(c.commit_on_amend);
        // Per-task commits are default-on to provide git-aware DAG recovery
        // points at task granularity. Operators can opt out in git-config.json.
        assert!(c.commit_on_task_completed);
        assert!(!c.auto_push);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("git-config.json");
        let c = GitConfig {
            enabled: true,
            remote_url: Some("git@github.com:alan/provenance.git".into()),
            ..Default::default()
        };
        c.save(&path).unwrap();
        let back = GitConfig::load_or_default(&path);
        assert!(back.enabled);
        assert_eq!(
            back.remote_url.as_deref(),
            Some("git@github.com:alan/provenance.git")
        );
    }

    /// A legacy `git-config.json` from before the per-package refactor
    /// carries `repo_path`. Loading it must succeed (the rest of the
    /// fields are still valid) and must not include `repo_path` in the
    /// in-memory shape — the field has been removed from the struct.
    #[test]
    fn load_drops_legacy_repo_path_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("git-config.json");
        let legacy = serde_json::json!({
            "enabled": true,
            "repo_path": "/home/alan/old-packages",
            "remote_url": "git@example.com:alan/repo.git",
            "ssh_key_path": null,
            "author_name": "Alan",
            "author_email": "alan@example.com",
            "commit_on_emit": true,
            "commit_on_amend": true,
            "commit_on_task_completed": false,
            "auto_push": false,
            "push_timeout_secs": 30
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

        let loaded = GitConfig::load_or_default(&path);
        assert!(loaded.enabled);
        assert_eq!(loaded.author_name, "Alan");
        assert_eq!(
            loaded.remote_url.as_deref(),
            Some("git@example.com:alan/repo.git")
        );
        // After re-save the file must no longer carry repo_path.
        loaded.save(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !raw.contains("repo_path"),
            "re-saved config must not carry the legacy repo_path field; got:\n{}",
            raw
        );
    }

    #[test]
    fn effective_enabled_respects_kill_switch() {
        let _guard = ENV_LOCK.lock();
        let c = GitConfig {
            enabled: true,
            ..Default::default()
        };
        std::env::set_var("SWFC_GIT_ENABLED", "0");
        assert!(!c.effective_enabled());
        std::env::remove_var("SWFC_GIT_ENABLED");
        assert!(c.effective_enabled());
    }

    #[test]
    fn validate_rejects_ssh_key_outside_home() {
        let _guard = ENV_LOCK.lock();
        let c = GitConfig {
            ssh_key_path: Some("/etc/ssh/evil_key".into()),
            ..Default::default()
        };
        std::env::set_var("HOME", "/home/alan");
        assert!(c.validate().is_err());
    }

    #[test]
    fn validate_rejects_relative_path_for_ssh_key() {
        // Relative paths can never be inside an absolute $HOME because
        // `starts_with` is path-segment based. Lock this in so a future
        // refactor doesn't change the semantics by accident.
        let _guard = ENV_LOCK.lock();
        let c = GitConfig {
            ssh_key_path: Some("../id_rsa".into()),
            ..Default::default()
        };
        std::env::set_var("HOME", "/home/alan");
        assert!(
            c.validate().is_err(),
            "relative ssh_key_path must be rejected"
        );
    }

    #[cfg(unix)]
    #[test]
    fn validate_rejects_symlink_resolving_outside_home() {
        // Symlink in $HOME pointing at a file outside $HOME. The
        // syntactic `starts_with(home)` check passes (the symlink path
        // itself does live in $HOME) — only the canonical resolution
        // catches it.
        use std::os::unix::fs::symlink;
        let _guard = ENV_LOCK.lock();
        let tmp_home = tempfile::tempdir().unwrap();
        // Outside the temp $HOME — a sibling temp file we won't claim
        // is "in" $HOME.
        let outside = tempfile::NamedTempFile::new().unwrap();
        let symlink_path = tmp_home.path().join("evil_key");
        symlink(outside.path(), &symlink_path).unwrap();

        let c = GitConfig {
            ssh_key_path: Some(symlink_path.to_string_lossy().to_string()),
            ..Default::default()
        };
        std::env::set_var("HOME", tmp_home.path());
        let res = c.validate();
        std::env::remove_var("HOME");
        assert!(
            res.is_err(),
            "symlink in $HOME resolving outside must be rejected"
        );
    }

    #[test]
    fn validate_rejects_embedded_dotdot_traversal() {
        // `$HOME/.ssh/../../../etc/passwd` syntactically starts with
        // $HOME, so the existing path-prefix check accepts it. The
        // canonicalization defense kicks in only if the path exists.
        // Construct a real existing path for the test by canonical-
        // izing /etc and relying on its `..` traversal.
        // Strategy: $HOME/sub/../outside-target — when `outside-target`
        // exists at $HOME's parent, canonicalize resolves to it.
        let _guard = ENV_LOCK.lock();
        let tmp_home = tempfile::tempdir().unwrap();
        let parent_dir = tmp_home.path().parent().unwrap();
        let outside_file = parent_dir.join("outside-key.txt");
        std::fs::write(&outside_file, "key contents").unwrap();
        // Path through tmp_home that climbs out via `..`.
        let traversal = tmp_home
            .path()
            .join("..")
            .join(outside_file.file_name().unwrap());

        let c = GitConfig {
            ssh_key_path: Some(traversal.to_string_lossy().to_string()),
            ..Default::default()
        };
        std::env::set_var("HOME", tmp_home.path());
        let res = c.validate();
        std::env::remove_var("HOME");
        let _ = std::fs::remove_file(&outside_file);
        assert!(
            res.is_err(),
            "ssh_key_path with embedded `..` resolving outside $HOME must be rejected"
        );
    }
}
