//! Harness-side
//! sandbox enforcement. Translates the typed `SandboxPolicy` into
//! container-runtime invocation arguments + a pre-dispatch refusal
//! check that runs alongside the silent-completion guard.
//!
//! Why both compose-time + dispatch-time:
//!
//! - The composer `composer_v4::policy_gate::evaluate` runs at
//!   composition time; it catches policy violations before the SME
//!   sees a confirmation prompt. But composition runs once per
//!   session-confirm; tasks dispatch one at a time over the lifetime
//!   of an execution. A node that drifts (lifecycle re-classified,
//!   review_status updated) between compose and dispatch needs a
//!   second check.
//! - The harness owns the actual subprocess; only here can we map
//!   `SandboxPolicy::deny_network` → container `--network=none` and
//!   `SandboxPolicy::deny_secrets` → omit secret-bearing env vars.
//!
//! The harness-side surface is intentionally narrow: declare the
//! policy, get back a typed refusal report or a list of container CLI
//! flags. The harness call site (executor::local / aws / slurm)
//! decides whether to apply the flags via the runtime it has
//! (apptainer / docker / podman) — the policy is uniform.
//!
//! ## Bubblewrap runtime enforcement
//!
//! `BubblewrapRunner` translates a `SandboxPolicy` into a `bwrap`-wrapped
//! `std::process::Command`. Activated only when
//! `SWFC_LOCAL_SANDBOX=bubblewrap`; the `off` (default) path returns the
//! original command unchanged so non-sandbox users pay zero cost.
//!
//! Provenance: each wrapped dispatch appends a record to
//! `runtime/sandbox-runs.jsonl` (excluded from the byte-reproducibility
//! gate because it contains timestamps and exit codes).

use crate::validators::ValidatorOutcome;
use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::PathBuf;

pub use ecaa_workflow_core::sandbox_policy::{
    check_generated_code_node, check_workflow_dag, SandboxPolicy, SandboxRefusal,
};
use ecaa_workflow_core::workflow_contracts::implementation::Implementation;
use ecaa_workflow_core::workflow_contracts::task_node::TaskNode;

/// Runtime-shaped flags the harness can pass to a container engine
/// (docker / apptainer / podman). The local executor renders them as
/// command-line args.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ContainerSandboxFlags {
    /// Set network to `none` when the policy denies network access.
    pub network_none: bool,
    /// Drop all secret-bearing env vars.
    pub redact_secret_env: bool,
    /// Read-only host bind mounts only.
    pub host_fs_readonly: bool,
    /// Memory ceiling in MB (rendered as `--memory=NNN`).
    pub memory_limit_mb: Option<u32>,
    /// Wall-clock seconds before SIGTERM.
    pub wall_timeout_secs: Option<u32>,
    /// Allowlist of additional env vars permitted in the container
    /// (anything outside is dropped). Empty = pass-through.
    pub env_allowlist: BTreeSet<String>,
}

impl ContainerSandboxFlags {
    /// Render as a list of container-runtime CLI args. The shape
    /// matches the docker/podman common subset; apptainer rewrites
    /// these into its `--no-net`/`--no-mount` etc. form. The list is
    /// sorted by flag name for byte-stable replay.
    pub fn to_cli_args(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.network_none {
            out.push("--network=none".into());
        }
        if self.host_fs_readonly {
            out.push("--read-only".into());
        }
        if let Some(mb) = self.memory_limit_mb {
            out.push(format!("--memory={mb}m"));
        }
        if let Some(secs) = self.wall_timeout_secs {
            out.push(format!("--stop-timeout={secs}"));
        }
        out.sort();
        out
    }
}

/// Translate a `SandboxPolicy` into a `ContainerSandboxFlags`. Every
/// policy field with a runtime equivalent maps; fields enforced
/// elsewhere (static analysis, dependency allowlist) become no-ops.
pub fn flags_from_policy(policy: &SandboxPolicy) -> ContainerSandboxFlags {
    ContainerSandboxFlags {
        network_none: policy.deny_network,
        redact_secret_env: policy.deny_secrets,
        host_fs_readonly: policy.deny_host_fs,
        memory_limit_mb: policy.memory_limit_mb,
        wall_timeout_secs: policy.wall_timeout_secs,
        env_allowlist: BTreeSet::new(),
    }
}

/// Pre-dispatch refusal check for a task node. Returns `Some(report)`
/// when the task should be refused; `None` when dispatch may proceed.
///
/// Mirrors the compose-time `composer_v4::policy_gate` and
/// `sandbox_policy::check_generated_code_node` checks; runs against
/// the *current* TaskNode state so post-compose lifecycle drift is caught.
pub fn pre_dispatch_check(node: &TaskNode, policy: &SandboxPolicy) -> Option<PreDispatchRefusal> {
    let refusals = check_generated_code_node(node, policy);
    if refusals.is_empty()
        // Container requirement applies to non-GeneratedCode nodes too
        // when policy.require_container is set and the implementation
        // isn't already container-bound.
        && !needs_container_wrap(node, policy)
    {
        return None;
    }
    Some(PreDispatchRefusal {
        node_id: node.id.clone(),
        sandbox_refusals: refusals,
        needs_container_wrap: needs_container_wrap(node, policy),
    })
}

fn needs_container_wrap(node: &TaskNode, policy: &SandboxPolicy) -> bool {
    if !policy.require_container {
        return false;
    }
    matches!(node.implementation, Implementation::Unimplemented)
        || matches!(node.implementation, Implementation::ManualProtocol { .. })
}

/// Per-task refusal report. Mirrors `BlockerKind::SandboxRefused`
/// payload shape so the harness can pass it through unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreDispatchRefusal {
    /// Task node id that was refused.
    pub node_id: String,
    /// Typed refusals raised by the sandbox policy check.
    pub sandbox_refusals: Vec<SandboxRefusal>,
    /// `true` when the node requires container wrapping but no runtime is available.
    pub needs_container_wrap: bool,
}

impl PreDispatchRefusal {
    /// Human summary surfaced to the SME via the existing
    /// `BlockerCard`/`BlockerKind` channels.
    pub fn human_summary(&self) -> String {
        if self.sandbox_refusals.is_empty() && self.needs_container_wrap {
            return format!(
                "node {} requires container wrapping under active sandbox policy",
                self.node_id
            );
        }
        let kinds: Vec<String> = self
            .sandbox_refusals
            .iter()
            .map(|r| format!("{:?}", r))
            .collect();
        format!(
            "node {} sandbox-refused: {}",
            self.node_id,
            kinds.join(", ")
        )
    }
}

// ── Phase C7: bubblewrap-backed runtime sandbox ──────────────────────────

/// Errors from the bubblewrap runner. Typed so callers can surface the
/// right recovery affordance to the SME without string-matching.
#[derive(Debug, thiserror::Error)]
pub enum SandboxRunnerError {
    /// The `bwrap` binary was not found at the configured path.
    #[error("bwrap binary not found at {0:?}; install bubblewrap or set SWFC_LOCAL_SANDBOX=off")]
    BwrapBinaryMissing(PathBuf),
    /// The sandbox policy configuration was invalid or inconsistent.
    #[error("local sandbox policy invalid: {0}")]
    InvalidPolicy(String),
    /// W2.2 — task-nodes.json was loaded successfully but does not
    /// list the requested task. A package whose sandbox-policy.json is
    /// present but whose node-list is incomplete is a packaging bug,
    /// not a "skip me" signal — refusing to dispatch surfaces the
    /// inconsistency at runtime instead of silently bypassing the
    /// sandbox for that task.
    #[error("task {task_id:?} not listed in task-nodes.json; refusing to dispatch unsandboxed")]
    TaskNotInNodeList { task_id: String },
}

/// Translates a `SandboxPolicy` into a `bwrap`-wrapped
/// `std::process::Command`. Call `from_env` to construct — it checks
/// `SWFC_LOCAL_SANDBOX` and errors loudly when `bubblewrap` is
/// requested but `bwrap` is absent (no silent fallback).
#[derive(Debug)]
pub struct BubblewrapRunner {
    bwrap_path: PathBuf,
    workdir: PathBuf,
}

/// Secret-bearing env var name patterns (case-insensitive substring match).
/// Anything matching one of these gets `--unsetenv`'d when `deny_secrets` is
/// active so the sandboxed process cannot exfiltrate tokens.
const SECRET_ENV_PATTERNS: &[&str] = &[
    "KEY",
    "TOKEN",
    "SECRET",
    "PASSWORD",
    // SSH agent sockets are functionally equivalent to a key file.
    "SSH_AUTH_SOCK",
    "SSH_AGENT_PID",
];

/// Minimal writable tmpfs path used as a clean `HOME` when the policy
/// drops the real `HOME` to prevent credential exfiltration. The
/// sandboxed process still needs a home dir for Python/R toolchain
/// defaults.
const SANDBOX_HOME: &str = "/tmp/sandbox_home";

impl BubblewrapRunner {
    /// Read `SWFC_LOCAL_SANDBOX`. Returns:
    /// - `Ok(None)` — mode is `off` (default); caller should skip wrapping.
    /// - `Ok(Some(r))` — mode is `bubblewrap` and `bwrap` exists.
    /// - `Err(_)` — mode is `bubblewrap` but `bwrap` is missing.
    ///
    /// The error is hard — the user explicitly opted in; silent fallback
    /// would violate the security guarantee.
    pub fn from_env(workdir: PathBuf) -> Result<Option<Self>, SandboxRunnerError> {
        let mode = std::env::var("SWFC_LOCAL_SANDBOX").unwrap_or_else(|_| "off".into());
        match mode.to_ascii_lowercase().as_str() {
            "off" | "" => Ok(None),
            "bubblewrap" => {
                let bwrap_path = PathBuf::from("/usr/bin/bwrap");
                if !bwrap_path.exists() {
                    return Err(SandboxRunnerError::BwrapBinaryMissing(bwrap_path));
                }
                Ok(Some(Self {
                    bwrap_path,
                    workdir,
                }))
            }
            other => Err(SandboxRunnerError::InvalidPolicy(format!(
                "SWFC_LOCAL_SANDBOX={other:?} is not a recognised value; \
                 expected 'off' or 'bubblewrap'"
            ))),
        }
    }

    /// Test helper: variant of `from_env` that uses a caller-supplied `bwrap_path`.
    /// Checks `SWFC_LOCAL_SANDBOX` but substitutes the given path instead of
    /// `/usr/bin/bwrap`. Integration tests use this to simulate a missing binary.
    ///
    /// Not part of the production public API; exposed for integration testing
    /// from `crates/harness/tests/`.
    #[doc(hidden)]
    pub fn from_env_with_bwrap_path(
        bwrap_path: PathBuf,
        workdir: PathBuf,
    ) -> Result<Option<Self>, SandboxRunnerError> {
        let mode = std::env::var("SWFC_LOCAL_SANDBOX").unwrap_or_else(|_| "off".into());
        match mode.to_ascii_lowercase().as_str() {
            "off" | "" => Ok(None),
            "bubblewrap" => {
                if !bwrap_path.exists() {
                    return Err(SandboxRunnerError::BwrapBinaryMissing(bwrap_path));
                }
                Ok(Some(Self {
                    bwrap_path,
                    workdir,
                }))
            }
            other => Err(SandboxRunnerError::InvalidPolicy(format!(
                "SWFC_LOCAL_SANDBOX={other:?} is not a recognised value"
            ))),
        }
    }

    /// Test helper: construct a `BubblewrapRunner` without checking the env var
    /// or verifying that `/usr/bin/bwrap` exists. Callers that only test
    /// `render_args` / `wrap` output (not spawning) can use this to avoid the
    /// env-var dance.
    ///
    /// Not part of the production public API; exposed for integration testing
    /// from `crates/harness/tests/`.
    #[doc(hidden)]
    pub fn new_for_test(workdir: PathBuf) -> Self {
        Self {
            bwrap_path: PathBuf::from("/usr/bin/bwrap"),
            workdir,
        }
    }

    /// Render the complete bwrap argument list for `policy` in a
    /// deterministic order suitable for replay tests and provenance
    /// Recording. The final `--` separator is included; the caller
    /// appends the real argv (program + args) after it.
    ///
    /// Argument order:
    /// 1. safety flags (`--die-with-parent`, `--new-session`)
    /// 2. filesystem namespace (`--proc`, `--dev`, `--tmpfs`, `--ro-bind`s)
    /// 3. network (`--unshare-net`)
    /// 4. env redaction (`--unsetenv` per secret pattern)
    /// 5. env allowlist (`--clearenv` + `--setenv` per allowed name)
    /// 6. resource prefix built separately (prlimit / timeout)
    /// 7. `--`
    ///
    /// NOTE: resource limits (memory, timeout) are prepended as wrapper
    /// commands (`prlimit`, `timeout`) rather than bwrap flags because
    /// bwrap itself has no `--memory` flag. `render_args` returns the
    /// bwrap args only; the caller uses `wrap()` which prepends those
    /// wrappers.
    pub fn render_args(&self, policy: &SandboxPolicy) -> Vec<String> {
        // Safety flags + minimal kernel surfaces + ephemeral tmp — always
        // present regardless of policy.
        let mut args: Vec<String> = vec![
            "--die-with-parent".into(),
            "--new-session".into(),
            "--proc".into(),
            "/proc".into(),
            "--dev".into(),
            "/dev".into(),
            "--tmpfs".into(),
            "/tmp".into(),
        ];

        if policy.deny_host_fs {
            // Read-only allowlist: standard system directories + workdir.
            // /bin, /lib, /lib64, /usr are the minimal runtime surface on
            // a Debian/Ubuntu host (Rust stdlib links against glibc,
            // Python needs /usr/lib). /etc/resolv.conf is included
            // conditionally below only when network is *not* denied.
            for dir in &["/usr", "/bin", "/lib", "/lib64"] {
                let p = PathBuf::from(dir);
                if p.exists() {
                    args.push("--ro-bind".into());
                    args.push(dir.to_string());
                    args.push(dir.to_string());
                }
            }
            // The package workdir is bind-mounted RW so the agent can
            // write outputs into `runtime/`.
            args.push("--bind".into());
            args.push(self.workdir.to_string_lossy().to_string());
            args.push(self.workdir.to_string_lossy().to_string());

            // resolv.conf only when we're allowing network (deny_network
            // would make it irrelevant — no egress even with the file).
            if !policy.deny_network {
                let resolv = "/etc/resolv.conf";
                if PathBuf::from(resolv).exists() {
                    args.push("--ro-bind".into());
                    args.push(resolv.into());
                    args.push(resolv.into());
                }
            }

            // Minimal /dev/null via --bind to the already-mounted /dev.
            // bwrap's `--dev /dev` covers this, but be explicit.
            args.push("--ro-bind".into());
            args.push("/dev/null".into());
            args.push("/dev/null".into());

            // A clean writable HOME so Python/R toolchains don't
            // immediately crash on missing dotfiles.
            args.push("--tmpfs".into());
            args.push(SANDBOX_HOME.into());
            args.push("--setenv".into());
            args.push("HOME".into());
            args.push(SANDBOX_HOME.into());
        }

        // Network namespace isolation.
        if policy.deny_network {
            args.push("--unshare-net".into());
        }

        if policy.deny_secrets {
            // Unset every env var whose name (uppercased) contains any of
            // the secret patterns. The caller's env is inherited by bwrap
            // (its own process env) before bwrap forks the child, so
            // --unsetenv strips them from the child's perspective.
            //
            // We enumerate the *current* process environment — that's
            // what bwrap will inherit. The secret patterns are checked
            // with a case-insensitive substring match.
            //
            // `env::vars()` returns items in unspecified order; collect
            // into a BTreeSet so the emitted `--unsetenv` sequence is
            // sorted by key. Without the sort, two back-to-back
            // `render_args` calls can disagree when a sibling test
            // mutates the process environment between them (the
            // `args_are_deterministic_per_policy` invariant).
            let mut secret_keys: BTreeSet<String> = BTreeSet::new();
            for (key, _) in std::env::vars() {
                let upper = key.to_ascii_uppercase();
                let is_secret = SECRET_ENV_PATTERNS.iter().any(|pat| upper.contains(pat));
                if is_secret {
                    secret_keys.insert(key);
                }
            }
            for key in secret_keys {
                args.push("--unsetenv".into());
                args.push(key);
            }
        }

        // Strict env allowlist: when non-empty, every process env var NOT
        // in the allowlist (and not already handled by deny_secrets above)
        // gets an --unsetenv. This is independent of deny_secrets — deny_secrets
        // scrubs by pattern (tokens / keys / passwords), allow_envs scrubs by
        // explicit name (only these vars survive). The two together give:
        // deny_secrets → remove tokens/keys/passwords
        // Allow_envs → remove everything else not explicitly permitted
        //
        // Names already emitted by deny_secrets are skipped here (deduped by
        // the BTreeSet below) to keep the arg list tidy.
        if !policy.allow_envs.is_empty() {
            let allowlist: BTreeSet<&str> = policy.allow_envs.iter().map(|s| s.as_str()).collect();
            let mut seen: BTreeSet<String> = BTreeSet::new();
            // Collect the names already scheduled for unsetenv by deny_secrets
            // so we don't duplicate them.
            let already_unset: BTreeSet<String> = args
                .windows(2)
                .filter_map(|w| {
                    if w[0] == "--unsetenv" {
                        Some(w[1].clone())
                    } else {
                        None
                    }
                })
                .collect();
            let mut extra_unsets: Vec<String> = Vec::new();
            for (key, _) in std::env::vars() {
                if !allowlist.contains(key.as_str())
                    && !already_unset.contains(&key)
                    && seen.insert(key.clone())
                {
                    extra_unsets.push(key);
                }
            }
            // Sort for byte-stable replay.
            extra_unsets.sort();
            for key in extra_unsets {
                args.push("--unsetenv".into());
                args.push(key);
            }
        }

        // `--` separates bwrap flags from the child command.
        args.push("--".into());
        args
    }

    /// Wrap `cmd` so that argv0 becomes `/usr/bin/bwrap` (or `/usr/bin/prlimit
    /// /usr/bin/timeout bwrap` when resource limits are set) and the original
    /// program + args are appended after the bwrap separator.
    ///
    /// The returned `Command` is ready to spawn; callers should NOT set
    /// additional args on it after wrapping (that would append to the bwrap
    /// args, not the child).
    pub fn wrap(
        &self,
        program: &str,
        program_args: &[&str],
        policy: &SandboxPolicy,
    ) -> std::process::Command {
        let bwrap_args = self.render_args(policy);

        // Build the prefix chain: [timeout N] [prlimit --as=N --] bwrap...
        // Both are optional.
        let has_timeout = policy.wall_timeout_secs.is_some();
        let has_memory = policy.memory_limit_mb.is_some();

        let (argv0, leading_args): (&str, Vec<String>) = match (has_timeout, has_memory) {
            (true, true) => {
                // Timeout N prlimit --as=BYTES -- bwrap...
                let timeout_secs = policy.wall_timeout_secs.unwrap();
                let mem_bytes = (policy.memory_limit_mb.unwrap() as u64) * 1024 * 1024;
                (
                    "/usr/bin/timeout",
                    vec![
                        timeout_secs.to_string(),
                        "/usr/bin/prlimit".into(),
                        format!("--as={}", mem_bytes),
                        "--".into(),
                        self.bwrap_path.to_string_lossy().to_string(),
                    ],
                )
            }
            (true, false) => {
                // Timeout N bwrap...
                let timeout_secs = policy.wall_timeout_secs.unwrap();
                (
                    "/usr/bin/timeout",
                    vec![
                        timeout_secs.to_string(),
                        self.bwrap_path.to_string_lossy().to_string(),
                    ],
                )
            }
            (false, true) => {
                // Prlimit --as=BYTES -- bwrap...
                let mem_bytes = (policy.memory_limit_mb.unwrap() as u64) * 1024 * 1024;
                (
                    "/usr/bin/prlimit",
                    vec![
                        format!("--as={}", mem_bytes),
                        "--".into(),
                        self.bwrap_path.to_string_lossy().to_string(),
                    ],
                )
            }
            (false, false) => (self.bwrap_path.to_str().unwrap_or("/usr/bin/bwrap"), vec![]),
        };

        let mut cmd = std::process::Command::new(argv0);
        // leading args (timeout/prlimit wrappers + bwrap path)
        cmd.args(leading_args.iter());
        // bwrap policy args (ends with --)
        cmd.args(bwrap_args.iter());
        // the real program
        cmd.arg(program);
        // the real program's args
        cmd.args(program_args.iter());
        cmd
    }

    /// Compute a short hex digest of the policy for the provenance record.
    /// Uses a stable JSON serialization so the digest is consistent across
    /// runs given the same policy fields.
    pub fn policy_digest(policy: &SandboxPolicy) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        // Stable JSON (BTreeMap fields order is alphabetical via serde).
        let json = serde_json::to_string(policy).unwrap_or_default();
        let mut h = DefaultHasher::new();
        json.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

/// Append a provenance record to `<package>/runtime/sandbox-runs.jsonl`.
///
/// The sidecar is excluded from the byte-reproducibility gate (timestamps
/// and exit codes are inherently non-reproducible). It provides forensic
/// evidence that the sandbox was applied and what bwrap flags were used.
///
/// Best-effort: failures are logged to stderr but never abort dispatch.
pub fn append_sandbox_run_record(
    package_root: &std::path::Path,
    task_id: &str,
    policy: &SandboxPolicy,
    bwrap_args: &[String],
    exit_code: Option<i32>,
) {
    let runtime = package_root.join("runtime");
    if let Err(e) = std::fs::create_dir_all(&runtime) {
        tracing::warn!(
            task_id = %task_id,
            error = %e,
            "sandbox-enforcer: mkdir runtime/ failed",
        );
        return;
    }
    let path = runtime.join("sandbox-runs.jsonl");
    let record = serde_json::json!({
        "task_id": task_id,
        "policy_digest": BubblewrapRunner::policy_digest(policy),
        "bwrap_args": bwrap_args,
        "start_time": ecaa_workflow_core::time_helpers::now_rfc3339(),
        "exit_code": exit_code,
    });
    let mut line = serde_json::to_string(&record).unwrap_or_default();
    line.push('\n');
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        Ok(mut f) => {
            if let Err(e) = f.write_all(line.as_bytes()) {
                tracing::warn!(
                    task_id = %task_id,
                    error = %e,
                    "sandbox-enforcer: sandbox-runs.jsonl write failed",
                );
            }
        }
        Err(e) => tracing::warn!(
            task_id = %task_id,
            error = %e,
            "sandbox-enforcer: sandbox-runs.jsonl open failed",
        ),
    }
}

/// Validate a task's emitted artifacts against `policy.validate_output_schema`.
/// When the policy requires schema validation but no validator
/// reports were produced, return `Failed`. Wired alongside the
/// existing silent-completion guard.
pub fn enforce_output_schema_validation(
    policy: &SandboxPolicy,
    validator_outcomes: &[ValidatorOutcome],
) -> Result<(), SandboxRefusal> {
    if !policy.validate_output_schema {
        return Ok(());
    }
    if validator_outcomes.is_empty() {
        return Err(SandboxRefusal::OutputSchemaValidationFailed {
            reason: "no validator runs recorded; policy requires schema validation".into(),
        });
    }
    if let Some(failed) = validator_outcomes
        .iter()
        .find(|o| matches!(o, ValidatorOutcome::Failed { .. }))
    {
        let message = match failed {
            ValidatorOutcome::Failed { message } => message.clone(),
            _ => unreachable!(),
        };
        return Err(SandboxRefusal::OutputSchemaValidationFailed { reason: message });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ecaa_workflow_core::workflow_contracts::implementation::ReviewStatus;

    #[test]
    fn policy_to_flags_round_trips() {
        let policy = SandboxPolicy::default_strict();
        let flags = flags_from_policy(&policy);
        assert!(flags.network_none);
        assert!(flags.redact_secret_env);
        assert!(flags.host_fs_readonly);
        assert_eq!(flags.memory_limit_mb, Some(8192));
        assert_eq!(flags.wall_timeout_secs, Some(7200));
    }

    #[test]
    fn flags_render_as_cli_args() {
        let policy = SandboxPolicy::default_strict();
        let args = flags_from_policy(&policy).to_cli_args();
        assert!(args.iter().any(|a| a == "--network=none"));
        assert!(args.iter().any(|a| a == "--read-only"));
        assert!(args.iter().any(|a| a == "--memory=8192m"));
    }

    #[test]
    fn pre_dispatch_check_clears_humanreviewed_node() {
        let mut node = TaskNode::skeleton("g", "Generated");
        node.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: ReviewStatus::HumanReviewed,
            artifact_digest: Some("sha256:abc".into()),
        };
        let policy = SandboxPolicy::default_strict();
        let refusal = pre_dispatch_check(&node, &policy);
        // GeneratedCode + HumanReviewed = no refusals; needs_container_wrap is false because the
        // node is GeneratedCode, not Unimplemented/ManualProtocol.
        assert!(refusal.is_none(), "expected None, got {:?}", refusal);
    }

    #[test]
    fn pre_dispatch_check_refuses_unreviewed_node() {
        let mut node = TaskNode::skeleton("g", "Generated");
        node.implementation = Implementation::GeneratedCode {
            repository_ref: "git@example.com/repo".into(),
            review_status: ReviewStatus::Unreviewed,
            artifact_digest: None,
        };
        let policy = SandboxPolicy::default_strict();
        let refusal = pre_dispatch_check(&node, &policy).unwrap();
        assert!(refusal
            .sandbox_refusals
            .contains(&SandboxRefusal::StaticAnalysisRequired));
    }

    #[test]
    fn output_schema_validation_passes_when_disabled() {
        let mut policy = SandboxPolicy::default_strict();
        policy.validate_output_schema = false;
        let outcomes = vec![ValidatorOutcome::Failed {
            message: "x".into(),
        }];
        assert!(enforce_output_schema_validation(&policy, &outcomes).is_ok());
    }

    #[test]
    fn output_schema_validation_fails_when_validator_failed() {
        let policy = SandboxPolicy::default_strict();
        let outcomes = vec![ValidatorOutcome::Failed {
            message: "padj=1.5".into(),
        }];
        let res = enforce_output_schema_validation(&policy, &outcomes);
        assert!(matches!(
            res,
            Err(SandboxRefusal::OutputSchemaValidationFailed { .. })
        ));
    }

    #[test]
    fn output_schema_validation_passes_when_validator_passed() {
        let policy = SandboxPolicy::default_strict();
        let outcomes = vec![ValidatorOutcome::Passed];
        assert!(enforce_output_schema_validation(&policy, &outcomes).is_ok());
    }
}
