//! Task re-run abstraction and the offline review-routing default.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// An instruction to re-run a task with a repair directive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairDirective {
    /// Task to re-run.
    pub task: String,
    /// Human/agent-readable instruction describing the needed repair.
    pub instruction: String,
}

/// Re-runs a task to satisfy a repair directive. Implementations may invoke an
/// agent, or (offline) route the need to human review.
pub trait TaskRunner {
    /// Attempt to satisfy `directive` against the package at `pkg`.
    fn rerun(&self, pkg: &Path, directive: &RepairDirective) -> anyhow::Result<()>;
}

/// Offline default `TaskRunner`: instead of invoking an agent, it appends the
/// directive as one JSON line to `<pkg>/runtime/repair-requests.jsonl` so that
/// agentic needs are surfaced for human review.
pub struct ReviewRoutingRunner;

impl TaskRunner for ReviewRoutingRunner {
    fn rerun(&self, pkg: &Path, directive: &RepairDirective) -> anyhow::Result<()> {
        let runtime = pkg.join("runtime");
        std::fs::create_dir_all(&runtime)
            .with_context(|| format!("creating runtime dir at {}", runtime.display()))?;
        let path = runtime.join("repair-requests.jsonl");
        let mut line = serde_json::to_string(directive).context("serializing repair directive")?;
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_routing_writes_jsonl_line() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pkg = dir.path();
        let runner = ReviewRoutingRunner;
        let d1 = RepairDirective {
            task: "deseq".to_string(),
            instruction: "rerun with corrected contrast".to_string(),
        };
        let d2 = RepairDirective {
            task: "equiv".to_string(),
            instruction: "re-check equivalence".to_string(),
        };
        runner.rerun(pkg, &d1).expect("first rerun");
        runner.rerun(pkg, &d2).expect("second rerun");

        let path = pkg.join("runtime").join("repair-requests.jsonl");
        let contents = std::fs::read_to_string(&path).expect("read jsonl");
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(lines.len(), 2, "two directives must append two lines");
        let parsed: RepairDirective = serde_json::from_str(lines[0]).expect("first line parses");
        assert_eq!(parsed, d1, "first directive must round-trip from jsonl");
    }
}
