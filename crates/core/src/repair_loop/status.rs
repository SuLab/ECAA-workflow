//! Terminal repair status and its persisted form.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use super::failure::{Failure, FailureSet};

/// Overall verdict at the end of a repair run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairVerdict {
    /// No unresolved failures remain.
    FullyPassing,
    /// A tolerable number of unresolved failures remain.
    MostlyPassing,
    /// Too many unresolved failures remain.
    Failing,
}

/// A single failure surfaced for human review, with a reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewItem {
    /// The unresolved failure.
    pub failure: Failure,
    /// Why it is being routed to review.
    pub why: String,
}

/// Terminal status of a repair run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairStatus {
    /// Overall verdict.
    pub verdict: RepairVerdict,
    /// Number of repair rounds executed.
    pub rounds: usize,
    /// Failures left for human review.
    pub review: Vec<ReviewItem>,
}

impl RepairStatus {
    /// Persist as pretty JSON to `<pkg>/runtime/repair-status.json`.
    pub fn persist(&self, pkg: &Path) -> anyhow::Result<()> {
        let runtime = pkg.join("runtime");
        std::fs::create_dir_all(&runtime)
            .with_context(|| format!("creating runtime dir at {}", runtime.display()))?;
        let path = runtime.join("repair-status.json");
        let json = serde_json::to_string_pretty(self).context("serializing repair status")?;
        let mut file =
            File::create(&path).with_context(|| format!("creating {}", path.display()))?;
        file.write_all(json.as_bytes())
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }
}

/// Build the terminal status from the final failure set. Unresolved failures
/// become review items. Verdict: `FullyPassing` if none remain, `Failing` if
/// more than `failing_threshold` remain, else `MostlyPassing`.
pub fn from_final(fs: &FailureSet, rounds: usize, failing_threshold: usize) -> RepairStatus {
    let review: Vec<ReviewItem> = fs
        .unresolved()
        .into_iter()
        .map(|f| ReviewItem {
            failure: f.clone(),
            why: format!("unresolved after repair ({:?})", f.status),
        })
        .collect();
    let verdict = if review.is_empty() {
        RepairVerdict::FullyPassing
    } else if review.len() > failing_threshold {
        RepairVerdict::Failing
    } else {
        RepairVerdict::MostlyPassing
    };
    RepairStatus {
        verdict,
        rounds,
        review,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::failure::{FailureSource, FailureStatus, RepairClass};

    fn mk(subject: &str, status: FailureStatus) -> Failure {
        let mut f = Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::CitationFix,
            "t",
            subject,
            "d",
        );
        f.status = status;
        f
    }

    #[test]
    fn fully_passing_when_no_unresolved() {
        let fs = FailureSet(vec![mk("a", FailureStatus::Resolved)]);
        let st = from_final(&fs, 3, 2);
        assert_eq!(
            st.verdict,
            RepairVerdict::FullyPassing,
            "all resolved => fully passing"
        );
        assert!(st.review.is_empty(), "no review items expected");
        assert_eq!(st.rounds, 3, "rounds must be carried through");
    }

    #[test]
    fn mostly_passing_within_threshold() {
        let fs = FailureSet(vec![
            mk("a", FailureStatus::Open),
            mk("b", FailureStatus::Resolved),
        ]);
        let st = from_final(&fs, 5, 2);
        assert_eq!(
            st.verdict,
            RepairVerdict::MostlyPassing,
            "one unresolved with threshold 2 => mostly passing"
        );
        assert_eq!(st.review.len(), 1, "exactly one review item");
        assert_eq!(
            st.review[0].failure.subject, "a",
            "open failure must be the review item"
        );
    }

    #[test]
    fn failing_above_threshold() {
        let fs = FailureSet(vec![
            mk("a", FailureStatus::Open),
            mk("b", FailureStatus::InReview),
            mk("c", FailureStatus::Open),
        ]);
        let st = from_final(&fs, 7, 2);
        assert_eq!(
            st.verdict,
            RepairVerdict::Failing,
            "three unresolved over threshold 2 => failing"
        );
        assert_eq!(st.review.len(), 3, "all unresolved become review items");
    }

    #[test]
    fn persist_writes_pretty_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let fs = FailureSet(vec![mk("a", FailureStatus::Open)]);
        let st = from_final(&fs, 1, 5);
        st.persist(dir.path()).expect("persist");
        let path = dir.path().join("runtime").join("repair-status.json");
        let contents = std::fs::read_to_string(&path).expect("read status");
        assert!(contents.contains("\n  "), "pretty json must be indented");
        let back: RepairStatus = serde_json::from_str(&contents).expect("round-trip");
        assert_eq!(back, st, "persisted status must round-trip");
    }
}
