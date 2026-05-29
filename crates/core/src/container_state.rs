//! Container-state sidecar.
//!
//! The agent wrappers (local / AWS / SLURM) write
//! `runtime/outputs/<task_id>/.container-state.json` after the
//! container exits, so the container-aware orphan reaper can
//! distinguish "container is hung inside a live host" from
//! "container exited cleanly but the host is still alive".
//!
//! This module is the canonical Rust reader: orphan reapers
//! deserialize the file via [`ContainerState::read_from_task_dir`] to
//! decide whether to reap the container only (preserving the
//! instance for retry) vs. tear the host down.
//!
//! The file is best-effort — agent scripts emit `|| true` after the
//! `cat <<EOF` so a missing or malformed file never poisons a task
//! exit. The reader matches: a missing file returns `Ok(None)` and a
//! malformed file returns `Err`. Callers tolerate both.

use crate::ids::TaskId;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Shape of `.container-state.json`. Matches the bash heredoc in
/// `scripts/agent-claude{,-aws,-slurm}.sh`. Forward-compatible via
/// `#[serde(default)]` on every field except `task_id`, which is
/// the load-bearing identity for the join with `WORKFLOW.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct ContainerState {
    /// Task id this state belongs to. Required.
    pub task_id: TaskId,
    /// Container exit code (the `claude` CLI's exit status).
    #[serde(default)]
    pub exit_code: i32,
    /// `image:tag` or `image@sha256:digest` resolved at run time.
    #[serde(default)]
    pub image: String,
    /// Runtime that ran the container — `docker`, `podman`,
    /// `apptainer`, `singularity`. Useful for the SLURM reaper which
    /// has no docker-label join key on the apptainer path.
    #[serde(default)]
    pub runtime: String,
    /// Session id the task belongs to. Empty when the agent ran
    /// outside a chat session (CLI-only path).
    #[serde(default)]
    pub session_id: String,
    /// Backend that ran the agent. Filled by the AWS + SLURM scripts;
    /// empty on local-host invocations.
    #[serde(default)]
    pub backend: String,
    /// ISO-8601 UTC timestamp of container exit. Comparable across
    /// agent invocations on the same host.
    #[serde(default)]
    pub ended_at: String,
}

impl ContainerState {
    /// Read `<task_dir>/.container-state.json` if present. Returns
    /// `Ok(None)` for a missing file (the common case during a task
    /// that is still running or pre-S15.22 emit), `Ok(Some(state))`
    /// for a well-formed sidecar, `Err` for a malformed sidecar so
    /// the caller can log + skip.
    pub fn read_from_task_dir(task_dir: &Path) -> std::io::Result<Option<Self>> {
        let path = task_dir.join(".container-state.json");
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)?;
        Self::parse_bytes(&bytes).map(Some).map_err(|e| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    ".container-state.json malformed at {}: {}",
                    path.display(),
                    e
                ),
            )
        })
    }

    /// Parse a sidecar from raw bytes. Used by remote probes that read
    /// the file over SSH or SSM RunShellScript and never touch the
    /// local filesystem.
    pub fn parse_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

/// Outcome of a remote probe against a (possibly stale) running task.
/// The orphan reaper consults this to decide whether to reap the
/// container only (preserving the host instance for retry) vs. tear
/// the host down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContainerProbeOutcome {
    /// `docker ps --filter label=ecaa-task=<id>` returned a row: the
    /// container is still alive on the host. Reaper should leave the
    /// host alone and reap the container only (for SLURM: cancel the
    /// step) because the host itself is healthy.
    ContainerAlive {
        /// Docker container id observed for the task label, when the
        /// runtime exposes one. Empty for apptainer (no container-id
        /// concept; `apptainer instance list` returns instance names).
        container_id: String,
        /// Runtime that reported the alive container — `docker`,
        /// `podman`, or `apptainer`.
        runtime: String,
    },
    /// Container exited (sidecar present, no live `docker ps` row).
    /// The reaper can use the recorded exit code to decide whether
    /// the host can be reused for a retry or should be torn down.
    ContainerExited {
        /// Recorded container state from the sidecar.
        state: ContainerState,
    },
    /// Neither a live container nor a sidecar was observed. The host
    /// is alive but the orphan reaper has no container-level signal —
    /// fall back to instance-level reap policy (the legacy path).
    NoSignal,
    /// Probe transport (SSM RunShellScript / SSH) failed. Reaper
    /// records the reason and falls back to instance-level reap.
    ProbeFailed {
        /// Human-readable reason the probe transport failed.
        reason: String,
    },
}

impl ContainerProbeOutcome {
    /// True when the probe found a still-alive container the reaper
    /// should target instead of the host.
    pub fn is_container_alive(&self) -> bool {
        matches!(self, ContainerProbeOutcome::ContainerAlive { .. })
    }

    /// True when the probe found a sidecar showing the container exited.
    pub fn has_exited_sidecar(&self) -> bool {
        matches!(self, ContainerProbeOutcome::ContainerExited { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_state(task_dir: &Path, body: &str) {
        std::fs::create_dir_all(task_dir).unwrap();
        std::fs::write(task_dir.join(".container-state.json"), body).unwrap();
    }

    #[test]
    fn missing_file_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = ContainerState::read_from_task_dir(tmp.path()).unwrap();
        assert!(
            result.is_none(),
            "missing .container-state.json must return Ok(None), got {:?}",
            result
        );
    }

    #[test]
    fn well_formed_state_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(
            tmp.path(),
            r#"{
                "task_id": "alignment",
                "exit_code": 0,
                "image": "ghcr.io/scripps/scripps-bio-base:1.4.4",
                "runtime": "docker",
                "session_id": "abc-123",
                "backend": "aws",
                "ended_at": "2026-05-05T12:34:56Z"
            }"#,
        );
        let parsed = ContainerState::read_from_task_dir(tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.task_id.as_str(), "alignment");
        assert_eq!(parsed.exit_code, 0);
        assert_eq!(parsed.runtime, "docker");
        assert_eq!(parsed.backend, "aws");
        assert_eq!(parsed.ended_at, "2026-05-05T12:34:56Z");
    }

    #[test]
    fn missing_optional_fields_default() {
        let tmp = tempfile::tempdir().unwrap();
        // task_id is the only required field; everything else falls
        // back to its `Default` so the legacy local-host (no
        // backend, no session_id) emit shape stays parseable.
        write_state(tmp.path(), r#"{"task_id": "qc_preprocessing"}"#);
        let parsed = ContainerState::read_from_task_dir(tmp.path())
            .unwrap()
            .unwrap();
        assert_eq!(parsed.task_id.as_str(), "qc_preprocessing");
        assert_eq!(parsed.exit_code, 0);
        assert_eq!(parsed.image, "");
        assert_eq!(parsed.runtime, "");
        assert_eq!(parsed.session_id, "");
        assert_eq!(parsed.backend, "");
    }

    #[test]
    fn malformed_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), "not json");
        let result = ContainerState::read_from_task_dir(tmp.path());
        assert!(
            result.is_err(),
            "malformed sidecar should err so the orphan reaper logs + skips"
        );
    }

    #[test]
    fn missing_required_task_id_returns_err() {
        let tmp = tempfile::tempdir().unwrap();
        write_state(tmp.path(), r#"{"exit_code": 0}"#);
        let result = ContainerState::read_from_task_dir(tmp.path());
        assert!(
            result.is_err(),
            "absent task_id is the load-bearing failure: orphan reaper has nothing to join on"
        );
    }

    #[test]
    fn parse_bytes_round_trip() {
        let bytes = br#"{
            "task_id": "qc",
            "exit_code": 137,
            "image": "ghcr.io/scripps/scripps-bio-base:1.4.4",
            "runtime": "apptainer",
            "session_id": "s",
            "backend": "slurm",
            "ended_at": "2026-05-05T18:00:00Z"
        }"#;
        let parsed = ContainerState::parse_bytes(bytes).unwrap();
        assert_eq!(parsed.task_id.as_str(), "qc");
        assert_eq!(parsed.exit_code, 137);
        assert_eq!(parsed.runtime, "apptainer");
    }

    #[test]
    fn probe_outcome_alive_classification() {
        let alive = ContainerProbeOutcome::ContainerAlive {
            container_id: "abc123".into(),
            runtime: "docker".into(),
        };
        assert!(alive.is_container_alive());
        assert!(!alive.has_exited_sidecar());
    }

    #[test]
    fn probe_outcome_exited_classification() {
        let exited = ContainerProbeOutcome::ContainerExited {
            state: ContainerState {
                task_id: "alignment".into(),
                exit_code: 0,
                image: String::new(),
                runtime: "docker".into(),
                session_id: String::new(),
                backend: "aws".into(),
                ended_at: String::new(),
            },
        };
        assert!(!exited.is_container_alive());
        assert!(exited.has_exited_sidecar());
    }

    #[test]
    fn probe_outcome_no_signal_classification() {
        let none = ContainerProbeOutcome::NoSignal;
        assert!(!none.is_container_alive());
        assert!(!none.has_exited_sidecar());
    }

    #[test]
    fn probe_outcome_serde_round_trip() {
        let alive = ContainerProbeOutcome::ContainerAlive {
            container_id: "deadbeef".into(),
            runtime: "docker".into(),
        };
        let json = serde_json::to_string(&alive).unwrap();
        let back: ContainerProbeOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(back, alive);
        assert!(json.contains(r#""kind":"container_alive""#));
    }
}
