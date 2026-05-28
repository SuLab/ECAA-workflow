//! ReexecutionBucket enum — 5-class re-execution outcome classification
//! (Q sub-graph `RerunOutcome.class`).

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// The five re-execution buckets per PAR-26-040 §Aim 3A primary endpoint.
///
/// On the wire (serde rename_all = "snake_case"):
///   `byte_identical` | `semantic_equivalent` | `acknowledged_non_determinism` |
///   `unavailable` | `failed`
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReexecutionBucket {
    ByteIdentical,
    SemanticEquivalent,
    AcknowledgedNonDeterminism,
    Unavailable,
    Failed,
}
