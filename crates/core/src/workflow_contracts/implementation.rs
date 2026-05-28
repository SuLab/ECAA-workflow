//! Implementation variants per design §7.
//!
//! All seven variants are declared in this enum. `ContainerCommand`
//! and `CompositeDag` are the active composer outputs. `GeneratedCode`
//! is used by the local sandbox executor. `WasmPlugin` and
//! `ExistingWorkflow` are reserved for external registry ingestion.
//! `ManualProtocol` covers SME-assignee atoms (typed variant rather
//! than a free-form description). `Unimplemented` is the default for
//! hypothesized nodes awaiting promotion.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Reference to an OCI image. Mirrors today's `atom::ContainerSpec`
/// shape so atom→TaskNode conversion is information-preserving.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct OciImageRef {
    /// Image reference (e.g. `ghcr.io/scripps/scripps-bio-base`).
    pub image: String,
    /// Tag.
    pub tag: String,
    /// SHA-256 digest (`sha256:...`). Empty until digest resolution
    /// at emit time per ADR 0025.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    /// Architecture allowlist; defaults to `["amd64"]` in the
    /// converter.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arch: Vec<String>,
    /// True when image needs GPU passthrough.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gpu: bool,
}

/// Reference to an entry in an external registry (bio.tools,
/// Dockstore, GA4GH TRS, etc.). Populated by the external registry
/// importer when `ExistingWorkflow` tasks are dispatched.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct RegistryRef {
    /// Registry kind (`bio_tools`, `dockstore`, `workflowhub`,
    /// `trs`, `local_cwl`, etc.).
    pub registry: String,
    /// Entry id within the registry.
    pub id: String,
    /// Optional version pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub version: Option<String>,
    /// Optional URL for human inspection / provenance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub url: Option<String>,
}

/// Review status of a generated implementation. Determines whether
/// the implementation may run in production or is restricted to
/// the sandbox executor.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, Default, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    /// Generated but not reviewed. May execute only in sandbox.
    #[default]
    Unreviewed,
    /// Auto-reviewed by static analysis (no human signoff).
    AutoReviewed,
    /// Human-reviewed and signed off.
    HumanReviewed,
    /// Reviewed and rejected — refuse production execution.
    Rejected,
}

/// What a node actually does. Design §7 — all seven variants. The
/// composer treats these uniformly for graph-correctness purposes;
/// the harness/agent dispatches per variant.
#[derive(
    Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema,
)]
#[ts(export)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Implementation {
    /// Reference to an existing pipeline in an external registry
    /// (CWL/WDL/Nextflow/Galaxy, Dockstore TRS).
    ExistingWorkflow { registry_ref: RegistryRef },
    /// A container image + command template. Today's atom default.
    ContainerCommand {
        /// Image.
        image: OciImageRef,
        /// Argv-style command template. Tokens like `${input.bam}`
        /// are resolved by the agent at dispatch.
        command_template: Vec<String>,
    },
    /// A subgraph that itself is a workflow DAG. Used by the
    /// composer when it routes a high-level task through a
    /// templated archetype rather than a single executable.
    CompositeDag { subgraph_id: String },
    /// A WASM plugin (deferred; reserved for future use).
    WasmPlugin { module_ref: String },
    /// Generated code in a local repository. Requires sandbox executor.
    GeneratedCode {
        /// Repository ref.
        repository_ref: String,
        /// Review status.
        review_status: ReviewStatus,
        /// Artifact digest (sha256) for byte-pinning.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        artifact_digest: Option<String>,
    },
    /// Manual protocol — SME action, no automation. Mirrors today's
    /// `AtomAssignee::Sme` atoms.
    ManualProtocol { sop_ref: String },
    /// Placeholder for the `HypothesizedNode` lifecycle. Hypothesized
    /// nodes carry this variant until promoted to a concrete implementation.
    #[default]
    Unimplemented,
}

impl Implementation {
    /// Stable variant key for byte-stable scoring tuples.
    pub fn variant_key(&self) -> &'static str {
        match self {
            Implementation::ExistingWorkflow { .. } => "existing_workflow",
            Implementation::ContainerCommand { .. } => "container_command",
            Implementation::CompositeDag { .. } => "composite_dag",
            Implementation::WasmPlugin { .. } => "wasm_plugin",
            Implementation::GeneratedCode { .. } => "generated_code",
            Implementation::ManualProtocol { .. } => "manual_protocol",
            Implementation::Unimplemented => "unimplemented",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_variants_round_trip() {
        let variants = vec![
            Implementation::ExistingWorkflow {
                registry_ref: RegistryRef {
                    registry: "dockstore".into(),
                    id: "scripps/dna-seq".into(),
                    version: Some("v1.0.0".into()),
                    url: None,
                },
            },
            Implementation::ContainerCommand {
                image: OciImageRef {
                    image: "ghcr.io/scripps/bio-base".into(),
                    tag: "v0.4.0".into(),
                    digest: String::new(),
                    arch: vec!["amd64".into()],
                    gpu: false,
                },
                command_template: vec!["align".into(), "${input.fastq}".into()],
            },
            Implementation::CompositeDag {
                subgraph_id: "rnaseq_basic".into(),
            },
            Implementation::WasmPlugin {
                module_ref: "scripps://wasm/normalizer".into(),
            },
            Implementation::GeneratedCode {
                repository_ref: "git@example.com/repo".into(),
                review_status: ReviewStatus::Unreviewed,
                artifact_digest: Some("sha256:abc".into()),
            },
            Implementation::ManualProtocol {
                sop_ref: "sop-2026-001".into(),
            },
            Implementation::Unimplemented,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let back: Implementation = serde_json::from_str(&json).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn variant_keys_are_stable() {
        let pairs = vec![
            (
                Implementation::ContainerCommand {
                    image: OciImageRef {
                        image: "x".into(),
                        tag: "y".into(),
                        digest: String::new(),
                        arch: vec![],
                        gpu: false,
                    },
                    command_template: vec![],
                },
                "container_command",
            ),
            (
                Implementation::CompositeDag {
                    subgraph_id: "x".into(),
                },
                "composite_dag",
            ),
            (Implementation::Unimplemented, "unimplemented"),
        ];
        for (impl_, key) in pairs {
            assert_eq!(impl_.variant_key(), key);
        }
    }
}
