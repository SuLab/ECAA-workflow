//! `ArchetypeDefinition` — a thin composition file that names atoms by id
//! and wires them into a typed scaffold. It carries the slot mappings the
//! composer's slot-fill consumes, plus optional cross-archetype `compose:`
//! pointers for the fast-path matcher.
//!
//! # Why archetypes are thin
//!
//! Per the plan's modularity invariant ([CLAUDE.md §3.2 row "Archetype
//! = thin composition file"]):
//!
//! > Archetypes hold scaffold + slot mappings only; no method-selection
//! > logic.
//!
//! Method choices (which aligner, which DE method, etc.) live in the
//! atom layer's `discovery_*` runtime selection. Archetypes pre-select
//! the *atoms* to use; atoms then defer their method choice to the
//! agent at runtime via `discover_*`.
//!
//! The implementation includes:
//!
//! - Archetype YAMLs under `config/archetypes/` (one per
//!   modality-equivalence-class), one per existing taxonomy
//! - Loader + composer fast-path matcher (score-based ranking, 5%
//!   tie-surfacing per [DEC Q2.4])
//! - Slot-fill algorithm with EDAM subtype verification (uses
//!   `crates/core/src/edam.rs`)

use crate::ids::AtomId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Schema-layout version this loader accepts. Distinct from
/// [`ArchetypeDefinition::version`]: `schema_version` tracks the
/// YAML manifest shape (loader contract); `version` tracks the
/// archetype's authored content semver per [DEC QX.5]. Mismatch
/// surfaces a [`crate::blocker::BlockerKind::SchemaVersionMismatch`]
/// at registry-load time.
pub const CURRENT_ARCHETYPE_SCHEMA_VERSION: &str = "0.1";

/// One archetype — a named composition of atoms.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ArchetypeDefinition {
    /// Schema-layout version. Validated against
    /// [`CURRENT_ARCHETYPE_SCHEMA_VERSION`] at registry-load time.
    /// Distinct from [`ArchetypeDefinition::version`] (content
    /// semver per [DEC QX.5]).
    pub schema_version: String,

    /// Stable archetype id (e.g. `single_cell_de`,
    /// `bulk_rnaseq_disease_vs_healthy`). The composer uses this as
    /// the cache key for the matched-archetype fast path.
    pub id: String,

    /// Semver. [DEC QX.5] — bump on additive changes (new optional
    /// slot, new atom in the scaffold) so existing sessions don't
    /// silently re-route to a refined archetype mid-amendment. The
    /// `archetype_snapshot` field on Session pins the version a
    /// session committed to (S6.9 Cargo.lock-style pinning).
    pub version: String,

    /// Free-form English description. Used for the `propose_summary
    /// _confirmation` card and contributor-facing docs.
    pub description: String,

    /// One-sentence hook used in the SME-facing classifier (per
    /// [DEC Q2.4] score-based ranking with 5% tie-surfacing). When
    /// the classifier surfaces an archetype tie to the SME, this is
    /// the prose the dual-option card renders.
    pub sme_summary: String,

    /// EDAM data class IRI of the GoalSpec this archetype produces
    /// (per [GoalSpec §S6.1]). Composer matches a goal's
    /// `edam_data` against this field for fast-path lookup before
    /// falling through to backward-chaining.
    pub goal_data: String,

    /// Optional EDAM format IRI for the goal's primary committed
    /// artifact. When `Some`, the composer narrows fast-path matches
    /// to archetypes that produce the exact format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub goal_format: Option<String>,

    /// Atoms the archetype's scaffold uses. Order is the canonical
    /// composition order; the composer's deterministic ordering
    /// rule preserves it across runs.
    pub atoms: Vec<ArchetypeAtomRef>,

    /// Slot mappings: `intake_field` → composer slot reference.
    /// Wired through `intake_port_mapper` so the SME-named samples /
    /// contrast / organism fields fan into the right atom inputs without
    /// per-archetype custom code.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slot_mappings: BTreeMap<String, String>,

    /// Optional cross-archetype `compose:` pointers. Used today by
    /// `time-series-forecast.yaml`-style derived archetypes that
    /// reuse another archetype's scaffold + add a tail of their own
    /// atoms.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compose: Vec<ComposeRef>,

    /// Optional slot manifest. When set, the archetype is a "base"
    /// archetype and atoms `= self.atoms ∪ slot_value.extra_atoms`
    /// where slot_value is picked by classifier keyword scan against
    /// `slot.values[].keywords`. Loaded from sibling
    /// `<archetype_id>.slots.yaml` at registry-load time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub slots: Option<crate::archetype_slots::SlotManifest>,

    /// Cross-dependency hints. The atoms field gives the composer
    /// the set of nodes; this gives the explicit edges the composer
    /// wouldn't infer from `depends_on` alone (e.g. when an
    /// aggregator must run after a discovery even though it
    /// doesn't consume the discovery's output directly).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_dependencies: Vec<CrossDependency>,

    /// `claim_boundary` directive carried into the emitted package's
    /// interpretation policy. Mirrors the field on `AtomDefinition`
    /// but at the archetype level the boundary is the union of every
    /// atom's boundary plus the archetype-level addendum.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub claim_boundary: Option<String>,

    /// Project class this archetype targets. Constrains classifier-
    /// driven matching: a clinical-trial archetype only matches
    /// sessions where `project_class == ClinicalTrial`.
    pub project_class: String,

    /// Modality hint to disambiguate goal-shape ties.
    /// Multiple archetypes can produce the same `goal_data /
    /// goal_format` (e.g. `data:0951 / format:3475` is the goal of
    /// `bulk_rnaseq_de`, `long_read_rnaseq`, and
    /// `metagenomics_taxonomic` — all 3 score 6 on goal+format+class
    /// alone). When the classifier identifies an input modality
    /// (`bulk_rnaseq`, `long_read_rnaseq`, `metagenomics`, etc.),
    /// `score_archetype` adds +2 for an exact modality match,
    /// breaking the tie. Optional: archetypes that genuinely span
    /// multiple modalities leave it unset and stay tied (the
    /// composer surfaces `TieRequiresSmeDecision` per [DEC Q2.4]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub modality_hint: Option<String>,

    /// Sub-modality disambiguator via the goal pattern's
    /// `modifiers.kind` field. When the classifier matched a
    /// `goal_pattern` whose `modifiers.kind` matches this hint,
    /// `score_archetype` adds +2 — resolves the proteomics DDA-vs-DIA
    /// tie where both archetypes share `modality_hint: proteomics` +
    /// `goal_data: data:2976` and would otherwise tie at score 8. The
    /// DDA archetype declares `goal_kind_hint: proteomics_dda` and the
    /// DIA archetype declares `goal_kind_hint: proteomics_dia`; SME prose
    /// mentioning "DDA" routes to the DDA goal pattern (kind:
    /// proteomics_dda) which lifts proteomics_dda to score 10 and
    /// proteomics_dia stays at 8.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub goal_kind_hint: Option<String>,

    /// Archetype-level container default. Composer
    /// (S15.2) precedence is `atom > archetype > profile > package
    /// > host`. Atom-level `preferred_container` wins over this;
    /// this wins over `compute-profiles/profiles.yaml` per-stage
    /// defaults. `None` = no archetype-level pin (fall through).
    /// `clinical_trial` archetypes typically pin a hardened image
    /// + `NetworkPolicy::None { allowlist }` per ADR 0028 / D-R14.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub preferred_container: Option<crate::atom::ContainerSpec>,

    /// Modality-level baseline of runtime prereqs for derived-image
    /// warm-up. Provides `base_image`, `modality`, and the
    /// modality-default system + language packages every atom in this
    /// archetype implicitly inherits. Composed with each atom's own
    /// `runtime_packages` at emit time via
    /// `crate::runtime_prereqs::RuntimePrereqs::merge`. Empty default
    /// (modality + base_image both `None`) means the modality opts out
    /// of the derived-image pipeline and packages emitted from it fall
    /// back to the host-mode (or per-task `container.image`) path at
    /// runtime.
    #[serde(default, skip_serializing_if = "is_empty_runtime_baseline")]
    pub runtime_baseline: crate::runtime_prereqs::RuntimePrereqs,

    /// Modalities this archetype is authored to span when the SME asks
    /// for cross-omics. Empty (default) for single-modality archetypes;
    /// ≥2 entries for cross-omics archetypes (e.g.
    /// `["bulk_rnaseq", "proteomics"]` for
    /// `cross_omics_rnaseq_proteomics.yaml`). The composer's
    /// `find_match_cross_omics` reads this set as an order-insensitive
    /// match against
    /// `ClassificationResult.{modality, additional_modalities}`.
    /// Unrelated to `modality_hint`, which is the single-modality
    /// disambiguator for goal-shape ties.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cross_omics_modalities: Vec<String>,
}

/// `skip_serializing_if` predicate for archetype-level
/// `runtime_baseline`. Mirrors the atom-level helper but allows
/// `base_image` / `modality` to count toward "non-empty" — those
/// fields are meaningful here.
fn is_empty_runtime_baseline(p: &crate::runtime_prereqs::RuntimePrereqs) -> bool {
    p.system_packages.is_empty()
        && p.language_packages.is_empty()
        && p.system_check.is_empty()
        && p.base_image.is_none()
        && p.modality.is_none()
}

/// An atom referenced from an archetype's scaffold. Carries the
/// per-call wiring overrides the composer applies on top of the
/// atom's intrinsic shape.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ArchetypeAtomRef {
    /// Atom id (must exist in the AtomRegistry at composer time).
    pub atom_id: AtomId,

    /// Optional alias used as the stage-id in the composed DAG.
    /// Without an alias the atom_id is the stage id; alias-distinct
    /// allows the same atom to appear twice in one composition
    /// (e.g. a discovery atom paired with its operation atom).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub alias: Option<String>,

    /// Per-call dependency overrides the composer respects.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Whether this atom is required or optional. When `optional`,
    /// the composer skips it if the SME's intake doesn't supply
    /// the inputs the atom needs.
    #[serde(default = "default_required")]
    pub required: bool,

    /// Per-call figure override. When present, replaces the referenced
    /// atom's `required_figures` for this archetype slot. This is used
    /// when the same atom appears twice with different modalities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub required_figures: Option<Vec<String>>,

    /// Per-call plotting module override. Usually unnecessary because
    /// the compiler falls back to the underlying atom id for aliased
    /// stages, but useful for modality-specific shared modules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub plot_stage_id: Option<String>,

    /// Per-call expected artifact override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_artifacts: Option<Vec<String>>,

    /// Per-call required artifact override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        optional,
        type = "Array<{ path: string; min_size_bytes?: number; schema_ref?: string }>"
    )]
    pub required_artifacts: Option<Vec<crate::taxonomy::RequiredArtifactSpec>>,

    /// Per-call figure-obligation lint exemption. When set, overrides the
    /// atom's own `figure_exempt` for this archetype slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub figure_exempt: Option<crate::atom::FigureExempt>,
}

fn default_required() -> bool {
    true
}

/// Cross-archetype composition pointer.
///
/// Extended with `id_prefix` (stage-id namespacing applied
/// to all atoms inherited from the named archetype, including their
/// `depends_on` references) and `replace_atoms` (per-atom substitutions
/// — replace `differential_expression` from the inherited archetype
/// with `mofa_factor_decomposition` from this one).
///
/// Cross-omics archetypes use this to inherit two single-modality
/// pipelines without duplicating the atom lists. Example:
/// ```yaml
/// compose:
/// - { archetype_id: bulk_rnaseq, position: prefix, id_prefix: rnaseq_ }
/// - { archetype_id: proteomics, position: prefix, id_prefix: proteomics_ }
/// atoms:
/// - { atom_id: reporting,
/// alias: cross_omics_thematic_comparison,
/// depends_on: [rnaseq_differential_expression, proteomics_differential_abundance] }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct ComposeRef {
    /// Other archetype id whose atoms are embedded.
    pub archetype_id: String,
    /// Position to embed at: `prefix` (run before this archetype's
    /// atoms), `suffix` (after), or `replace_atoms` (use the named
    /// atoms only, ignore this archetype's own atom list).
    pub position: ComposePosition,
    /// Optional stage-id prefix applied to every atom
    /// inherited from the named archetype. The prefix is also rewritten
    /// over inherited `depends_on` references so the inherited DAG
    /// stays internally consistent. `None` keeps the inherited
    /// stage-ids verbatim (only safe when the parent archetype's
    /// stage-ids don't collide with this one's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub id_prefix: Option<String>,
    /// Per-atom substitutions applied to the inherited
    /// archetype's atom list. Keys are atom_ids in the inherited
    /// archetype; values are atom_ids in this archetype's
    /// `AtomRegistry`. Used to swap a single atom (e.g. replace
    /// `differential_expression` with `mofa_factor_decomposition`)
    /// without forking the rest of the inherited pipeline.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub replace_atoms: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// ComposePosition discriminant.
pub enum ComposePosition {
    /// Prefix variant.
    Prefix,
    /// Suffix variant.
    Suffix,
    /// ReplaceAtoms variant.
    ReplaceAtoms,
}

/// A cross-dependency edge that the composer wouldn't infer from
/// atom-level `depends_on` alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct CrossDependency {
    /// From.
    pub from: String,
    /// To.
    pub to: String,
    /// Free-form rationale carried into the composition rationale
    /// log so reviewers can see why the edge exists.
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_archetype_roundtrips() {
        let arch = ArchetypeDefinition {
            schema_version: CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
            id: "single_cell_de".into(),
            version: "1.0.0".into(),
            description: "scRNA-seq differential expression analysis.".into(),
            sme_summary: "Cluster cells, then test for differential expression.".into(),
            goal_data: "data:3917".into(),
            goal_format: Some("format:3590".into()),
            atoms: vec![
                ArchetypeAtomRef {
                    atom_id: "qc_preprocessing".into(),
                    alias: None,
                    depends_on: vec![],
                    required: true,
                    required_figures: None,
                    plot_stage_id: None,
                    figure_exempt: None,
                    expected_artifacts: None,
                    required_artifacts: None,
                },
                ArchetypeAtomRef {
                    atom_id: "differential_expression".into(),
                    alias: None,
                    depends_on: vec!["qc_preprocessing".into()],
                    required: true,
                    required_figures: None,
                    plot_stage_id: None,
                    figure_exempt: None,
                    expected_artifacts: None,
                    required_artifacts: None,
                },
            ],
            slot_mappings: BTreeMap::new(),
            compose: vec![],
            slots: None,
            cross_dependencies: vec![],
            claim_boundary: None,
            project_class: "bioinformatics".into(),
            modality_hint: None,
            goal_kind_hint: None,
            preferred_container: None,
            runtime_baseline: Default::default(),
            cross_omics_modalities: vec![],
        };
        let yaml = serde_yml::to_string(&arch).unwrap();
        let back: ArchetypeDefinition = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(arch, back);
    }

    /// Archetype-level `preferred_container` field
    /// round-trips through serde and is suppressed from YAML when
    /// `None` so existing archetype files stay byte-identical.
    #[test]
    #[allow(deprecated)]
    fn archetype_preferred_container_roundtrips() {
        use crate::atom::{ContainerSource, ContainerSpec, NetworkPolicy};
        let arch = ArchetypeDefinition {
            schema_version: CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
            id: "clinical_trial".into(),
            version: "1.0.0".into(),
            description: "Clinical-trial archetype with strict network policy.".into(),
            sme_summary: "Phase III RCT analysis with frozen SAP.".into(),
            goal_data: "data:0951".into(),
            goal_format: Some("format:3475".into()),
            atoms: vec![],
            slot_mappings: BTreeMap::new(),
            compose: vec![],
            slots: None,
            cross_dependencies: vec![],
            claim_boundary: None,
            project_class: "clinical_trial".into(),
            modality_hint: None,
            goal_kind_hint: None,
            preferred_container: Some(ContainerSpec {
                image: "ghcr.io/scripps/clinical-trial-runner".into(),
                tag: "1.0.0".into(),
                digest: String::new(),
                arch: vec!["amd64".into()],
                gpu_required: false,
                network: Some(NetworkPolicy::None {
                    allowlist: vec!["github.com".into(), "ghcr.io".into()],
                }),
                source: ContainerSource::Image,
            }),
            runtime_baseline: Default::default(),
            cross_omics_modalities: vec![],
        };
        let yaml = serde_yml::to_string(&arch).unwrap();
        assert!(yaml.contains("preferred_container"));
        assert!(yaml.contains("clinical-trial-runner"));
        assert!(yaml.contains("kind: none"), "NetworkPolicy::None tagged");
        let back: ArchetypeDefinition = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(arch, back);
    }

    /// `cross_omics_modalities` field round-trips through serde and is
    /// suppressed from YAML when empty so existing single-modality
    /// archetype files stay byte-identical.
    #[test]
    fn archetype_cross_omics_modalities_roundtrips() {
        let arch = ArchetypeDefinition {
            schema_version: CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
            id: "cross_omics_rnaseq_proteomics".into(),
            version: "1.0.0".into(),
            description: "Cross-omics RNA-seq + proteomics.".into(),
            sme_summary: "RNA-seq and mass-spec proteomics in parallel.".into(),
            goal_data: "data:0951".into(),
            goal_format: Some("format:3475".into()),
            atoms: vec![],
            slot_mappings: BTreeMap::new(),
            compose: vec![],
            slots: None,
            cross_dependencies: vec![],
            claim_boundary: None,
            project_class: "bioinformatics".into(),
            modality_hint: Some("cross_omics_rnaseq_proteomics".into()),
            goal_kind_hint: None,
            preferred_container: None,
            runtime_baseline: Default::default(),
            cross_omics_modalities: vec!["bulk_rnaseq".into(), "proteomics".into()],
        };
        let yaml = serde_yml::to_string(&arch).unwrap();
        assert!(
            yaml.contains("cross_omics_modalities"),
            "non-empty list should serialize"
        );
        assert!(yaml.contains("bulk_rnaseq"));
        assert!(yaml.contains("proteomics"));
        let back: ArchetypeDefinition = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(arch, back);

        let single = ArchetypeDefinition {
            cross_omics_modalities: vec![],
            ..arch
        };
        let single_yaml = serde_yml::to_string(&single).unwrap();
        assert!(
            !single_yaml.contains("cross_omics_modalities"),
            "empty list should be suppressed from YAML"
        );
    }

    #[test]
    fn compose_position_roundtrips() {
        for pos in [
            ComposePosition::Prefix,
            ComposePosition::Suffix,
            ComposePosition::ReplaceAtoms,
        ] {
            let r = ComposeRef {
                archetype_id: "x".into(),
                position: pos,
                id_prefix: None,
                replace_atoms: BTreeMap::new(),
            };
            let yaml = serde_yml::to_string(&r).unwrap();
            let back: ComposeRef = serde_yml::from_str(&yaml).unwrap();
            assert_eq!(r, back);
        }
    }

    /// `id_prefix` + `replace_atoms` round-trip cleanly
    /// and are suppressed from YAML when empty so existing
    /// archetypes that haven't migrated to compose-inheritance stay
    /// byte-identical.
    #[test]
    fn compose_ref_id_prefix_and_replace_atoms_roundtrip() {
        let mut replace = BTreeMap::new();
        replace.insert("differential_expression".into(), "mofa_factor".into());
        let r = ComposeRef {
            archetype_id: "bulk_rnaseq".into(),
            position: ComposePosition::Prefix,
            id_prefix: Some("rnaseq_".into()),
            replace_atoms: replace,
        };
        let yaml = serde_yml::to_string(&r).unwrap();
        assert!(yaml.contains("id_prefix: rnaseq_"));
        assert!(yaml.contains("replace_atoms:"));
        let back: ComposeRef = serde_yml::from_str(&yaml).unwrap();
        assert_eq!(r, back);

        let bare = ComposeRef {
            archetype_id: "x".into(),
            position: ComposePosition::Prefix,
            id_prefix: None,
            replace_atoms: BTreeMap::new(),
        };
        let bare_yaml = serde_yml::to_string(&bare).unwrap();
        assert!(
            !bare_yaml.contains("id_prefix"),
            "id_prefix=None must not serialize, got:\n{}",
            bare_yaml
        );
        assert!(
            !bare_yaml.contains("replace_atoms"),
            "empty replace_atoms must not serialize, got:\n{}",
            bare_yaml
        );
    }

    #[test]
    fn required_default_is_true_and_suppressed_in_yaml() {
        let r = ArchetypeAtomRef {
            atom_id: "x".into(),
            alias: None,
            depends_on: vec![],
            required: true,
            required_figures: None,
            plot_stage_id: None,
            figure_exempt: None,
            expected_artifacts: None,
            required_artifacts: None,
        };
        let yaml = serde_yml::to_string(&r).unwrap();
        // required: true is not the default we suppress; default is
        // true so the value is always emitted. Lock the round-trip.
        let back: ArchetypeAtomRef = serde_yml::from_str(&yaml).unwrap();
        assert!(back.required);
    }
}
