//! Reconstruct a read-only `Session` from an uploaded, emitted ECAA package.
//!
//! The reconstruction is a pure load — it sets state fields directly rather
//! than driving the state machine (`try_transition`), because there is no
//! intake/confirm/emit history to replay: the durable RO-Crate on disk IS the
//! artifact of a prior SME confirmation. A fresh `Session::new(false)` supplies
//! a new `audit_writer_secret` (the origin's secret never leaves its process),
//! so any re-verification of the imported package must run verifier-less.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use ecaa_workflow_core::decision_log::DecisionRecord;
use ecaa_workflow_core::package_import::reconstruct_workflow_dag_from_package;

use crate::session::state::{Session, SessionState, Turn};

impl Session {
    /// Build a read-only `Session` (state = `Emitted`, `imported = true`) over
    /// an already-extracted package directory. Reconstructs the DAG + task
    /// states from `WORKFLOW.json`/sidecars and the transcript/decisions from
    /// the runtime jsonl. Uses a fresh `audit_writer_secret` — the origin's
    /// never leaves its process, so reverify must run verifier-less (see the
    /// server handler).
    ///
    /// This is a load, not a state-machine transition: `state` is assigned
    /// directly. The caller (the server import handler) is responsible for
    /// persisting the returned session and gating every mutating endpoint on
    /// `session.imported`.
    pub fn from_imported_package(root: &Path) -> anyhow::Result<Self> {
        let mut s = Session::new(false);
        s.state = SessionState::Emitted;
        s.emitted_package_path = Some(root.to_path_buf());
        s.imported = true;

        let (workflow_dag, task_states) = reconstruct_workflow_dag_from_package(root)
            .context("reconstruct DAG from imported package")?;
        s.workflow_dag = Some(workflow_dag);
        s.task_states = task_states;

        s.conversation = Arc::new(read_conversation(root));
        s.decisions = read_decisions(root);
        Ok(s)
    }
}

/// `intake-conversation.jsonl` is N `Turn` lines followed by M `ToolCallRecord`
/// lines (written by `emit/audit_log.rs`). Only the `Turn` lines are the
/// transcript; `ToolCallRecord` lines fail `Turn` deserialization (no
/// `role`/`content`) and are dropped by `filter_map`.
fn read_conversation(root: &Path) -> Vec<Turn> {
    let path = root.join("runtime/intake-conversation.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<Turn>(l).ok())
        .collect()
}

/// `decisions.jsonl` is one `DecisionRecord` per line. Lines that don't parse
/// as a `DecisionRecord` (schema drift, truncated writes) are dropped rather
/// than failing the whole import — an imported package must always remain
/// explorable even if its decision log is partial.
fn read_decisions(root: &Path) -> Vec<DecisionRecord> {
    let path = root.join("runtime/decisions.jsonl");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<DecisionRecord>(l).ok())
        .collect()
}
