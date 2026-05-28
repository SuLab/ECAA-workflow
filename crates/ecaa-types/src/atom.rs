//! Atom-payload types referenced by `BlockerKind` variants.
//!
//! `NetworkPolicy` and `SandboxRequirement` are the two atom-safety enums
//! that appear directly inside `BlockerKind` variant payloads
//! (`NetworkPolicyMismatch`, `SandboxRequired`). They are extracted here
//! so the canonical `BlockerKind` binding can stand alone without pulling
//! the rest of `crates/core/src/atom.rs` (which is the compiler's
//! AtomDefinition / ContainerSpec catalog and has no place in a
//! downstream-consumer crate).
//!
//! Re-exported from `scripps_workflow_core::atom` for backward
//! compatibility with existing call sites.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Typed network policy on `ContainerSpec`.
/// `Bridge` inherits the harness network (the catalog default for most
/// archetypes). `None { allowlist }` is the strict default for the
/// clinical_trial archetype: `--network=none` plus a sidecar allowlist
/// of hostnames the entrypoint helper resolves at launch (allowlist
/// hostnames, not pinned IPs, so DNS rotation still works).
///
/// Note: most archetypes default to `Bridge` (the historical
/// behavior); the clinical_trial-specific `None`-by-default override
/// happens at archetype-load time, not via the `Default` impl. The
/// derived `Default` is here for ergonomic constructors only.
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkPolicy {
    /// Inherit the harness's default bridge network. The current
    /// behavior for non-clinical archetypes.
    #[default]
    Bridge,
    /// `--network=none` plus a hostname allowlist resolved by
    /// the entrypoint helper at task launch. Empty allowlist = full
    /// network isolation (no egress).
    None {
        #[serde(default)]
        allowlist: Vec<String>,
    },
}

/// Process-isolation requirement above the container's default.
#[derive(
    Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum SandboxRequirement {
    /// Container's default isolation only.
    #[default]
    None,
    /// Process-level sandbox (bubblewrap on Local; SLURM 25.11+
    /// `--container`; equivalent on AWS).
    ProcessIsolation,
    /// Hardware enclave (AWS nitro-enclave / equivalent). v1 accepts
    /// the variant; dispatch returns NotYetImplemented until wired.
    HardwareEnclave,
}
