//! Append-only repair provenance log.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// One line in the repair provenance log: what was attempted on which failure
/// in which round, and the outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairLogEntry {
    /// Round index (0-based) in which the attempt happened.
    pub round: usize,
    /// Stable id of the failure that was attempted.
    pub failure_id: String,
    /// Repair class, as a string tag.
    pub class: String,
    /// Outcome tag (e.g. "applied", "needs_agent", "unrepairable").
    pub outcome: String,
    /// Free-form note describing the attempt.
    pub note: String,
}

/// Append `entry` as one JSON line to `<pkg>/runtime/repair-log.jsonl`.
pub fn append_repair_log(pkg: &Path, entry: &RepairLogEntry) -> anyhow::Result<()> {
    let runtime = pkg.join("runtime");
    std::fs::create_dir_all(&runtime)
        .with_context(|| format!("creating runtime dir at {}", runtime.display()))?;
    let path = runtime.join("repair-log.jsonl");
    let mut line = serde_json::to_string(entry).context("serializing repair log entry")?;
    line.push('\n');
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("appending to {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_accumulates_lines() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg = dir.path();
        for round in 0..3 {
            let entry = RepairLogEntry {
                round,
                failure_id: format!("id{round}"),
                class: "citation_fix".to_string(),
                outcome: "applied".to_string(),
                note: format!("round {round} note"),
            };
            append_repair_log(pkg, &entry).expect("append");
        }
        let path = pkg.join("runtime").join("repair-log.jsonl");
        let contents = std::fs::read_to_string(&path).expect("read log");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 3, "three appends must yield three lines");
        let first: RepairLogEntry = serde_json::from_str(lines[0]).expect("first line parses");
        assert_eq!(first.round, 0, "first entry round must be 0");
        assert_eq!(first.failure_id, "id0", "first entry id must round-trip");
    }
}
