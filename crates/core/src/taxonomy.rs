//! Stage-spec data types retained after the legacy taxonomy YAML
//! loader was retired when the 100-percent-closure
//! plan completed.
//!
//! Pre-deletion this module wrapped `config/stage-taxonomies/*.yaml` —
//! the legacy taxonomy DAG-build entry. With v4 archetype composition
//! covering every bare-modality / project-class case (B.1, B.2) and
//! `discover_*` companion synthesis covering the discovery flow (B.3),
//! the YAML directory + loader are gone.
//!
//! ### What remains here
//!
//! - **`StageSpec`** / **`StageCardinality`** / **`DiscoveryRequirement`** /
//!   **`RequiredArtifactSpec`** / **`SENSITIVITY_COMPARISON_CLASS`** /
//!   **`derive_role_from_id`** — load-bearing for the composer-driven
//!   build path (`composer::ComposedAtom → composed_atom_to_stage_spec
//! → emit_stage` in `builder.rs`); the in-memory stage representation
//!   the builder consumes.
//! - **`StageTaxonomy`** — a thin metadata holder. After B4 the loader
//!   is gone; the struct stays so existing session JSON (which embeds
//!   it via `Session.taxonomy: Option<StageTaxonomy>`) continues to
//!   round-trip. New sessions populate it from the matched
//!   `ArchetypeDefinition` rather than YAML on disk.
//!
//! A future cleanup may rename the module to `stage_spec.rs`; for now
//! the types stay where downstream consumers reference them as
//! `crate::taxonomy::*`.

use serde::{Deserialize, Serialize};

/// Phase B4 — metadata holder used by conversation
/// `Session.taxonomy` and `emit/mod.rs`. Pre-B4 this struct
/// with a YAML loader (`StageTaxonomy::load`) backed by
/// `config/stage-taxonomies/*.yaml`. With the YAML directory deleted
/// the loader is gone too; this struct stays so existing session JSON
/// keeps round-tripping and the post-emission `apply_checkpoint_mode`
/// + RO-Crate metadata threading don't have to change shape.
///
/// New sessions populate this from the matched `ArchetypeDefinition`:
/// `id` ← archetype id, `domain` ← `"computational biology"`,
/// `description` ← archetype description, `stages` ← composed atoms.
/// Legacy sessions persisted before B4 deserialize unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
pub struct StageTaxonomy {
    /// Id.
    pub id: String,
    #[serde(default)]
    /// Domain.
    pub domain: String,
    #[serde(default)]
    /// Description.
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Policies.
    pub policies: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Claim boundary.
    pub claim_boundary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Validation contract ref.
    pub validation_contract_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Project class.
    pub project_class: Option<crate::project_class::ProjectClass>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Preferred container.
    pub preferred_container: Option<String>,
    #[serde(default, skip_serializing_if = "is_empty_runtime_baseline")]
    /// Runtime baseline.
    pub runtime_baseline: crate::runtime_prereqs::RuntimePrereqs,
    #[serde(default)]
    /// Stages.
    pub stages: Vec<StageSpec>,
}

fn is_empty_runtime_baseline(p: &crate::runtime_prereqs::RuntimePrereqs) -> bool {
    p.system_packages.is_empty()
        && p.language_packages.is_empty()
        && p.system_check.is_empty()
        && p.base_image.is_none()
        && p.modality.is_none()
}

/// Stage class string for sensitivity-comparison stages. A stage with
/// `class: sensitivity_comparison` enumerates N variant runs and blocks
/// on a user selection via `select_sensitivity_winner`. The builder
/// treats these as a single logical task with the `variants` field
/// listing the variant method names; individual variant execution is a
/// harness concern.
pub const SENSITIVITY_COMPARISON_CLASS: &str = "sensitivity_comparison";

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// StageSpec data.
pub struct StageSpec {
    /// Id.
    pub id: String,
    /// Class.
    pub class: String,
    /// Discovery.
    pub discovery: DiscoveryRequirement,
    /// Depends on.
    pub depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Assignee.
    pub assignee: Option<String>,
    /// Description.
    pub description: String,
    /// Typed stage role. Stamped at construction time
    /// — composer-native stages copy from the underlying atom's
    /// `AtomRole`; pre-v4 stages were derived from the filename
    /// prefix (`discover_*` → `Discovery`, `validate_*` →
    /// `Validation`, `select_*` → `Selection`, otherwise
    /// `Operation`) via `derive_role_from_id`. Consumers should
    /// branch on `stage.role.default_behavior_class()` rather than
    /// the raw stage `id` prefix.
    #[serde(default)]
    pub role: crate::atom::AtomRole,
    #[serde(default)]
    /// Cardinality.
    pub cardinality: StageCardinality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Expansion source.
    pub expansion_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Expansion instructions.
    pub expansion_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Condition.
    pub condition: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Edam operation.
    pub edam_operation: Option<String>,
    /// Per-stage SME-reviewed method guidance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method_prose: Option<String>,
    /// Populated only when `class == sensitivity_comparison`. Each
    /// entry is a variant id — typically a method/parameter label —
    /// that the harness runs in parallel. `select_sensitivity_winner`
    /// resolves which variant becomes the stage's canonical result.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Hardware resource class hint. Accepted values: `cpu_heavy`,
    /// `io_heavy`, `memory_heavy`, `gpu`. Default (when absent) is
    /// `cpu_heavy`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_class: Option<String>,
    /// SME review gate. When true, the scheduler pauses dispatch on
    /// any task that depends transitively on this stage until the SME
    /// POSTs `/api/chat/session/:id/confirm { stage: "<id>" }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_sme_review: Option<bool>,
    /// Figure ids the agent must produce under
    /// `runtime/outputs/<stage_id>/figures/`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_figures: Vec<String>,
    /// Optional plotting module id to use when the emitted stage id is
    /// an alias or spelling variant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plot_stage_id: Option<String>,
    /// Headline output filenames the agent must write under
    /// `runtime/outputs/<stage_id>/`. Drives Tier-1 determinate
    /// progress in the per-task progress bar.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expected_artifacts: Vec<String>,
    /// Spec-preferred method ids the best-practice scorer should boost
    /// via the `spec_match` axis.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub spec_preferred_methods: std::collections::BTreeMap<String, String>,
    /// Stage-scoped claim-boundary sentence surfaced to the LLM during
    /// confirmation so the SME sees exactly what this stage's narrative
    /// may assert.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_boundary: Option<String>,
    /// `required` = always pauses on Selective/Gated; `recommended` =
    /// auto-advances under Selective. Fast auto-advances regardless.
    /// Absent defaults to Required (conservative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_level: Option<String>,
    /// Required artifacts the agent must produce before the harness
    /// considers the stage complete.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_artifacts: Vec<RequiredArtifactSpec>,

    /// Validation-obligation ids the harness runs
    /// against this stage's artifacts after task completion. Mirrors
    /// `AtomDefinition::validators`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<String>,

    /// When set, this stage is exempt from the figure-obligation lint.
    /// Mirrors `AtomDefinition::figure_exempt`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub figure_exempt: Option<crate::atom::FigureExempt>,
}

/// Required-artifact descriptor mirrored from `dag::RequiredArtifact`
/// but lives here so the YAML parser can deserialize it without a
/// cross-module dep. The builder copies validated entries onto
/// `Task.required_artifacts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct RequiredArtifactSpec {
    /// Relative path under `runtime/outputs/<stage_id>/`. Must not
    /// contain `..` segments (validated by the schema sidecar).
    pub path: String,
    /// Optional minimum file size. Zero = any non-empty file passes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_size_bytes: Option<u64>,
    /// Optional JSON-schema reference (path relative to the package
    /// root) used to validate artifact contents post-completion. Only
    /// checked when the file ends in `.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_ref: Option<String>,
}

impl StageSpec {
    /// True when this stage is a sensitivity comparison. Used by the
    /// builder + the AwaitingSmeSelection blocker flow to surface the
    /// variants.
    pub fn is_sensitivity_comparison(&self) -> bool {
        self.class == SENSITIVITY_COMPARISON_CLASS
    }
}

/// Derive a typed `AtomRole` from a stage id. Used by
/// callers that need to switch on stage role given only the id.
/// Composer-native stages set the role from the underlying atom's
/// `AtomRole` directly.
pub fn derive_role_from_id(id: &str) -> crate::atom::AtomRole {
    use crate::atom::AtomRole;
    if id.starts_with("discover_") {
        AtomRole::Discovery
    } else if id.starts_with("validate_") {
        AtomRole::Validation
    } else if id.starts_with("select_") {
        AtomRole::Selection
    } else {
        AtomRole::Operation
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
/// StageCardinality discriminant.
pub enum StageCardinality {
    #[default]
    /// One variant.
    One,
    /// PerSample variant.
    PerSample,
    /// Runtime-expanded iteration. The atom emits a
    /// 4-template-task scaffold at compile time
    /// (`iterate_gate_<id>`, `<id>` placeholder, `iterate_check_<id>`,
    /// `validate_<id>`) and the agent fans out a linear chain of
    /// `<id>_iter_N` tasks at runtime until the convergence metric
    /// drops below `iterate.convergence.threshold` for
    /// `iterate.convergence.consecutive_iterations` consecutive
    /// passes.
    IterateUntil,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
/// DiscoveryRequirement discriminant.
pub enum DiscoveryRequirement {
    /// None variant.
    None,
    /// LiteratureOnly variant.
    LiteratureOnly,
    /// EmpiricalRequired variant.
    EmpiricalRequired,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn sensitivity_comparison_class_predicate() {
        let stage = StageSpec {
            id: "compare_integration".into(),
            class: SENSITIVITY_COMPARISON_CLASS.into(),
            discovery: DiscoveryRequirement::None,
            depends_on: vec![],
            assignee: None,
            description: "Compare integration methods.".into(),
            role: crate::atom::AtomRole::Operation,
            cardinality: StageCardinality::default(),
            expansion_source: None,
            expansion_instructions: None,
            condition: None,
            edam_operation: None,
            method_prose: None,
            variants: vec!["harmony".into(), "scanorama".into(), "bbknn".into()],
            resource_class: None,
            requires_sme_review: None,
            required_figures: vec![],
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: vec![],
            spec_preferred_methods: BTreeMap::new(),
            claim_boundary: None,
            checkpoint_level: None,
            required_artifacts: vec![],
            validators: vec![],
        };
        assert!(stage.is_sensitivity_comparison());
        assert_eq!(stage.variants.len(), 3);
    }

    #[test]
    fn non_sensitivity_stage_has_empty_variants() {
        let yaml = r#"
id: x
class: preprocessing_qc
discovery: none
depends_on: []
description: qc
"#;
        let stage: StageSpec = serde_yml::from_str(yaml).unwrap();
        assert!(!stage.is_sensitivity_comparison());
        assert!(stage.variants.is_empty());
    }

    #[test]
    fn sensitivity_stage_deserializes_variants() {
        let yaml = r#"
id: compare_integration
class: sensitivity_comparison
discovery: none
depends_on: []
description: Compare integration methods
variants:
  - harmony
  - scanorama
  - bbknn
"#;
        let stage: StageSpec = serde_yml::from_str(yaml).unwrap();
        assert!(stage.is_sensitivity_comparison());
        assert_eq!(stage.variants, vec!["harmony", "scanorama", "bbknn"]);
    }

    #[test]
    fn derive_role_from_id_branches() {
        use crate::atom::AtomRole;
        assert!(matches!(
            derive_role_from_id("discover_batch_correction"),
            AtomRole::Discovery
        ));
        assert!(matches!(
            derive_role_from_id("validate_clusters"),
            AtomRole::Validation
        ));
        assert!(matches!(
            derive_role_from_id("select_winner"),
            AtomRole::Selection
        ));
        assert!(matches!(
            derive_role_from_id("alignment"),
            AtomRole::Operation
        ));
    }
}
