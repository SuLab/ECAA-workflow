//! v3 P11 — cross-session `Unknown` registry-improvement aggregator.
//!
//! Lexical aggregator that walks on-disk session files and surfaces
//! recurring `LocalExtension` / `Opaque` types as a registry-
//! improvement signal. Designed to be invoked from `make doctor` so
//! operators see a one-line "you have 4 recurring opaque types — file
//! a registry-improvement issue" prompt rather than having to grep
//! `~/.ecaa-workflow/sessions/*` themselves.
//!
//! Supersession note: both sides of the v3 §4 "persistent Unknown =
//! registry-improvement signal" commitment now have purpose-built
//! aggregators in the conversation crate:
//! - `crates/conversation/src/session/cross_session_aggregator.rs`
//!   (v4 P6 / D4) carries `LocalExtension` IRIs with usage_count /
//!   unique_sessions / outcomes per IRI, and computes
//!   `LocalExtensionMaturity::GraduationCandidate` when the
//!   `config/local-extension-graduation.yaml` thresholds are met.
//! - `crates/conversation/src/session/opaque_aggregator.rs`
//!   (v3+v4 Round-2 G1 / G13 close-out) carries `Opaque` hashes
//!   across sessions with occurrence_count / session_ids / ports,
//!   and surfaces `registry_improvement_candidates` once the
//!   `config/_opaque-registry.schema.json` thresholds (≥3 distinct
//!   sessions) are met.
//!
//! The thin adapter below is no longer the load-bearing surface for
//! either side — both purpose-built aggregators ship typed `TS`-
//! exported entries and dedicated readers. The aggregator here
//! remains as a read-only fallback for `make doctor` reports that
//! walk legacy session JSON files predating the dedicated `.jsonl`
//! registries (no migration required — the aggregator runs over
//! whatever session files happen to exist on disk).
//!
//! Wiring:
//! - `aggregate_unknowns_from_paths` — pure function taking a slice of
//!   serialized session JSON values + a recurrence threshold.
//! - `aggregate_unknowns` — walks a directory, deserializes session
//!   files, and dispatches to the pure path.

use crate::workflow_contracts::semantic_type::SemanticType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use ts_rs::TS;

/// One registry-improvement signal surfaced from the recurring-
/// `Unknown` aggregator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RegistryImprovementSignal {
    /// Stable id (the offending `SemanticType.stable_id()` value).
    pub semantic_type_id: String,
    /// Variant key (`opaque` / `local_extension` / `ontology_term`).
    pub variant_key: String,
    /// Distinct sessions in which the type appeared.
    pub sessions_seen: Vec<String>,
    /// Latest session timestamp (RFC3339) seen — `None` when no
    /// session carried a `last_activity` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_seen_at: Option<String>,
    /// Human description (best-effort) — for `Opaque`, this is the
    /// stored reason; for `LocalExtension`, the definition. Truncated
    /// to 200 chars.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,
}

/// Build a sorted-by-id list of recurring-`Unknown` signals from a
/// list of (session_id, last_activity, semantic_types) tuples.
///
/// `min_sessions` — minimum unique session count for a type to surface
/// as a signal. The caller decides the threshold; `make doctor`
/// passes `3`.
///
/// The returned `Vec` is byte-stable: sorted by `semantic_type_id` for
/// deterministic CLI output.
pub fn aggregate_unknowns_from_inputs(
    inputs: &[AggregatorInput],
    min_sessions: u32,
) -> Vec<RegistryImprovementSignal> {
    // (variant_key, stable_id) → accumulator
    type Key = (String, String);
    let mut buckets: BTreeMap<Key, AggregatorBucket> = BTreeMap::new();
    for input in inputs {
        for st in &input.semantic_types {
            // Only `LocalExtension` and `Opaque` are "unknown-ish";
            // skip `OntologyTerm`.
            match st {
                SemanticType::OntologyTerm { .. } => continue,
                // Union members may individually be LocalExtension / Opaque; the
                // union itself is not a candidate for graduation and is skipped.
                SemanticType::LocalExtension { .. } | SemanticType::Opaque { .. } => {}
                SemanticType::Union { .. } => continue,
            }
            let key = (st.variant_key().to_string(), st.stable_id());
            let bucket = buckets.entry(key).or_default();
            bucket.sessions_seen.insert(input.session_id.clone());
            // Update last_seen_at if the input has a newer timestamp.
            match (&bucket.last_seen_at, input.last_activity.as_ref()) {
                (Some(prev), Some(cur)) if cur > prev => {
                    bucket.last_seen_at = Some(cur.clone());
                }
                (None, Some(cur)) => {
                    bucket.last_seen_at = Some(cur.clone());
                }
                _ => {}
            }
            if bucket.description.is_none() {
                bucket.description = description_of(st);
            }
        }
    }
    let mut out = Vec::new();
    for ((variant_key, stable_id), bucket) in buckets {
        if bucket.sessions_seen.len() as u32 >= min_sessions {
            let mut sessions: Vec<String> = bucket.sessions_seen.into_iter().collect();
            sessions.sort();
            out.push(RegistryImprovementSignal {
                semantic_type_id: stable_id,
                variant_key,
                sessions_seen: sessions,
                last_seen_at: bucket.last_seen_at,
                description: bucket.description,
            });
        }
    }
    out
}

/// Walk every `.json` file in the given sessions directory and
/// aggregate recurring `Unknown` types. Each session file's
/// `semantic_types` are extracted via `extract_semantic_types_from_value`
/// (lossy/best-effort — see the per-shape comments inside).
///
/// `min_sessions` — see `aggregate_unknowns_from_inputs`.
///
/// Returns an empty `Vec` for a missing or unreadable directory.
pub fn aggregate_unknowns(
    sessions_dir: &Path,
    min_sessions: u32,
) -> Vec<RegistryImprovementSignal> {
    let entries = match std::fs::read_dir(sessions_dir) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut inputs: Vec<AggregatorInput> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        // Skip the cross-session registry file maintained by v4 P6.
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('_') {
                continue;
            }
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let value: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let last_activity = value
            .get("last_activity")
            .and_then(|v| v.as_str())
            .map(String::from);
        let semantic_types = extract_semantic_types_from_value(&value);
        inputs.push(AggregatorInput {
            session_id,
            last_activity,
            semantic_types,
        });
    }
    aggregate_unknowns_from_inputs(&inputs, min_sessions)
}

/// Walk a serialized session value and collect every `SemanticType`
/// referenced under a `semantic_type` key. Best-effort traversal —
/// every key/value pair is inspected; non-`SemanticType`-shaped
/// objects under `semantic_type` are skipped via `serde_json::from_value`
/// returning `Err`.
fn extract_semantic_types_from_value(value: &serde_json::Value) -> Vec<SemanticType> {
    let mut out = Vec::new();
    walk(value, &mut out);
    out
}

fn walk(v: &serde_json::Value, out: &mut Vec<SemanticType>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(child) = map.get("semantic_type") {
                if let Ok(st) = serde_json::from_value::<SemanticType>(child.clone()) {
                    out.push(st);
                }
                // Even if not a SemanticType, keep walking — the value
                // may itself be a container with nested ports.
                walk(child, out);
            }
            for (k, child) in map {
                if k == "semantic_type" {
                    continue; // Already handled above.
                }
                walk(child, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                walk(item, out);
            }
        }
        _ => {}
    }
}

/// One session's contribution to the aggregator. Bundled together so
/// the pure function in `aggregate_unknowns_from_inputs` doesn't have
/// to take 3 parallel slices.
#[derive(Debug, Clone)]
pub struct AggregatorInput {
    /// Session id.
    pub session_id: String,
    /// Last activity.
    pub last_activity: Option<String>,
    /// Semantic types.
    pub semantic_types: Vec<SemanticType>,
}

#[derive(Debug, Default)]
struct AggregatorBucket {
    sessions_seen: std::collections::BTreeSet<String>,
    last_seen_at: Option<String>,
    description: Option<String>,
}

fn description_of(st: &SemanticType) -> Option<String> {
    let mut desc = match st {
        SemanticType::Opaque { description } => description.clone(),
        SemanticType::LocalExtension { definition, .. } => definition.clone(),
        SemanticType::OntologyTerm { .. } => return None,
        // Union types have no single description; callers handle graduation per-member.
        SemanticType::Union { .. } => return None,
    };
    desc.truncate(200);
    if desc.is_empty() {
        None
    } else {
        Some(desc)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(s: &str) -> SemanticType {
        SemanticType::opaque(s)
    }

    fn local_ext(ns: &str, id: &str, def: &str) -> SemanticType {
        SemanticType::LocalExtension {
            namespace: ns.into(),
            id: id.into(),
            proposed_parent_terms: vec![],
            definition: def.into(),
            maturity: crate::workflow_contracts::semantic_type::LocalExtensionMaturity::Minted,
        }
    }

    #[test]
    fn aggregator_surfaces_recurring_opaque_above_threshold() {
        let inputs = vec![
            AggregatorInput {
                session_id: "s1".into(),
                last_activity: Some("2026-05-10T00:00:00Z".into()),
                semantic_types: vec![op("profiler_failed")],
            },
            AggregatorInput {
                session_id: "s2".into(),
                last_activity: Some("2026-05-11T00:00:00Z".into()),
                semantic_types: vec![op("profiler_failed")],
            },
            AggregatorInput {
                session_id: "s3".into(),
                last_activity: None,
                semantic_types: vec![op("profiler_failed")],
            },
        ];
        let signals = aggregate_unknowns_from_inputs(&inputs, 3);
        assert_eq!(signals.len(), 1);
        assert!(signals[0].semantic_type_id.starts_with("opaque:"));
        assert_eq!(signals[0].variant_key, "opaque");
        assert_eq!(signals[0].sessions_seen.len(), 3);
        assert_eq!(
            signals[0].last_seen_at.as_deref(),
            Some("2026-05-11T00:00:00Z")
        );
    }

    #[test]
    fn aggregator_threshold_filters_singletons() {
        let inputs = vec![AggregatorInput {
            session_id: "s1".into(),
            last_activity: None,
            semantic_types: vec![op("one-off")],
        }];
        let signals = aggregate_unknowns_from_inputs(&inputs, 3);
        assert!(signals.is_empty());
    }

    #[test]
    fn aggregator_skips_ontology_terms() {
        let inputs = vec![
            AggregatorInput {
                session_id: "s1".into(),
                last_activity: None,
                semantic_types: vec![SemanticType::edam("data:0863", "BAM")],
            },
            AggregatorInput {
                session_id: "s2".into(),
                last_activity: None,
                semantic_types: vec![SemanticType::edam("data:0863", "BAM")],
            },
            AggregatorInput {
                session_id: "s3".into(),
                last_activity: None,
                semantic_types: vec![SemanticType::edam("data:0863", "BAM")],
            },
        ];
        let signals = aggregate_unknowns_from_inputs(&inputs, 3);
        assert!(signals.is_empty());
    }

    #[test]
    fn aggregator_handles_local_extension_recurrence() {
        let inputs = vec![
            AggregatorInput {
                session_id: "s1".into(),
                last_activity: None,
                semantic_types: vec![local_ext("ecaax", "novel_x", "Novel score X")],
            },
            AggregatorInput {
                session_id: "s2".into(),
                last_activity: None,
                semantic_types: vec![local_ext("ecaax", "novel_x", "Novel score X")],
            },
        ];
        let signals = aggregate_unknowns_from_inputs(&inputs, 2);
        assert_eq!(signals.len(), 1);
        assert_eq!(signals[0].variant_key, "local_extension");
        assert_eq!(signals[0].semantic_type_id, "ecaax:novel_x");
    }

    #[test]
    fn output_is_byte_stable_sorted_by_id() {
        let inputs = vec![
            AggregatorInput {
                session_id: "a".into(),
                last_activity: None,
                semantic_types: vec![op("zzz"), op("aaa")],
            },
            AggregatorInput {
                session_id: "b".into(),
                last_activity: None,
                semantic_types: vec![op("zzz"), op("aaa")],
            },
        ];
        let signals = aggregate_unknowns_from_inputs(&inputs, 2);
        assert_eq!(signals.len(), 2);
        // BTreeMap iteration ordered keys → sorted output.
        assert!(signals[0].semantic_type_id.lt(&signals[1].semantic_type_id));
    }

    #[test]
    fn walk_extracts_nested_semantic_types() {
        let v = serde_json::json!({
            "ports": [
                {"semantic_type": {"kind": "opaque", "description": "x"}},
                {"semantic_type": {"kind": "opaque", "description": "y"}},
            ],
            "nested": {
                "inner": {"semantic_type": {"kind": "opaque", "description": "z"}}
            }
        });
        let found = extract_semantic_types_from_value(&v);
        assert_eq!(found.len(), 3);
    }
}
