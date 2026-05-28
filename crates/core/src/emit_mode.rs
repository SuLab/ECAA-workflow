//! ECAA emission mode (Aim 3A Arm B″ control).
//!
//! `Full` emits the full ECAA package shape with all 8 subgraphs
//! materialized as typed sidecars (default). `Conventional` emits
//! a competent conventional-documentation envelope (README +
//! analysis.ipynb + basic WRROC + CSVs) with no ECAA-specific
//! sidecars — this is the Arm B″ control package.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
/// EcaaMode discriminant.
pub enum EcaaMode {
    #[default]
    /// Full variant.
    Full,
    /// Conventional variant.
    Conventional,
}

impl EcaaMode {
    /// Parse `SWFC_ECAA_MODE`. Unknown / empty / unset → `Full`.
    pub fn from_env_str(s: Option<&str>) -> Self {
        match s.map(|x| x.trim().to_ascii_lowercase()).as_deref() {
            Some("conventional") => Self::Conventional,
            Some("full") | None | Some("") => Self::Full,
            Some(other) => {
                tracing::warn!(value = %other, "unknown SWFC_ECAA_MODE; falling back to Full");
                Self::Full
            }
        }
    }
}
