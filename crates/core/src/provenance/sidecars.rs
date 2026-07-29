//! Sync readers for the three provenance sidecars observed-read
//! reconciliation folds together (design §5.2 C5): the declared per-edge
//! graph (`runtime/proofs.jsonl`), the harness-observed reads
//! (`runtime/invocations.jsonl`), and the per-task `read_allowance`
//! facets (`runtime/task-nodes.json`).
//!
//! These parsers are the single source of truth shared by BOTH consumers
//! of the reconciliation pass:
//! - `crates/conversation/src/emit/ro_crate.rs` (initial + re-emit), and
//! - `crates/harness/src/end_of_run_finalize.rs` (post-exec finalize).
//!
//! They live in core — not conversation — because the harness cannot
//! depend on the conversation crate (CLAUDE.md crate layering: harness is
//! the orchestrator, conversation is chat-side), and core is the only
//! crate both link against. Stdlib-only / sync (no tokio) so the sync
//! harness can call them directly; the async conversation emit calls them
//! too — the reads are small, emit-time, one-shot.
//!
//! Every reader is fail-soft: a missing file returns empty (the common
//! case for a pre-dispatch package), and a malformed line is skipped
//! rather than aborting the whole parse — one bad line must not blind
//! reconciliation for every other line the emitter/harness wrote (mirrors
//! `crates/harness::observed_reads::read_manifest`'s tolerance).

use crate::atom::ReadAllowance;
use crate::provenance::ObservedRead;
use crate::workflow_contracts::edge::EdgeContract;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Parse `runtime/proofs.jsonl`'s `EdgeContract` rows (the declared
/// per-edge graph the v4 composer proved) for observed-provenance
/// reconciliation. A malformed line is skipped rather than aborting the
/// whole parse. Absent file (no v4 composition, or a pre-emit package)
/// returns empty.
pub fn read_declared_edges(package_root: &Path) -> Vec<EdgeContract> {
    let path = package_root.join("runtime/proofs.jsonl");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// One row of `runtime/invocations.jsonl` as seen by reconciliation —
/// only the field it needs. The harness's fuller `InvocationRecord`
/// (task_id, epoch, sandbox, container_image, …) is intentionally NOT
/// reused: `serde_json`'s default unknown-field tolerance means this
/// minimal shape reads the real file without needing the harness type,
/// keeping this parser free of a harness dependency.
#[derive(Deserialize)]
struct InvocationObservedReadsRow {
    #[serde(default)]
    observed_reads: Vec<ObservedRead>,
}

/// Fold every `observed_reads` entry out of `runtime/invocations.jsonl`.
/// A task may appear on more than one line — the harness's pre-dispatch
/// record (reads not yet known) plus a completion-time follow-up once
/// reads are captured — so every line's `observed_reads` is concatenated
/// rather than only the last. Absent file (no harness dispatch yet)
/// returns empty.
pub fn read_observed_reads(package_root: &Path) -> Vec<ObservedRead> {
    let path = package_root.join("runtime/invocations.jsonl");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<InvocationObservedReadsRow>(l).ok())
        .flat_map(|row| row.observed_reads)
        .collect()
}

/// One `TaskNode` as seen by reconciliation — only the fields it needs
/// (mirrors `InvocationObservedReadsRow`'s minimal-shape rationale).
/// Reads `runtime/task-nodes.json`, the typed `TaskNode` list
/// `emit::audit_log::write_phase14_sidecars` writes from
/// `session.workflow_dag.nodes`.
#[derive(Deserialize)]
struct TaskNodeReadAllowanceRow {
    id: String,
    #[serde(default)]
    attributes: BTreeMap<String, serde_json::Value>,
}

/// Parse `runtime/task-nodes.json` for each task's declared
/// `read_allowance` facet (`TaskNode::attributes["read_allowance"]`,
/// threaded there by `workflow_contracts::from_atom::preserve_attributes`
/// and, for a synthesized `validate_<id>` companion, inherited from its
/// producer by `composer_v4::companion_synthesis`). Absent file (no v4
/// composition) or a task with no declared allowance is simply omitted
/// from the returned map — `reconcile_ro_crate_edges_with_allowances`
/// treats a missing entry as "no allowance" and reconciles normally.
pub fn read_task_read_allowances(package_root: &Path) -> BTreeMap<String, Vec<ReadAllowance>> {
    let path = package_root.join("runtime/task-nodes.json");
    let Ok(body) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    let Ok(rows) = serde_json::from_str::<Vec<TaskNodeReadAllowanceRow>>(&body) else {
        return BTreeMap::new();
    };
    rows.into_iter()
        .filter_map(|row| {
            let raw = row.attributes.get("read_allowance")?.clone();
            let parsed: Vec<ReadAllowance> = serde_json::from_value(raw).ok()?;
            (!parsed.is_empty()).then_some((row.id, parsed))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atom::ReadAllowanceScope;
    use std::io::Write;

    fn write(package_root: &Path, rel: &str, body: &str) {
        let path = package_root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn read_declared_edges_absent_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_declared_edges(dir.path()).is_empty());
    }

    #[test]
    fn read_declared_edges_parses_and_skips_bad_lines() {
        use crate::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};
        let dir = tempfile::tempdir().unwrap();
        let edge = EdgeContract {
            from_node: "quantification".into(),
            from_port: "count_matrix".into(),
            to_node: "differential_expression".into(),
            to_port: "raw_counts".into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: Some("counts".into()),
        };
        let body = format!("not json\n{}\n", serde_json::to_string(&edge).unwrap());
        write(dir.path(), "runtime/proofs.jsonl", &body);
        let edges = read_declared_edges(dir.path());
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].from_node, "quantification");
        assert_eq!(edges[0].mutually_exclusive_group.as_deref(), Some("counts"));
    }

    #[test]
    fn read_observed_reads_folds_every_line() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "runtime/invocations.jsonl",
            // pre-dispatch line (no reads) + enriched follow-up (one read)
            "{\"schema_version\":\"0.1.0\",\"task_id\":\"differential_expression\",\"epoch\":1,\"harness_run_id\":\"r\",\"started_at\":\"t\",\"port_typed_inputs_satisfied\":true,\"sandbox\":\"none\",\"sandbox_required\":false,\"network_policy\":null}\n{\"schema_version\":\"0.1.0\",\"task_id\":\"differential_expression\",\"epoch\":1,\"harness_run_id\":\"r\",\"started_at\":\"t\",\"port_typed_inputs_satisfied\":true,\"sandbox\":\"none\",\"sandbox_required\":false,\"network_policy\":null,\"observed_reads\":[{\"task_id\":\"differential_expression\",\"declared_port\":\"raw_counts\",\"path\":\"runtime/outputs/quantification/count_matrix.tsv\"}]}\n",
        );
        let reads = read_observed_reads(dir.path());
        assert_eq!(reads.len(), 1);
        assert_eq!(
            reads[0].path,
            "runtime/outputs/quantification/count_matrix.tsv"
        );
        assert_eq!(reads[0].declared_port.as_deref(), Some("raw_counts"));
    }

    #[test]
    fn read_task_read_allowances_parses_attribute() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "runtime/task-nodes.json",
            "[{\"id\":\"final_reporting\",\"attributes\":{\"read_allowance\":[{\"scope\":\"any_upstream_stage\",\"rationale\":\"dashboard aggregation\"}]}}]",
        );
        let map = read_task_read_allowances(dir.path());
        let a = map.get("final_reporting").expect("allowance present");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0].scope, ReadAllowanceScope::AnyUpstreamStage);
        assert_eq!(a[0].rationale, "dashboard aggregation");
    }

    #[test]
    fn read_task_read_allowances_omits_task_without_allowance() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "runtime/task-nodes.json",
            "[{\"id\":\"alignment\",\"attributes\":{}}]",
        );
        assert!(read_task_read_allowances(dir.path()).is_empty());
    }
}
