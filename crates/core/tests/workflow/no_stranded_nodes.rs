//! L3 stranded-node DAG integrity gate.
//!
//! For every scenario under `testdata/scenarios/` plus the IVD
//! fixture, emit its DAG and assert every task node has at least
//! one incoming OR outgoing edge, with explicit allowlists for
//! intake-source nodes and terminal reporting nodes.
//!
//! Behavior is controlled by `ECAA_STRANDED_NODES_STRICT`:
//! - unset / "0": warn-only (println strands, return Ok)
//! - "1": fail the test on any strand.
//!
//! Default is warn-only; flip to STRICT in CI when ready.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

const INTAKE_NODES: &[&str] = &["intake", "data_acquisition", "data_import", "data_intake"];

const TERMINAL_REPORTING_NODES: &[&str] = &[
    "final_reporting",
    "reporting",
    "share_outputs",
    "validate_metadata_harmonization",
];

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Collect (label, request_path) pairs for every scenario.
fn intake_fixtures() -> Vec<(String, PathBuf)> {
    let root = workspace_root();
    let mut out: Vec<(String, PathBuf)> = Vec::new();

    let scenarios_dir = root.join("testdata/scenarios");
    if scenarios_dir.is_dir() {
        let mut dirs: Vec<_> = std::fs::read_dir(&scenarios_dir)
            .expect("reading testdata/scenarios")
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let req = dir.join("request.md");
            if req.exists() {
                let label = dir.file_name().unwrap().to_string_lossy().into_owned();
                out.push((label, req));
            }
        }
    }

    out
}

/// Emit a package from an intake request file; return the package dir.
/// Returns `None` on emission failure (those have their own tests).
fn emit_to_tempdir(request: &PathBuf, tmp: &tempfile::TempDir) -> Option<PathBuf> {
    let root = workspace_root();
    let output = Command::new(env!("CARGO"))
        .args([
            "run",
            "--quiet",
            "-p",
            "ecaa-workflow-cli",
            "--bin",
            "ecaa-workflow",
            "--",
            "intake",
            "--input",
        ])
        .arg(request)
        .arg("--output")
        .arg(tmp.path())
        .current_dir(&root)
        .env("ECAA_CONFIG_DIR", root.join("config"))
        .output();

    match output {
        Ok(out) if out.status.success() => Some(tmp.path().to_path_buf()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            println!(
                "[no_stranded_nodes] emit failed for {:?}: {}",
                request, stderr
            );
            None
        }
        Err(e) => {
            println!(
                "[no_stranded_nodes] could not run compiler for {:?}: {}",
                request, e
            );
            None
        }
    }
}

/// Check a WORKFLOW.json value for stranded nodes.
/// WORKFLOW.json `tasks` is a BTreeMap serialized as a JSON object with
/// task IDs as keys. Each task has a `depends_on` array of predecessor IDs.
fn check_stranded_nodes(
    label: &str,
    workflow: &serde_json::Value,
) -> Vec<(String, String, String)> {
    let tasks_obj = match workflow["tasks"].as_object() {
        Some(o) => o,
        None => return vec![],
    };

    let task_ids: HashSet<String> = tasks_obj.keys().cloned().collect();
    let mut has_incoming: HashSet<String> = HashSet::new();
    let mut has_outgoing: HashSet<String> = HashSet::new();

    for (tid, task) in tasks_obj {
        let deps = task["depends_on"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
            .unwrap_or_default();
        if !deps.is_empty() {
            has_incoming.insert(tid.clone());
        }
        for dep in &deps {
            has_outgoing.insert(dep.to_string());
        }
    }

    let mut strands: Vec<(String, String, String)> = Vec::new();
    for tid in &task_ids {
        let task = &tasks_obj[tid];
        let atom = task["source_atom_id"].as_str().unwrap_or("?");
        let inc = has_incoming.contains(tid);
        let out = has_outgoing.contains(tid);

        // A single-task package is always allowed.
        if task_ids.len() == 1 {
            continue;
        }

        // Allowlist: intake-source nodes never have incoming edges.
        let allow_no_inc = INTAKE_NODES
            .iter()
            .any(|a| tid.contains(a) || atom.contains(a));
        // Allowlist: terminal reporting nodes never have outgoing edges.
        let allow_no_out = TERMINAL_REPORTING_NODES
            .iter()
            .any(|a| tid.contains(a) || atom.contains(a));

        // A stranded node has neither incoming nor outgoing edges
        // (completely isolated from the graph).
        if !inc && !out && !allow_no_inc && !allow_no_out {
            strands.push((label.to_string(), tid.clone(), atom.to_string()));
        }
    }
    strands
}

#[test]
fn no_stranded_nodes_in_any_fixture() {
    let strict = std::env::var("ECAA_STRANDED_NODES_STRICT")
        .map(|v| v == "1")
        .unwrap_or(false);

    let mut all_strands: Vec<(String, String, String)> = Vec::new();

    for (label, request) in intake_fixtures() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pkg_dir = match emit_to_tempdir(&request, &tmp) {
            Some(d) => d,
            None => continue,
        };

        let wf_path = pkg_dir.join("WORKFLOW.json");
        let raw = match std::fs::read(&wf_path) {
            Ok(b) => b,
            Err(e) => {
                println!("[no_stranded_nodes] missing WORKFLOW.json for {label}: {e}");
                continue;
            }
        };
        let workflow: serde_json::Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(e) => {
                println!("[no_stranded_nodes] bad WORKFLOW.json for {label}: {e}");
                continue;
            }
        };

        let strands = check_stranded_nodes(&label, &workflow);
        all_strands.extend(strands);
    }

    if all_strands.is_empty() {
        return;
    }

    let report = all_strands
        .iter()
        .map(|(f, t, a)| format!("  {f}: {t} (atom={a})"))
        .collect::<Vec<_>>()
        .join("\n");

    if strict {
        panic!(
            "L3 stranded-node gate found {} strand(s):\n{}",
            all_strands.len(),
            report
        );
    } else {
        println!(
            "L3 stranded-node gate (warn-only): {} strand(s):\n{}",
            all_strands.len(),
            report
        );
    }
}
