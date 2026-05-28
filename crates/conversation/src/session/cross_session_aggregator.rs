//! v4 P6 / D4 — cross-session `LocalExtension` registry aggregator.
//!
//! Maintains `<sessions_dir>/_local_extension_registry.jsonl` — a
//! JSONL file with one entry per minted IRI carrying:
//! - first-mint timestamp + label/definition/parents snapshot.
//! - per-usage counters (`usage_count`).
//! - per-session counters (`unique_sessions` map).
//! - per-outcome history (`outcomes` vector of bools — true =
//!   succeeded).
//!
//! Two consumer surfaces:
//! 1. **`record_usage`** — called by `tools::intake` whenever a
//!    `SemanticType::LocalExtension` is minted. Idempotent on duplicate
//!    (session_id, iri) pairs in the same call.
//! 2. **`check_graduation`** — pure read; consults the registry +
//!    thresholds and returns a `GraduationCandidacy` when all three
//!    thresholds satisfy.
//!
//! On-disk shape: one `RegistryEntry` per line (JSONL). Writes go
//! through tempfile + rename for atomicity. Reads are tolerant of
//! malformed lines (skip with tracing warning).
//!
//! v3 P11 supersession: the cross-session `Unknown` signal lives in
//! `core::external_registry::registry_improvement` for `Opaque` types;
//! this module owns the `LocalExtension` side of the same signal +
//! adds the graduation pathway on top.

use std::collections::BTreeMap;
use std::path::PathBuf;

use scripps_workflow_core::local_extension_graduation::{
    ExistingLocalExtension, GraduationCandidacy, GraduationThresholds,
};

/// Cross-session registry of minted `LocalExtension` IRIs.
///
/// The registry file is global (one per sessions_dir); the aggregator
/// is constructed per call site. Concurrent writers serialize via the
/// tempfile + rename dance (last-write-wins).
pub struct CrossSessionAggregator {
    registry_path: PathBuf,
}

impl CrossSessionAggregator {
    /// Open the aggregator for a sessions directory.
    pub fn new(sessions_dir: PathBuf) -> Self {
        Self {
            registry_path: sessions_dir.join("_local_extension_registry.jsonl"),
        }
    }

    /// Returns the registry's on-disk path. Useful for tests + the
    /// `make doctor` report.
    pub fn registry_path(&self) -> &std::path::Path {
        &self.registry_path
    }

    /// Record a single LocalExtension usage. Creates the registry
    /// entry on first sight; updates `usage_count`, `unique_sessions`,
    /// and `outcomes` on subsequent calls.
    ///
    /// `succeeded` — outcome of the workflow run the extension
    /// appeared in. The conversation crate calls this with `true`
    /// when the session reached `Emitted`, `false` when it ended in
    /// `Blocked`. Used to compute `success_rate` for graduation
    /// thresholds.
    #[allow(clippy::too_many_arguments)]
    pub fn record_usage(
        &self,
        iri: &str,
        label: &str,
        definition: &str,
        parents: &[String],
        modality: &str,
        session_id: &str,
        succeeded: bool,
    ) -> std::io::Result<()> {
        let mut entries = self.load_all()?;
        let entry = entries
            .entry(iri.to_string())
            .or_insert_with(|| RegistryEntry {
                iri: iri.into(),
                first_minted_at: chrono::Utc::now().to_rfc3339(),
                label: label.into(),
                definition: definition.into(),
                proposed_parent_terms: parents.to_vec(),
                modality: modality.into(),
                usage_count: 0,
                unique_sessions: BTreeMap::new(),
                outcomes: vec![],
            });
        entry.usage_count += 1;
        entry
            .unique_sessions
            .entry(session_id.into())
            .and_modify(|c| *c += 1)
            .or_insert(1);
        entry.outcomes.push(succeeded);
        self.write_all(&entries)
    }

    /// Return every registered `LocalExtension` in a form the
    /// `core::local_extension_graduation::detect_duplicates` function
    /// accepts.
    pub fn list_existing(&self) -> std::io::Result<Vec<ExistingLocalExtension>> {
        Ok(self
            .load_all()?
            .into_values()
            .map(|e| ExistingLocalExtension {
                iri: e.iri,
                label: e.label,
                definition: e.definition,
                proposed_parent_terms: e.proposed_parent_terms,
            })
            .collect())
    }

    /// Check whether the named IRI has crossed all three graduation
    /// thresholds. Returns `Some(candidacy)` when eligible.
    ///
    /// `modality_primary_ontologies` — the modality's primary
    /// ontology prefixes from the v4 P1 coverage matrix. The first
    /// entry is used as the `graduation_target_ontology`; falls back
    /// to `"EDAM"` when the slice is empty.
    pub fn check_graduation(
        &self,
        iri: &str,
        thresholds: &GraduationThresholds,
        modality_primary_ontologies: &[String],
    ) -> Option<GraduationCandidacy> {
        let entries = self.load_all().ok()?;
        let entry = entries.get(iri)?;
        let success_rate = if entry.outcomes.is_empty() {
            0.0
        } else {
            entry.outcomes.iter().filter(|b| **b).count() as f32 / entry.outcomes.len() as f32
        };
        if entry.usage_count >= thresholds.min_usage_count
            && entry.unique_sessions.len() as u32 >= thresholds.min_unique_sessions
            && success_rate >= thresholds.min_success_rate
        {
            let target = modality_primary_ontologies
                .first()
                .cloned()
                .unwrap_or_else(|| "EDAM".to_string());
            Some(GraduationCandidacy {
                usage_count: entry.usage_count,
                unique_sessions: entry.unique_sessions.len() as u32,
                success_rate,
                graduation_target_ontology: target,
            })
        } else {
            None
        }
    }

    /// List every entry that currently satisfies graduation
    /// thresholds. Useful for the UI's graduation-candidate listing
    /// endpoint.
    pub fn list_graduation_candidates(
        &self,
        thresholds: &GraduationThresholds,
        modality_primary_ontologies: &[String],
    ) -> Vec<GraduationCandidateSummary> {
        let entries = match self.load_all() {
            Ok(e) => e,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::new();
        let target = modality_primary_ontologies
            .first()
            .cloned()
            .unwrap_or_else(|| "EDAM".to_string());
        for (_, entry) in entries {
            let success_rate = if entry.outcomes.is_empty() {
                0.0
            } else {
                entry.outcomes.iter().filter(|b| **b).count() as f32 / entry.outcomes.len() as f32
            };
            if entry.usage_count >= thresholds.min_usage_count
                && entry.unique_sessions.len() as u32 >= thresholds.min_unique_sessions
                && success_rate >= thresholds.min_success_rate
            {
                out.push(GraduationCandidateSummary {
                    iri: entry.iri.clone(),
                    label: entry.label.clone(),
                    usage_count: entry.usage_count,
                    unique_sessions: entry.unique_sessions.len() as u32,
                    success_rate,
                    graduation_target_ontology: target.clone(),
                });
            }
        }
        // Stable order: descending by usage_count, then by IRI for ties.
        out.sort_by(|a, b| {
            b.usage_count
                .cmp(&a.usage_count)
                .then_with(|| a.iri.cmp(&b.iri))
        });
        out
    }

    fn load_all(&self) -> std::io::Result<BTreeMap<String, RegistryEntry>> {
        if !self.registry_path.exists() {
            return Ok(BTreeMap::new());
        }
        let raw = std::fs::read_to_string(&self.registry_path)?;
        let mut map = BTreeMap::new();
        for line in raw.lines() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<RegistryEntry>(line) {
                Ok(entry) => {
                    map.insert(entry.iri.clone(), entry);
                }
                Err(e) => {
                    tracing::warn!(
                        target: "cross_session_aggregator",
                        path = %self.registry_path.display(),
                        err = ?e,
                        "skipping malformed line"
                    );
                }
            }
        }
        Ok(map)
    }

    fn write_all(&self, entries: &BTreeMap<String, RegistryEntry>) -> std::io::Result<()> {
        let mut buf = String::new();
        for entry in entries.values() {
            buf.push_str(&serde_json::to_string(entry).map_err(std::io::Error::other)?);
            buf.push('\n');
        }
        // Ensure the parent directory exists.
        if let Some(parent) = self.registry_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = self.registry_path.with_extension("jsonl.tmp");
        std::fs::write(&tmp, buf)?;
        std::fs::rename(&tmp, &self.registry_path)
    }
}

/// Summary of one graduation-candidate entry. Returned by
/// `list_graduation_candidates` and surfaced through the server's
/// `/graduation/candidates` endpoint to the UI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct GraduationCandidateSummary {
    /// IRI of the locally-extended term.
    pub iri: String,
    /// Human-readable label for the term.
    pub label: String,
    /// Total number of sessions that used this term.
    pub usage_count: u32,
    /// Number of distinct sessions that used this term.
    pub unique_sessions: u32,
    /// Fraction of using sessions that completed without a blocker.
    pub success_rate: f32,
    /// Target public ontology for graduation (e.g. EDAM IRI prefix).
    pub graduation_target_ontology: String,
}

/// One entry persisted to the registry file. Kept module-private —
/// callers consume `ExistingLocalExtension` or
/// `GraduationCandidateSummary` instead.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RegistryEntry {
    iri: String,
    first_minted_at: String,
    label: String,
    definition: String,
    proposed_parent_terms: Vec<String>,
    modality: String,
    usage_count: u32,
    unique_sessions: BTreeMap<String, u32>,
    outcomes: Vec<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn record_usage_creates_entry_on_first_call() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        agg.record_usage(
            "swfc:novel",
            "Novel Type",
            "A novel type",
            &["data:2603".to_string()],
            "single_cell_rnaseq",
            "session-a",
            true,
        )
        .unwrap();
        let entries = agg.list_existing().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].iri, "swfc:novel");
        assert_eq!(entries[0].label, "Novel Type");
    }

    #[test]
    fn graduation_thresholds_gate_candidacy() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        // 1 usage across 1 session — under thresholds.
        agg.record_usage("swfc:x", "X", "x", &[], "m", "s1", true)
            .unwrap();
        let thr = GraduationThresholds::default(); // 5 / 3 / 0.6
        assert!(agg.check_graduation("swfc:x", &thr, &[]).is_none());

        // 5 usages across 3 sessions, all succeeded — passes.
        for sid in &["s1", "s2", "s3"] {
            agg.record_usage("swfc:y", "Y", "y", &[], "m", sid, true)
                .unwrap();
        }
        agg.record_usage("swfc:y", "Y", "y", &[], "m", "s1", true)
            .unwrap();
        agg.record_usage("swfc:y", "Y", "y", &[], "m", "s2", true)
            .unwrap();
        let cand = agg.check_graduation("swfc:y", &thr, &[]).expect("eligible");
        assert!(cand.usage_count >= 5);
        assert!(cand.unique_sessions >= 3);
        assert!(cand.success_rate >= 0.6);
        assert_eq!(cand.graduation_target_ontology, "EDAM");
    }

    #[test]
    fn graduation_target_uses_primary_ontologies_first() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        for sid in &["s1", "s2", "s3"] {
            for _ in 0..2 {
                agg.record_usage("swfc:z", "Z", "z", &[], "m", sid, true)
                    .unwrap();
            }
        }
        let thr = GraduationThresholds::default();
        let cand = agg
            .check_graduation("swfc:z", &thr, &["GO".to_string(), "EDAM".to_string()])
            .expect("eligible");
        assert_eq!(cand.graduation_target_ontology, "GO");
    }

    #[test]
    fn list_graduation_candidates_orders_by_usage_count() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        // swfc:high: 10 usages across 3 sessions.
        for sid in &["s1", "s2", "s3"] {
            for _ in 0..4 {
                agg.record_usage("swfc:high", "H", "h", &[], "m", sid, true)
                    .unwrap();
            }
        }
        // swfc:low: 5 usages across 3 sessions.
        for sid in &["s1", "s2", "s3"] {
            agg.record_usage("swfc:low", "L", "l", &[], "m", sid, true)
                .unwrap();
            agg.record_usage("swfc:low", "L", "l", &[], "m", sid, true)
                .unwrap();
        }
        let thr = GraduationThresholds::default();
        let list = agg.list_graduation_candidates(&thr, &[]);
        assert!(list.len() >= 2);
        assert!(list[0].usage_count >= list[1].usage_count);
    }

    #[test]
    fn unknown_iri_returns_no_candidacy() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        let thr = GraduationThresholds::default();
        assert!(agg.check_graduation("swfc:missing", &thr, &[]).is_none());
    }

    #[test]
    fn round_trips_after_write_then_load() {
        let dir = tempdir().unwrap();
        let agg = CrossSessionAggregator::new(dir.path().to_path_buf());
        agg.record_usage("swfc:rt", "RT", "rt", &[], "m", "s1", true)
            .unwrap();
        // New aggregator instance reads the same file.
        let agg2 = CrossSessionAggregator::new(dir.path().to_path_buf());
        let entries = agg2.list_existing().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].iri, "swfc:rt");
    }
}
