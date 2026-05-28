//! Route: `GET /api/chat/session/:id/value-add-metrics`
//!
//! Reads `runtime/value-add-metrics.jsonl` from the session's emitted
//! package directory, groups rows by `tier`, keeps the latest row per
//! tier (last-wins on repeated runs), and returns an aggregated
//! `ValueAddMetricsResponse`.
//!
//! Returns 200 + null body when the session has not emitted yet or the
//! file does not exist (matching the `get_pilot_report` pattern) so the
//! UI tile can show an empty state rather than filling the console with
//! 404 errors.

use super::*;
use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// One aggregated tier result as exposed by the Performance tab tile.
/// Fields map directly to display columns in `ValueAddTile`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub(super) struct TierResult {
    /// Dotted tier identifier, e.g. `"15.1"`, `"16.2"`.
    pub tier: String,
    /// Single-letter eval bucket the tier belongs to: A, B, C, D, E, or F.
    pub bucket: String,
    /// Whether the most recent run for this tier passed its threshold.
    pub passed: bool,
    /// Primary score read out of the JSONL row.
    pub score: f64,
    /// Pass/fail threshold the runner used on its last run.
    pub threshold: f64,
    /// Unix epoch milliseconds of the most recent row for this tier.
    #[ts(type = "number")]
    pub last_run_ms: u64,
}

/// Top-level response for `GET /api/chat/session/:id/value-add-metrics`.
#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[ts(export)]
pub(super) struct ValueAddMetricsResponse {
    /// Latest result per tier, sorted by tier id (lexicographic).
    pub tier_results: Vec<TierResult>,
    /// Unix epoch milliseconds when the most recent row was written.
    #[ts(type = "number")]
    pub last_updated_ms: u64,
}

// ── JSONL row deserialisation ─────────────────────────────────────────────────

/// Flexible deserialiser for the heterogeneous JSONL rows written by the
/// tier bins. Each bin uses slightly different field names for the primary
/// score; we try a common set of aliases and fall back to 0.0 so the tile
/// renders something rather than crashing.
#[derive(Debug, Deserialize, Default)]
struct RawRow {
    #[serde(default)]
    tier: String,
    #[serde(default)]
    passed: bool,
    // Primary score aliases (each tier uses a different field name).
    #[serde(default)]
    score: Option<f64>,
    #[serde(default)]
    effectiveness: Option<f64>,
    #[serde(default)]
    mean_delta: Option<f64>,
    #[serde(default)]
    median_s: Option<f64>,
    #[serde(default)]
    median_rounds: Option<f64>,
    #[serde(default)]
    median_amendments: Option<f64>,
    #[serde(default)]
    pass_rate: Option<f64>,
    // Threshold aliases.
    #[serde(default)]
    threshold: Option<f64>,
    #[serde(default)]
    threshold_s: Option<f64>,
    #[serde(default)]
    threshold_rounds: Option<f64>,
    #[serde(default)]
    threshold_amendments: Option<f64>,
    /// Per-row emit timestamp written by the tier runner at the moment it
    /// appended the row. When present, used as `TierResult::last_run_ms`
    /// instead of the route handler's wall-clock `now_ms`. Absent in rows
    /// written before this field was introduced (graceful fallback).
    #[serde(default)]
    run_at_ms: Option<u64>,
}

impl RawRow {
    fn primary_score(&self) -> f64 {
        self.score
            .or(self.effectiveness)
            .or(self.mean_delta)
            .or(self.median_s)
            .or(self.median_rounds)
            .or(self.median_amendments)
            .or(self.pass_rate)
            .unwrap_or(0.0)
    }

    fn primary_threshold(&self) -> f64 {
        self.threshold
            .or(self.threshold_s)
            .or(self.threshold_rounds)
            .or(self.threshold_amendments)
            .unwrap_or(0.0)
    }
}

/// Derive the single-letter bucket label from a dotted tier id.
/// Convention (from the evaluation plan):
/// A — Tier 15.x (compiler correctness delta)
/// B — Tier 16.x (SME experience)
/// C — Tier 17.x (boundary enforcement)
/// D — Tier 18.x (provenance utility)
/// E — Tier 19.x (cross-session reproducibility)
/// F — Tier 0.x + Tier 20.x (claim-verifier rigor)
fn bucket_for_tier(tier: &str) -> String {
    let prefix = tier.split('.').next().unwrap_or("0");
    match prefix {
        "15" => "A",
        "16" => "B",
        "17" => "C",
        "18" => "D",
        "19" => "E",
        _ => "F",
    }
    .to_string()
}

// ── Handler ───────────────────────────────────────────────────────────────────

pub(super) async fn get_value_add_metrics(
    State(app): State<ChatAppState>,
    Path(session_id): Path<Uuid>,
) -> impl IntoResponse {
    let Some(session) = app.conversation.get_session(session_id).await else {
        return (StatusCode::NOT_FOUND, "session not found").into_response();
    };

    // Pre-emit sessions have no package directory and therefore no
    // value-add-metrics.jsonl. Return null (200) rather than 404 so the
    // UI polling does not generate console errors.
    let Some(root) = session.emitted_package_path.clone() else {
        return Json(serde_json::Value::Null).into_response();
    };

    let path = root.join("runtime").join("value-add-metrics.jsonl");

    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(_) => return Json(serde_json::Value::Null).into_response(),
    };

    // Parse JSONL → latest row per tier (last-wins).
    let mut by_tier: std::collections::BTreeMap<String, (RawRow, u64)> =
        std::collections::BTreeMap::new();

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    for (line_idx, line) in contents.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<RawRow>(trimmed) {
            Ok(row) if !row.tier.is_empty() => {
                // Use line position as a monotonic sequence number so the
                // last row in the file wins for a given tier id.
                let seq = line_idx as u64;
                by_tier.insert(row.tier.clone(), (row, seq));
            }
            _ => continue,
        }
    }

    let mut tier_results: Vec<TierResult> = by_tier
        .into_iter()
        .map(|(tier_id, (row, _seq))| TierResult {
            bucket: bucket_for_tier(&tier_id),
            passed: row.passed,
            score: row.primary_score(),
            threshold: row.primary_threshold(),
            last_run_ms: row.run_at_ms.unwrap_or(now_ms),
            tier: tier_id,
        })
        .collect();

    // Sort by tier id so the table rows are stable across polls.
    tier_results.sort_by(|a, b| a.tier.cmp(&b.tier));

    let last_updated_ms = if tier_results.is_empty() {
        now_ms
    } else {
        tier_results
            .iter()
            .map(|r| r.last_run_ms)
            .max()
            .unwrap_or(now_ms)
    };

    Json(ValueAddMetricsResponse {
        tier_results,
        last_updated_ms,
    })
    .into_response()
}

// ── Route inventory ───────────────────────────────────────────────────────────

pub(super) const ROUTES: &[(&str, &str)] = &[("GET", "/api/chat/session/:id/value-add-metrics")];

pub(super) fn routes() -> axum::Router<ChatAppState> {
    axum::Router::new().route(
        "/api/chat/session/:id/value-add-metrics",
        axum::routing::get(get_value_add_metrics),
    )
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_mapping_covers_all_known_prefixes() {
        assert_eq!(bucket_for_tier("15.1"), "A");
        assert_eq!(bucket_for_tier("15.5"), "A");
        assert_eq!(bucket_for_tier("16.1"), "B");
        assert_eq!(bucket_for_tier("16.5"), "B");
        assert_eq!(bucket_for_tier("17.1"), "C");
        assert_eq!(bucket_for_tier("18.1"), "D");
        assert_eq!(bucket_for_tier("19.1"), "E");
        assert_eq!(bucket_for_tier("0.5"), "F");
        assert_eq!(bucket_for_tier("20.1"), "F");
    }

    #[test]
    fn raw_row_picks_first_non_none_score_alias() {
        let row = RawRow {
            effectiveness: Some(0.82),
            mean_delta: Some(0.17),
            ..Default::default()
        };
        // `effectiveness` is checked before `mean_delta` in primary_score.
        assert!((row.primary_score() - 0.82).abs() < f64::EPSILON);
    }

    #[test]
    fn raw_row_falls_back_to_zero_when_all_absent() {
        let row = RawRow::default();
        assert_eq!(row.primary_score(), 0.0);
        assert_eq!(row.primary_threshold(), 0.0);
    }

    #[test]
    fn jsonl_parsing_last_row_wins_per_tier() {
        let jsonl = r#"{"tier":"15.1","passed":false,"effectiveness":0.50,"threshold":0.70}
{"tier":"15.1","passed":true,"effectiveness":0.75,"threshold":0.70}
{"tier":"16.1","passed":false,"median_s":2000.0,"threshold_s":1800.0}"#;

        let mut by_tier: std::collections::BTreeMap<String, (RawRow, u64)> =
            std::collections::BTreeMap::new();
        for (idx, line) in jsonl.lines().enumerate() {
            if let Ok(row) = serde_json::from_str::<RawRow>(line) {
                if !row.tier.is_empty() {
                    by_tier.insert(row.tier.clone(), (row, idx as u64));
                }
            }
        }
        // Last row for 15.1 should have passed=true.
        let (row_15_1, _) = by_tier.get("15.1").expect("15.1 present");
        assert!(row_15_1.passed);
        assert!((row_15_1.primary_score() - 0.75).abs() < f64::EPSILON);

        let (row_16_1, _) = by_tier.get("16.1").expect("16.1 present");
        assert!(!row_16_1.passed);
    }
}
