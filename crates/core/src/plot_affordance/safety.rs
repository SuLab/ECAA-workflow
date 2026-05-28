use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// PlotSafety discriminant.
pub enum PlotSafety {
    /// Catalog-registered, snapshot-tested renderer.
    Validated,
    /// Parent-term renderer; type-specific renderer not yet authored.
    InheritanceWarn,
    /// Universal structural primitive; legible but type-agnostic.
    Generic,
    /// LLM-drafted, sandboxed, snapshot-validated; not human-reviewed.
    Generated,
    /// No automatic renderer; an SME description is sent instead.
    None,
}
