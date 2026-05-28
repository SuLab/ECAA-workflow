//! v3 §4 / v4 §4 Round-2 closure (G1 / G13) — cross-session `Opaque`
//! semantic-type observation aggregator.
//!
//! Sibling to `cross_session_aggregator.rs` (which covers
//! `LocalExtension` types under the v4 P6/D4 graduation pathway). This
//! module covers the `Opaque` side that the LocalExtension graduation
//! pipeline does NOT track: `SemanticType::Opaque { description }`
//! produced by the compatibility engine on unprofiled / opaquely-typed
//! ports.
//!
//! Persistent `Opaque` observations across sessions are a
//! registry-improvement signal: an opaque hash that recurs across
//! ≥3 sessions or ≥10 occurrences within a window means the system
//! repeatedly hits the same un-modeled type, and the operator should
//! either:
//! - mint a `LocalExtension` for the type, or
//! - file an upstream-ontology issue requesting a term, or
//! - update the modality taxonomy's facet table.
//!
//! On-disk shape: append-style JSONL at the caller-supplied path
//! (typically `<sessions_dir>/_opaque_registry.jsonl`). One
//! `OpaqueRegistryEntry` per line. Writes go through whole-file
//! rewrite (last-write-wins under in-process `Mutex`); reads tolerate
//! malformed lines (skip silently — caller may switch on
//! `tracing::warn!` later).
//!
//! Threading: a single `OpaqueAggregator` instance is safe to share
//! across threads (interior `Mutex` serializes record_observation).
//! The aggregator is constructed per call site; multiple aggregators
//! sharing the same path serialize via the file system's last-write-
//! wins semantics — same model as `cross_session_aggregator`.

use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;
/// One entry persisted to `_opaque_registry.jsonl`. Tracks observation
/// counts, distinct sessions, and the producer→consumer ports that
/// surfaced the opaque type.
///
/// R6-U7: ts-rs export removed — only consumed server-side by the
/// `make doctor` opaque-registry aggregator; UI never reads this shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct OpaqueRegistryEntry {
    /// Stable hash of the opaque type's description (typically a
    /// `blake3:<hex>` content hash). Distinct hashes mean distinct
    /// opaque types even if the human-readable description differs by
    /// whitespace; equal hashes collapse into the same entry.
    pub opaque_hash: String,
    /// RFC3339 timestamp of the first observation.
    pub first_seen: String,
    /// RFC3339 timestamp of the most recent observation.
    pub last_seen: String,
    /// Cumulative number of observations (across all sessions).
    pub occurrence_count: u32,
    /// Distinct sessions in which this opaque type was observed. Used
    /// by `registry_improvement_candidates` to decide whether the type
    /// has crossed the "≥3 distinct sessions" threshold.
    pub session_ids: Vec<String>,
    /// Distinct (node, port) call sites that produced the opaque type.
    /// Useful for the operator's `make doctor` report so the prompt
    /// can name the offending stage.
    pub ports: Vec<OpaquePortRef>,
}

/// A (node, port) call site that produced an opaque-typed value.
///
/// R6-U7: ts-rs export removed alongside `OpaqueRegistryEntry`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
pub struct OpaquePortRef {
    /// Task / atom node id.
    pub node: String,
    /// Port name within that node.
    pub port: String,
}

/// Append-style aggregator over `_opaque_registry.jsonl`. One instance
/// per call site; interior `Mutex` guards in-process writes; on-disk
/// concurrency relies on tempfile-style atomic rewrite semantics from
/// `write_all`.
pub struct OpaqueAggregator {
    path: PathBuf,
    lock: Mutex<()>,
}

impl OpaqueAggregator {
    /// Open the aggregator over the supplied registry path. The file
    /// is created lazily on the first successful `record_observation`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Mutex::new(()),
        }
    }

    /// Returns the registry's on-disk path. Useful for tests and the
    /// `make doctor` summary that names the file the operator should
    /// inspect.
    pub fn registry_path(&self) -> &std::path::Path {
        &self.path
    }

    /// Record a single opaque-type observation. Creates the entry on
    /// first sight; increments `occurrence_count`, appends the
    /// session_id (de-duped), appends the (node, port) ref (de-duped),
    /// and updates `last_seen` on every subsequent call.
    ///
    /// `opaque_hash` — caller-supplied stable hash of the opaque
    /// description (e.g. `blake3:<hex>`). Distinct hashes are distinct
    /// entries.
    /// `session_id` — the chat session id whose composer surfaced the
    /// opaque type. Used to count distinct sessions for the
    /// registry-improvement threshold.
    /// `node`, `port` — call site that produced the opaque type.
    /// `timestamp` — RFC3339 timestamp; caller decides clock source so
    /// the function stays pure for tests.
    pub fn record_observation(
        &self,
        opaque_hash: &str,
        session_id: &str,
        node: &str,
        port: &str,
        timestamp: &str,
    ) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        let mut entries = self.load_all()?;
        if let Some(entry) = entries.iter_mut().find(|e| e.opaque_hash == opaque_hash) {
            entry.occurrence_count += 1;
            if !entry.session_ids.iter().any(|s| s == session_id) {
                entry.session_ids.push(session_id.to_string());
            }
            let port_ref = OpaquePortRef {
                node: node.to_string(),
                port: port.to_string(),
            };
            if !entry
                .ports
                .iter()
                .any(|p| p.node == port_ref.node && p.port == port_ref.port)
            {
                entry.ports.push(port_ref);
            }
            entry.last_seen = timestamp.to_string();
        } else {
            entries.push(OpaqueRegistryEntry {
                opaque_hash: opaque_hash.to_string(),
                first_seen: timestamp.to_string(),
                last_seen: timestamp.to_string(),
                occurrence_count: 1,
                session_ids: vec![session_id.to_string()],
                ports: vec![OpaquePortRef {
                    node: node.to_string(),
                    port: port.to_string(),
                }],
            });
        }
        self.write_all(&entries)
    }

    /// Load every entry from the registry file. Returns an empty Vec
    /// if the file does not yet exist. Malformed lines are skipped
    /// rather than failing the whole load (mirrors
    /// `cross_session_aggregator::load_all`).
    pub fn load_all(&self) -> std::io::Result<Vec<OpaqueRegistryEntry>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }
        let file = std::fs::File::open(&self.path)?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<OpaqueRegistryEntry>(&line) {
                Ok(entry) => entries.push(entry),
                Err(err) => {
                    tracing::warn!(
                        target: "opaque_aggregator",
                        path = %self.path.display(),
                        err = ?err,
                        "skipping malformed registry line"
                    );
                }
            }
        }
        Ok(entries)
    }

    /// Rewrite the registry file with the supplied entries. Used by
    /// `record_observation`. Ensures the parent directory exists,
    /// then truncates + rewrites — last-write-wins under concurrent
    /// access (same contract as `cross_session_aggregator`).
    fn write_all(&self, entries: &[OpaqueRegistryEntry]) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        for entry in entries {
            let json = serde_json::to_string(entry)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            writeln!(file, "{json}")?;
        }
        Ok(())
    }

    /// Return every entry that has crossed the registry-improvement
    /// thresholds.
    ///
    /// Threshold mirrors v4 D4 thresholds in
    /// `config/local-extension-graduation.yaml::graduation_thresholds`
    /// for cross-doc symmetry: an entry is a candidate when it has
    /// been observed in `>= session_threshold` distinct sessions AND
    /// its `last_seen` timestamp falls within the `days_window`
    /// rolling window from `Utc::now()`. Entries with malformed or
    /// unparseable `last_seen` timestamps are dropped (treated as
    /// stale) — matches the policy of `cross_session_aggregator`
    /// where corrupt JSONL lines are silently skipped.
    pub fn registry_improvement_candidates(
        &self,
        session_threshold: usize,
        days_window: u32,
    ) -> Vec<OpaqueRegistryEntry> {
        use chrono::{DateTime, Duration, Utc};
        let cutoff = Utc::now() - Duration::days(days_window as i64);
        self.load_all()
            .unwrap_or_default()
            .into_iter()
            .filter(|entry| entry.session_ids.len() >= session_threshold)
            .filter(|entry| {
                DateTime::parse_from_rfc3339(&entry.last_seen)
                    .map(|ts| ts.with_timezone(&Utc) >= cutoff)
                    .unwrap_or(false)
            })
            .collect()
    }
}

/// Concrete adapter from `ecaa_workflow_core::compatibility::engine::OpaqueObservationSink`
/// to the `OpaqueAggregator` JSONL-on-disk store. Holds a single
/// `Arc<OpaqueAggregator>` so the same instance is reused across the
/// session's lifetime; IO errors are logged but never propagated
/// (the trait contract is "best-effort, do not panic").
///
/// Constructed by `tools::rebuild_dag` and stored on `PlanningContext`
/// for the duration of each composition call.
///
/// `Debug` is implemented manually (not derived) because
/// `OpaqueAggregator` itself doesn't derive `Debug` — it holds an
/// interior `Mutex<()>` whose `Debug` impl is intentionally noisy.
/// The manual impl prints the registry path so operator logs stay
/// useful without leaking lock state.
pub struct OpaqueObservationSinkImpl {
    aggregator: std::sync::Arc<OpaqueAggregator>,
}

impl OpaqueObservationSinkImpl {
    /// Create from a shared `OpaqueAggregator` reference.
    pub fn new(aggregator: std::sync::Arc<OpaqueAggregator>) -> Self {
        Self { aggregator }
    }
}

impl std::fmt::Debug for OpaqueObservationSinkImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpaqueObservationSinkImpl")
            .field("registry_path", &self.aggregator.registry_path())
            .finish()
    }
}

impl ecaa_workflow_core::compatibility::engine::OpaqueObservationSink
    for OpaqueObservationSinkImpl
{
    fn record_opaque(
        &self,
        opaque_hash: &str,
        session_id: &str,
        node_id: &str,
        port_name: &str,
        timestamp: &str,
    ) {
        if let Err(e) = self.aggregator.record_observation(
            opaque_hash,
            session_id,
            node_id,
            port_name,
            timestamp,
        ) {
            tracing::warn!(
                target: "opaque_aggregator",
                error = %e,
                opaque_hash = %opaque_hash,
                session_id = %session_id,
                "OpaqueObservationSinkImpl::record_opaque IO error; observation dropped"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn record_then_load_round_trip() {
        let dir = TempDir::new().unwrap();
        let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        agg.record_observation("blake3:abc", "s1", "n", "p", "2026-05-12T00:00:00Z")
            .unwrap();
        // New instance reads the same file.
        let agg2 = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        let entries = agg2.load_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].opaque_hash, "blake3:abc");
        assert_eq!(entries[0].occurrence_count, 1);
    }

    #[test]
    fn distinct_hashes_are_distinct_entries() {
        let dir = TempDir::new().unwrap();
        let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        agg.record_observation("blake3:a", "s1", "n", "p", "2026-05-12T00:00:00Z")
            .unwrap();
        agg.record_observation("blake3:b", "s1", "n", "p", "2026-05-12T00:00:00Z")
            .unwrap();
        let entries = agg.load_all().unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn duplicate_port_refs_collapse() {
        let dir = TempDir::new().unwrap();
        let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        for _ in 0..3 {
            agg.record_observation("blake3:x", "s1", "n", "p", "2026-05-12T00:00:00Z")
                .unwrap();
        }
        let entries = agg.load_all().unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ports.len(), 1);
        assert_eq!(entries[0].session_ids.len(), 1);
        assert_eq!(entries[0].occurrence_count, 3);
    }

    #[test]
    fn last_seen_advances_on_repeat() {
        let dir = TempDir::new().unwrap();
        let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        agg.record_observation("blake3:x", "s1", "n", "p", "2026-05-10T00:00:00Z")
            .unwrap();
        agg.record_observation("blake3:x", "s2", "n", "p", "2026-05-12T00:00:00Z")
            .unwrap();
        let entries = agg.load_all().unwrap();
        assert_eq!(entries[0].first_seen, "2026-05-10T00:00:00Z");
        assert_eq!(entries[0].last_seen, "2026-05-12T00:00:00Z");
    }

    #[test]
    fn candidates_filter_by_session_threshold() {
        let dir = TempDir::new().unwrap();
        let agg = OpaqueAggregator::new(dir.path().join("_opaque_registry.jsonl"));
        agg.record_observation("blake3:lonely", "s1", "n", "p", "2026-05-12T00:00:00Z")
            .unwrap();
        for sid in &["s1", "s2", "s3"] {
            agg.record_observation("blake3:recurring", sid, "n", "p", "2026-05-12T00:00:00Z")
                .unwrap();
        }
        let candidates = agg.registry_improvement_candidates(3, 30);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].opaque_hash, "blake3:recurring");
    }
}
