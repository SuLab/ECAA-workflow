//! Narrative-correction executor.
//!
//! Deterministic prose→table corrector for aggregate-count mismatches. When a
//! claim verifier flags that a narrative states a count of `N` while the frozen
//! result table actually has `M`, this executor rewrites the narrative so the
//! prose agrees with the table — it NEVER touches the table. The table is the
//! frozen source of truth; the only correctable side is the words.
//!
//! It refuses any failure whose detail is not exactly the `compare_count`
//! count-mismatch shape (see `claim_verifier::compare_count`). Anything else is
//! returned `Unrepairable` so it routes to review rather than being guessed.

use std::path::{Path, PathBuf};

use crate::repair_loop::executor::{Executor, RepairOutcome};
use crate::repair_loop::failure::{Failure, RepairClass};
use crate::repair_loop::runner::TaskRunner;

/// Mechanically corrects a narrative's aggregate count to match the frozen
/// result table. Deterministic; writes only the narrative file.
pub struct NarrativeCorrection;

/// The literal prefix every `compare_count` mismatch detail begins with.
/// See `crate::claim_verifier::compare_count`:
/// `"count claim: narrative says {N}, \`{table}\` has {M} ({what})"`.
const COUNT_PREFIX: &str = "count claim: narrative says ";

/// Parsed `(claimed_n, observed_m)` from a `compare_count` mismatch detail, or
/// `None` if `detail` is not that exact shape. Conservative: any deviation from
/// the known format yields `None` (routes to review) rather than a guess.
fn parse_count_detail(detail: &str) -> Option<(i64, usize)> {
    // "count claim: narrative says {N}, `{table}` has {M} ({what})"
    let rest = detail.strip_prefix(COUNT_PREFIX)?;
    // N is the run of digits immediately after the prefix, terminated by ','.
    let comma = rest.find(',')?;
    let n_str = rest[..comma].trim();
    let claimed_n: i64 = n_str.parse().ok()?;

    // The observed count follows the literal " has " that precedes the
    // back-tick-quoted table label. Take the LAST such marker so a stray
    // "has " inside the table label cannot mislead us.
    let after_comma = &rest[comma + 1..];
    let has_at = after_comma.rfind(" has ")?;
    let after_has = &after_comma[has_at + " has ".len()..];
    // M is the leading run of digits, terminated by the " (" that opens {what}.
    let m_end = after_has
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_has.len());
    let m_str = &after_has[..m_end];
    if m_str.is_empty() {
        return None;
    }
    let observed_m: usize = m_str.parse().ok()?;
    Some((claimed_n, observed_m))
}

/// Replicate `finalize::find_narrative_artifact`: locate the `.md`/`.txt`
/// narrative under the canonical `runtime/outputs/<task>/` (or legacy
/// `runtime/<task>/`) directory, ranked report > interpretation > summary.
fn find_narrative_artifact(pkg: &Path, task: &str) -> Option<PathBuf> {
    let canonical = pkg.join("runtime").join("outputs").join(task);
    let runtime_dir = if canonical.is_dir() {
        canonical
    } else {
        let legacy = pkg.join("runtime").join(task);
        if legacy.is_dir() {
            legacy
        } else {
            return None;
        }
    };

    let rd = std::fs::read_dir(&runtime_dir).ok()?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        let ext_lower = ext.to_ascii_lowercase();
        if ext_lower == "md" || ext_lower == "txt" {
            candidates.push(path);
        }
    }
    candidates.sort_by_key(|p| {
        let name = p
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.contains("report") {
            0
        } else if name.contains("interpretation") {
            1
        } else if name.contains("summary") {
            2
        } else {
            3
        }
    });
    candidates.into_iter().next()
}

/// Replace the FIRST standalone occurrence of `needle` (a decimal integer) in
/// `text` with `replacement`, returning the rewritten text, or `None` if no
/// standalone occurrence exists. "Standalone" means not flanked by an ASCII
/// digit or a decimal point on either side, so `9` does not match inside `19`,
/// `90`, or `1.9`.
fn replace_first_standalone_number(text: &str, needle: &str, replacement: &str) -> Option<String> {
    let needle_bytes = needle.as_bytes();
    let bytes = text.as_bytes();
    let mut search_from = 0usize;
    while let Some(rel) = text[search_from..].find(needle) {
        let start = search_from + rel;
        let end = start + needle.len();

        let before_ok = start == 0 || {
            let b = bytes[start - 1];
            !(b.is_ascii_digit() || b == b'.')
        };
        let after_ok = end == bytes.len() || {
            let b = bytes[end];
            !(b.is_ascii_digit() || b == b'.')
        };

        if before_ok && after_ok {
            let mut out = String::with_capacity(text.len() + replacement.len());
            out.push_str(&text[..start]);
            out.push_str(replacement);
            out.push_str(&text[end..]);
            return Some(out);
        }
        // Advance past this occurrence and keep looking.
        search_from = start + needle_bytes.len().max(1);
        if search_from >= text.len() {
            break;
        }
    }
    None
}

impl Executor for NarrativeCorrection {
    fn class(&self) -> RepairClass {
        RepairClass::NarrativeCorrection
    }

    fn repair(
        &self,
        f: &Failure,
        pkg: &Path,
        _config_dir: &Path,
        _runner: &dyn TaskRunner,
    ) -> RepairOutcome {
        let Some((claimed_n, observed_m)) = parse_count_detail(&f.detail) else {
            return RepairOutcome::Unrepairable("not a parseable count mismatch".to_string());
        };

        let Some(narrative_path) = find_narrative_artifact(pkg, &f.task) else {
            return RepairOutcome::Unrepairable(format!(
                "no narrative artifact found for task {}",
                f.task
            ));
        };

        let text = match std::fs::read_to_string(&narrative_path) {
            Ok(t) => t,
            Err(e) => {
                return RepairOutcome::Unrepairable(format!(
                    "reading narrative {}: {e}",
                    narrative_path.display()
                ));
            }
        };

        let n_str = claimed_n.to_string();
        let m_str = observed_m.to_string();
        let Some(corrected) = replace_first_standalone_number(&text, &n_str, &m_str) else {
            return RepairOutcome::Unrepairable(format!(
                "claimed count {claimed_n} not present as a standalone number in narrative {}",
                narrative_path.display()
            ));
        };

        if let Err(e) = std::fs::write(&narrative_path, corrected.as_bytes()) {
            return RepairOutcome::Unrepairable(format!(
                "writing corrected narrative {}: {e}",
                narrative_path.display()
            ));
        }

        RepairOutcome::Applied {
            deterministic: true,
            note: format!("corrected count {claimed_n}->{observed_m} in {}", f.task),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repair_loop::failure::FailureSource;
    use crate::repair_loop::runner::RepairDirective;
    use std::fs;

    /// Stub runner that must never be invoked: narrative correction is purely
    /// deterministic and never routes to an agent.
    struct NoRunner;
    impl TaskRunner for NoRunner {
        fn rerun(&self, _pkg: &Path, _directive: &RepairDirective) -> anyhow::Result<()> {
            Ok(())
        }
    }

    /// Build a package with a canonical narrative + a sibling table, returning
    /// `(tempdir, report_path, table_path)`.
    fn build_pkg(
        narrative_name: &str,
        narrative_body: &str,
        table_name: &str,
        table_body: &[u8],
    ) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let task_dir = dir.path().join("runtime").join("outputs").join("reporting");
        fs::create_dir_all(&task_dir).expect("create task dir");
        let report = task_dir.join(narrative_name);
        fs::write(&report, narrative_body).expect("write narrative");
        let table = task_dir.join(table_name);
        fs::write(&table, table_body).expect("write table");
        (dir, report, table)
    }

    fn failure_with_detail(detail: &str) -> Failure {
        Failure::new(
            FailureSource::ClaimMismatch,
            RepairClass::NarrativeCorrection,
            "reporting",
            "count_claim",
            detail,
        )
    }

    #[test]
    fn corrects_count_in_narrative_without_touching_table() {
        let (dir, report, table) = build_pkg(
            "report.md",
            "9 gene sets were significantly enriched at FDR < 0.05; see below.\n",
            "pw.tsv",
            b"id\tpadj\nGO:1\t0.01\nGO:2\t0.02\nGO:3\t0.03\n",
        );
        let table_before = fs::read(&table).expect("read table before");

        let f = failure_with_detail(
            "count claim: narrative says 9, `pw.tsv` has 3 (rows below the cited threshold)",
        );
        let outcome = NarrativeCorrection.repair(&f, dir.path(), dir.path(), &NoRunner);

        match outcome {
            RepairOutcome::Applied { deterministic, note } => {
                assert!(deterministic, "narrative correction must be deterministic");
                assert!(
                    note.contains("9->3"),
                    "note must record the count correction, got: {note}"
                );
            }
            other => panic!("expected Applied, got {other:?}"),
        }

        let after = fs::read_to_string(&report).expect("read narrative after");
        assert!(
            after.contains("3 gene sets"),
            "narrative must now state the observed count, got: {after}"
        );
        assert!(
            !after.contains("9 gene sets"),
            "the stale claimed count must be gone, got: {after}"
        );

        let table_after = fs::read(&table).expect("read table after");
        assert_eq!(
            table_before, table_after,
            "the frozen result table must be byte-for-byte unchanged (anti-gaming)"
        );
    }

    #[test]
    fn faithful_twin_non_count_detail_is_unrepairable_and_writes_nothing() {
        // A correct, well-formed narrative that must NOT be touched because the
        // failure is an effect-size mismatch, not a count mismatch.
        let body = "The effect size was 9.0 log2FC for the top gene.\n";
        let (dir, report, _table) = build_pkg(
            "report.md",
            body,
            "de.tsv",
            b"gene\tlog2fc\nA\t9.0\n",
        );

        let f = failure_with_detail(
            "effect size: narrative says 9.0000, table has 2.5000 (tolerance ±0.0500)",
        );
        let outcome = NarrativeCorrection.repair(&f, dir.path(), dir.path(), &NoRunner);

        assert!(
            matches!(outcome, RepairOutcome::Unrepairable(ref r) if r.contains("not a parseable count")),
            "non-count detail must be Unrepairable, got: {outcome:?}"
        );
        let after = fs::read_to_string(&report).expect("read narrative after");
        assert_eq!(after, body, "narrative must be left exactly as-is");
    }

    #[test]
    fn claimed_number_absent_from_narrative_is_unrepairable() {
        // Narrative never mentions the standalone number 9 — only 19 and 90,
        // which must NOT be matched (word-boundary safety).
        let body = "19 modules survived, covering 90 percent of variance.\n";
        let (dir, report, _table) = build_pkg(
            "report.md",
            body,
            "mod.tsv",
            b"module\nm1\nm2\nm3\n",
        );

        let f = failure_with_detail(
            "count claim: narrative says 9, `mod.tsv` has 3 (rows below threshold)",
        );
        let outcome = NarrativeCorrection.repair(&f, dir.path(), dir.path(), &NoRunner);

        assert!(
            matches!(outcome, RepairOutcome::Unrepairable(ref r) if r.contains("not present as a standalone number")),
            "absent claimed number must be Unrepairable, got: {outcome:?}"
        );
        let after = fs::read_to_string(&report).expect("read narrative after");
        assert_eq!(
            after, body,
            "narrative must be untouched when the claimed number is absent"
        );
    }

    #[test]
    fn standalone_replacement_does_not_touch_embedded_digits() {
        // Direct unit check of the word-boundary replacer: "9" must skip 19, 90,
        // 1.9 and land on the genuinely standalone 9.
        let text = "groups 19 and 90 differ; 1.9 fold; exactly 9 passed.";
        let out =
            replace_first_standalone_number(text, "9", "3").expect("standalone 9 must be found");
        assert_eq!(
            out, "groups 19 and 90 differ; 1.9 fold; exactly 3 passed.",
            "only the standalone 9 may be rewritten"
        );
    }

    #[test]
    fn parse_count_detail_extracts_n_and_m() {
        let (n, m) = parse_count_detail(
            "count claim: narrative says 12, `pw.tsv` has 4 (rows below the cited threshold)",
        )
        .expect("count detail must parse");
        assert_eq!((n, m), (12, 4), "N and M must come straight from the detail");
    }
}
