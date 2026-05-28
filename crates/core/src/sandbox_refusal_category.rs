//! V4 categorical axis grouping the 12 `SandboxRefusal` kinds.
//! Drives UI dispatch and Tier 10.3 statistics. The `category()` method
//! on `SandboxRefusal` (in `sandbox_policy.rs`) produces these values.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// SandboxRefusalCategory discriminant.
pub enum SandboxRefusalCategory {
    /// Network variant.
    Network,
    /// Filesystem variant.
    Filesystem,
    /// Resource variant.
    Resource,
    /// Identity variant.
    Identity,
    /// Capability variant.
    Capability,
    /// SupplyChain variant.
    SupplyChain,
    /// OutputValidation variant.
    OutputValidation,
}
