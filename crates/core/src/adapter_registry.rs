//! Adapter registry and safety policy.
//!
//! Adapters are typed `TaskNode`s with an `AdapterClass` and
//! `AdapterSafety` per design §14:
//!
//! - `Lossless` adapters may be inserted automatically by the
//!   compatibility engine.
//! - `LossyDeclared` adapters require an assumption-ledger entry
//!   and a UI warning.
//! - `ScientificallyRisky` adapters require explicit SME
//!   confirmation before the composition is treated as
//!   executable.
//! - `PolicyRestricted` adapters require policy-engine approval
//!   and may be refused for clinical/regulated workflows.
//!
//! The registry ships pre-loaded with a small starter set
//! (gzip, BAM index/sort, manifest, sample sheet, schema
//! conversion) plus the explicitly-risky list (genome liftover,
//! gene ID mapping, normalization, batch correction, imputation,
//! cell type label transfer, variant normalization). Site-local
//! adapters can extend the registry via `AdapterRegistry::register`
//! at startup; the registry is keyed by `(class, from_format,
//! to_format)` so lookup is O(1) per edge.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

use crate::workflow_contracts::evidence::ValidatorRef;
use crate::workflow_contracts::implementation::Implementation;
use crate::workflow_contracts::lifecycle::{LifecycleState, NodeStatus, TrustLevel};
use crate::workflow_contracts::semantic_type::SemanticType;
use crate::workflow_contracts::task_node::{Provenance, SemVer, TaskNode};

/// What kind of work an adapter does. Mirrors design §14.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AdapterClass {
    /// FormatConversion variant.
    FormatConversion,
    /// Compression variant.
    Compression,
    /// IndexGeneration variant.
    IndexGeneration,
    /// Sorting variant.
    Sorting,
    /// Filtering variant.
    Filtering,
    /// Normalization variant.
    Normalization,
    /// IdentifierMapping variant.
    IdentifierMapping,
    /// CoordinateLiftover variant.
    CoordinateLiftover,
    /// MetadataJoin variant.
    MetadataJoin,
    /// SampleSheetGeneration variant.
    SampleSheetGeneration,
    /// ContainerWrapping variant.
    ContainerWrapping,
    /// SchemaConversion variant.
    SchemaConversion,
    /// ReferenceDownload variant.
    ReferenceDownload,
    /// QcGate variant.
    QcGate,
}

/// Safety classification of an adapter. Drives whether the
/// composer can auto-insert it.
#[derive(
    Debug,
    Clone,
    Copy,
    Serialize,
    Deserialize,
    PartialEq,
    Eq,
    Ord,
    PartialOrd,
    TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
pub enum AdapterSafety {
    /// Can be auto-inserted; no assumption ledger entry required.
    Lossless,
    /// Auto-insertable with an assumption-ledger entry.
    LossyDeclared,
    /// Requires explicit SME confirmation before insertion.
    ScientificallyRisky,
    /// Requires policy approval. Refused on
    /// clinical/regulated workflows.
    PolicyRestricted,
}

/// A typed adapter spec. The composer treats adapters as
/// first-class `TaskNode`s when inserting them; the registry's
/// role is to look up the right adapter for an edge and return
/// the materialized `TaskNode` skeleton.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct AdapterSpec {
    /// Stable id (`gzip_decompress`, `bam_index`, `liftover_grch37_grch38`).
    pub id: String,
    /// Human label.
    pub name: String,
    /// Class.
    pub class: AdapterClass,
    /// Safety.
    pub safety: AdapterSafety,
    /// Optional source format IRI (e.g. `format:1929` for FASTA;
    /// `None` matches any format).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_format: Option<String>,
    /// Optional destination format IRI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub to_format: Option<String>,
    /// Optional source semantic type stable id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub from_semantic_type: Option<String>,
    /// Optional destination semantic type stable id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub to_semantic_type: Option<String>,
    /// Free-text rationale. Surfaces in `CompatibilityProof.rationale`
    /// when this adapter is inserted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rationale: Option<String>,
    /// V3 alignment validators auto-attached to every
    /// consumer port connected via this adapter. For Lossless adapters,
    /// these are invariant checks (identity preservation, schema
    /// preservation). For Lossy / Risky adapters, these are
    /// postcondition checks.
    ///
    /// The F10 invariant: every Lossless adapter MUST carry at least
    /// one validator so the contracted output shape is checked rather
    /// than trusted. The starter set in `starter_adapters()` populates
    /// at least one validator per Lossless entry; the property test
    /// `f10_adapter_validator_presence.rs` locks the invariant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub validators: Vec<ValidatorRef>,
}

impl AdapterSpec {
    /// Convert the spec into a fully-formed `TaskNode` ready to
    /// insert into a `WorkflowDag`.
    pub fn to_task_node(&self) -> TaskNode {
        let mut attrs: BTreeMap<String, serde_json::Value> = BTreeMap::new();
        attrs.insert(
            "adapter_class".into(),
            serde_json::to_value(self.class).unwrap_or(serde_json::Value::Null),
        );
        attrs.insert(
            "adapter_safety".into(),
            serde_json::to_value(self.safety).unwrap_or(serde_json::Value::Null),
        );
        if let Some(rationale) = &self.rationale {
            attrs.insert(
                "adapter_rationale".into(),
                serde_json::Value::String(rationale.clone()),
            );
        }
        TaskNode {
            id: self.id.clone(),
            human_name: self.name.clone(),
            machine_name: self.id.clone(),
            status: NodeStatus::Active,
            intent: format!("Adapter: {}", self.name),
            inputs: Vec::new(),
            outputs: Vec::new(),
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            assumptions: Vec::new(),
            implementation: Implementation::Unimplemented,
            validators: Vec::new(),
            evidence: Default::default(),
            risk: match self.safety {
                AdapterSafety::Lossless => {
                    crate::workflow_contracts::evidence::RiskClass::Negligible
                }
                AdapterSafety::LossyDeclared => crate::workflow_contracts::evidence::RiskClass::Low,
                AdapterSafety::ScientificallyRisky => {
                    crate::workflow_contracts::evidence::RiskClass::Moderate
                }
                AdapterSafety::PolicyRestricted => {
                    crate::workflow_contracts::evidence::RiskClass::High
                }
            },
            provenance: Provenance {
                source: Some("adapter_registry::starter".into()),
                ..Provenance::default()
            },
            version: SemVer::default(),
            lifecycle_state: LifecycleState::Production,
            trust_level: TrustLevel::Reviewed,
            deprecation: None,
            attributes: attrs,
        }
    }

    /// Create a copy with semantic-type fields populated. Used
    /// by typed lookup.
    pub fn from_to_semantic_type(
        mut self,
        from: Option<&SemanticType>,
        to: Option<&SemanticType>,
    ) -> Self {
        self.from_semantic_type = from.map(|s| s.stable_id());
        self.to_semantic_type = to.map(|s| s.stable_id());
        self
    }
}

/// Hard-fail error from loading adapter YAML. Adapters are safety-
/// critical — a malformed file aborts registry construction rather
/// than silently dropping the spec.
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum AdapterLoadError {
    #[error("read adapter dir {}: {message}", path.display())]
    /// ReadDir variant.
    ReadDir {
        /// Path.
        path: std::path::PathBuf,
        /// Message.
        message: String,
    },
    #[error("read adapter file {}: {message}", path.display())]
    /// ReadFile variant.
    ReadFile {
        /// Path.
        path: std::path::PathBuf,
        /// Message.
        message: String,
    },
    #[error("parse adapter file {}: {message}", path.display())]
    /// Parse variant.
    Parse {
        /// Path.
        path: std::path::PathBuf,
        /// Message.
        message: String,
    },
}

/// In-memory adapter catalog. Pre-loaded with the design's
/// starter set; site-local installations can extend at startup.
#[derive(Debug, Clone, Default)]
pub struct AdapterRegistry {
    by_id: BTreeMap<String, AdapterSpec>,
}

impl AdapterRegistry {
    /// Construct with the starter adapters pre-registered.
    pub fn with_starters() -> Self {
        let mut reg = Self::default();
        for adapter in starter_adapters() {
            reg.register(adapter);
        }
        for adapter in risky_adapters() {
            reg.register(adapter);
        }
        reg
    }

    /// Construct with starter adapters, then merge any YAML files
    /// from `dir` (typically `config/adapters/`). Files are sorted
    /// by filename for byte-stable load order; later entries with
    /// the same `id` override earlier ones (site-local overrides
    /// built-in starters).
    ///
    /// Files starting with `_` (like `_adapter.schema.json`) are
    /// skipped. Unknown files that fail to parse return an error
    /// rather than being silently dropped — adapters are
    /// safety-critical, so a malformed file is treated as a hard
    /// failure rather than a no-op.
    pub fn with_starters_and_dir(
        dir: impl AsRef<std::path::Path>,
    ) -> Result<Self, AdapterLoadError> {
        let mut reg = Self::with_starters();
        reg.merge_yaml_dir(dir)?;
        Ok(reg)
    }

    /// Merge YAML adapters from `dir` into an existing registry.
    /// See `with_starters_and_dir` for semantics.
    pub fn merge_yaml_dir(
        &mut self,
        dir: impl AsRef<std::path::Path>,
    ) -> Result<(), AdapterLoadError> {
        let dir = dir.as_ref();
        if !dir.exists() {
            return Ok(());
        }
        let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| AdapterLoadError::ReadDir {
                path: dir.to_path_buf(),
                message: e.to_string(),
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| e == "yaml" || e == "yml")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| !n.starts_with('_'))
            })
            .collect();
        entries.sort();
        for path in entries {
            let bytes = std::fs::read_to_string(&path).map_err(|e| AdapterLoadError::ReadFile {
                path: path.clone(),
                message: e.to_string(),
            })?;
            let spec: AdapterSpec =
                serde_yml::from_str(&bytes).map_err(|e| AdapterLoadError::Parse {
                    path: path.clone(),
                    message: e.to_string(),
                })?;
            self.register(spec);
        }
        Ok(())
    }

    /// Register.
    pub fn register(&mut self, adapter: AdapterSpec) {
        self.by_id.insert(adapter.id.clone(), adapter);
    }

    /// Get.
    pub fn get(&self, id: &str) -> Option<&AdapterSpec> {
        self.by_id.get(id)
    }

    // Phantom-typed liftover-decision helper.
    //
    // Wraps the registry lookup with a compile-time-typed `From` /
    // `To` reference-genome pair so call sites that adopt the
    // phantom-typed `AlignedReads<R>` discipline cannot
    // accidentally ask for `liftover_grch38_grch38` — the type system
    // refuses `F ≡ T` via `liftover_required` returning `false`, and
    // the helper short-circuits to `None` before the registry sees
    // the bogus id. The wrapper type carries the compile-time
    // guarantee that `pub(crate)` discipline is preserved (the
    // phantom types never escape the crate boundary; F23 forbids
    // serializing them).
    //
    // `#[allow(dead_code)]`: the typed surface is intentional today
    // — adopters who pick up `AlignedReads<R>` incrementally call
    // this helper. The legacy untyped path
    // (`try_resolve_facet_with_adapter`) carries the load-bearing
    // call site for now.
    #[allow(dead_code)]
    pub(crate) fn liftover_decision<F, T>(&self) -> Option<&AdapterSpec>
    where
        F: crate::compile_time_discipline::reference_genome::ReferenceGenome,
        T: crate::compile_time_discipline::reference_genome::ReferenceGenome,
    {
        use crate::compile_time_discipline::reference_genome::{liftover_required, AlignedReads};
        use std::marker::PhantomData;

        let from_reads: AlignedReads<F> = AlignedReads::new(true, true);
        if !liftover_required(&from_reads, PhantomData::<T>) {
            // Phantom-typed dispatch refused — F ≡ T, no liftover needed.
            return None;
        }
        let id = format!(
            "liftover_{}_{}",
            F::NAME.to_lowercase(),
            T::NAME.to_lowercase(),
        );
        self.by_id.get(&id)
    }

    /// Iter.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &AdapterSpec)> {
        self.by_id.iter()
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Find adapters that convert from `from` semantic type to
    /// `to`. Returns adapters in stable id order. Current
    /// implementation is exact-match lookup; a future planner
    /// extension will add chained adapter search.
    pub fn find_for_semantic_types(
        &self,
        from: Option<&str>,
        to: Option<&str>,
    ) -> Vec<&AdapterSpec> {
        self.by_id
            .values()
            .filter(|a| {
                let from_matches = match (from, a.from_semantic_type.as_deref()) {
                    (Some(want), Some(have)) => want == have,
                    (None, _) => true,
                    (Some(_), None) => true, // wildcard adapter
                };
                let to_matches = match (to, a.to_semantic_type.as_deref()) {
                    (Some(want), Some(have)) => want == have,
                    (None, _) => true,
                    (Some(_), None) => true,
                };
                from_matches && to_matches
            })
            .collect()
    }

    /// Find adapters that convert format → format. Used for the
    /// "BAM unsorted → sorted+indexed" path.
    pub fn find_for_formats(&self, from: &str, to: &str) -> Vec<&AdapterSpec> {
        self.by_id
            .values()
            .filter(|a| {
                a.from_format.as_deref() == Some(from) && a.to_format.as_deref() == Some(to)
            })
            .collect()
    }
}

/// Returns `true` when `atom_id` matches the id of any registered
/// adapter (starter or risky). Used by the figure-obligation lint
/// (`crates/core/src/plot_affordance/obligation.rs`) to skip adapters
/// rather than flagging them as `DataProduct`-obligation violations.
///
/// The check builds a `BTreeSet` from `starter_adapters()` +
/// `risky_adapters()` on each call. That is deliberately not cached
/// at the module level — adapters are a small fixed list (~16 entries)
/// and the lint is called at most once per run, so the allocation cost
/// is negligible versus the complexity of a `LazyLock` here.
pub fn is_adapter_atom(atom_id: &str) -> bool {
    starter_adapters()
        .iter()
        .chain(risky_adapters().iter())
        .any(|a| a.id == atom_id)
}

/// V3 alignment convenience constructor for a
/// `ValidatorRef` with only the id populated. Used inline in
/// `starter_adapters()` so the Lossless starters carry their
/// invariant checks.
fn validator(id: &str) -> ValidatorRef {
    ValidatorRef {
        id: id.into(),
        version: None,
        parameters: None,
    }
}

/// Starter (Lossless) adapters listed in the design / plan.
///
/// V3 alignment every Lossless starter carries at
/// least one validator (`schema_preservation` /
/// `bam_index_consistency` / `manifest_hash_integrity` / etc.) so the
/// F10 invariant ("every Lossless adapter inserted brings a
/// downstream validator into the emitted validator set") holds at
/// adapter-registration time.
fn starter_adapters() -> Vec<AdapterSpec> {
    vec![
        AdapterSpec {
            id: "gzip_decompress".into(),
            name: "Gzip decompress".into(),
            class: AdapterClass::Compression,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Decompress gzip wrapper; underlying bytes unchanged".into()),
            validators: vec![
                validator("schema_preservation"),
                validator("byte_invariant_post_decompress"),
            ],
        },
        AdapterSpec {
            id: "gzip_compress".into(),
            name: "Gzip compress".into(),
            class: AdapterClass::Compression,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Add gzip wrapper; underlying bytes unchanged".into()),
            validators: vec![
                validator("schema_preservation"),
                validator("byte_invariant_post_compress"),
            ],
        },
        AdapterSpec {
            id: "bam_sort_coordinate".into(),
            name: "Sort BAM by coordinate".into(),
            class: AdapterClass::Sorting,
            safety: AdapterSafety::Lossless,
            from_format: Some("format:2572".into()),
            to_format: Some("format:2572".into()),
            from_semantic_type: Some("data:0863".into()),
            to_semantic_type: Some("data:0863".into()),
            rationale: Some("samtools sort by coordinate; record content unchanged".into()),
            validators: vec![
                validator("bam_record_count_preserved"),
                validator("bam_sort_order_coordinate"),
            ],
        },
        AdapterSpec {
            id: "bam_index".into(),
            name: "Index BAM (BAI)".into(),
            class: AdapterClass::IndexGeneration,
            safety: AdapterSafety::Lossless,
            from_format: Some("format:2572".into()),
            to_format: Some("format:2572".into()),
            from_semantic_type: Some("data:0863".into()),
            to_semantic_type: Some("data:0863".into()),
            rationale: Some("Generate BAI; original BAM unchanged".into()),
            validators: vec![validator("bam_index_consistency")],
        },
        AdapterSpec {
            id: "manifest_generate".into(),
            name: "Generate file manifest".into(),
            class: AdapterClass::MetadataJoin,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Compute SHA-256 manifest of input files".into()),
            validators: vec![validator("manifest_hash_integrity")],
        },
        AdapterSpec {
            id: "sample_sheet_generate".into(),
            name: "Generate sample sheet".into(),
            class: AdapterClass::SampleSheetGeneration,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Build sample-sheet TSV from per-sample metadata".into()),
            validators: vec![validator("sample_sheet_completeness")],
        },
        AdapterSpec {
            id: "schema_convert".into(),
            name: "Schema conversion".into(),
            class: AdapterClass::SchemaConversion,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Re-shape JSON/TSV to match consumer schema".into()),
            validators: vec![validator("schema_preservation")],
        },
    ]
}

/// Explicitly-risky adapters: scientifically meaningful
/// transformations that must not be auto-inserted.
fn risky_adapters() -> Vec<AdapterSpec> {
    vec![
        AdapterSpec {
            id: "liftover_grch37_grch38".into(),
            name: "Genome liftover GRCh37 → GRCh38".into(),
            class: AdapterClass::CoordinateLiftover,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "UCSC liftover changes coordinates; ~99% of regions map cleanly, ~1% \
                 require manual review or are dropped"
                    .into(),
            ),
            validators: vec![
                validator("liftover_mapped_fraction_above_threshold"),
                validator("liftover_unmapped_audit"),
            ],
        },
        AdapterSpec {
            id: "liftover_grch38_grch37".into(),
            name: "Genome liftover GRCh38 → GRCh37".into(),
            class: AdapterClass::CoordinateLiftover,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some("Reverse liftover; same caveats as forward direction".into()),
            validators: vec![
                validator("liftover_mapped_fraction_above_threshold"),
                validator("liftover_unmapped_audit"),
            ],
        },
        AdapterSpec {
            id: "gene_symbol_to_ensembl".into(),
            name: "Gene symbol → Ensembl ID mapping".into(),
            class: AdapterClass::IdentifierMapping,
            safety: AdapterSafety::LossyDeclared,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "Symbol → Ensembl is many-to-one and not bijective; ambiguous \
                 symbols become missing values"
                    .into(),
            ),
            validators: vec![validator("gene_id_mapping_completeness")],
        },
        AdapterSpec {
            id: "ensembl_to_gene_symbol".into(),
            name: "Ensembl ID → gene symbol mapping".into(),
            class: AdapterClass::IdentifierMapping,
            safety: AdapterSafety::LossyDeclared,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "Ensembl → symbol drops version info and may collide on aliases".into(),
            ),
            validators: vec![validator("gene_id_mapping_completeness")],
        },
        AdapterSpec {
            id: "normalize_counts".into(),
            name: "Normalize count matrix".into(),
            class: AdapterClass::Normalization,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: Some("data:3917".into()),
            to_semantic_type: Some("data:3917".into()),
            rationale: Some(
                "Choice of normalization (TPM, vsn, quantile) materially affects \
                 downstream DE; method must be SME-confirmed"
                    .into(),
            ),
            validators: vec![validator("normalization_target_distribution")],
        },
        AdapterSpec {
            id: "batch_correct".into(),
            name: "Batch effect correction".into(),
            class: AdapterClass::Normalization,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "Batch correction can remove biological signal if method is mismatched \
                 to study design"
                    .into(),
            ),
            validators: vec![validator("batch_correction_residual_variance")],
        },
        AdapterSpec {
            id: "impute_missing".into(),
            name: "Impute missing values".into(),
            class: AdapterClass::Normalization,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "Imputation introduces synthetic values; downstream uncertainty \
                 inflated"
                    .into(),
            ),
            validators: vec![validator("imputation_uncertainty_recorded")],
        },
        AdapterSpec {
            id: "celltype_label_transfer".into(),
            name: "Cell type label transfer from reference".into(),
            class: AdapterClass::IdentifierMapping,
            safety: AdapterSafety::ScientificallyRisky,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: Some(
                "Reference annotations may not generalize to query samples; \
                 manual confirmation per cluster recommended"
                    .into(),
            ),
            validators: vec![validator("celltype_label_confidence")],
        },
        AdapterSpec {
            id: "variant_normalize_across_refs".into(),
            name: "Variant normalization across reference genomes".into(),
            class: AdapterClass::CoordinateLiftover,
            safety: AdapterSafety::PolicyRestricted,
            from_format: Some("format:3016".into()),
            to_format: Some("format:3016".into()),
            from_semantic_type: Some("data:3498".into()),
            to_semantic_type: Some("data:3498".into()),
            rationale: Some(
                "Cross-reference variant normalization is regulated for \
                 clinical interpretation; refused outside approved pipelines"
                    .into(),
            ),
            validators: vec![validator("variant_normalization_clinical_review")],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_registry_loads() {
        let reg = AdapterRegistry::with_starters();
        assert!(reg.len() >= 16);
        // Confirm the starter set is byte-stable.
        let ids: Vec<String> = reg.iter().map(|(k, _)| k.clone()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "registry iteration not in sorted order");
    }

    #[test]
    fn find_lossless_starter_by_id() {
        let reg = AdapterRegistry::with_starters();
        let a = reg.get("bam_sort_coordinate").unwrap();
        assert_eq!(a.class, AdapterClass::Sorting);
        assert_eq!(a.safety, AdapterSafety::Lossless);
    }

    #[test]
    fn liftover_is_scientifically_risky() {
        let reg = AdapterRegistry::with_starters();
        let a = reg.get("liftover_grch37_grch38").unwrap();
        assert_eq!(a.safety, AdapterSafety::ScientificallyRisky);
    }

    #[test]
    fn variant_normalize_is_policy_restricted() {
        let reg = AdapterRegistry::with_starters();
        let a = reg.get("variant_normalize_across_refs").unwrap();
        assert_eq!(a.safety, AdapterSafety::PolicyRestricted);
    }

    #[test]
    fn find_by_format_returns_bam_adapters() {
        let reg = AdapterRegistry::with_starters();
        let bam_adapters = reg.find_for_formats("format:2572", "format:2572");
        assert!(!bam_adapters.is_empty());
        // Both bam_sort_coordinate and bam_index match.
        let ids: Vec<&str> = bam_adapters.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"bam_sort_coordinate"));
        assert!(ids.contains(&"bam_index"));
    }

    #[test]
    fn adapter_to_task_node_carries_class_and_safety_in_attributes() {
        let reg = AdapterRegistry::with_starters();
        let spec = reg.get("bam_sort_coordinate").unwrap();
        let node = spec.to_task_node();
        assert_eq!(
            node.attributes
                .get("adapter_class")
                .and_then(|v| v.as_str()),
            Some("sorting")
        );
        assert_eq!(
            node.attributes
                .get("adapter_safety")
                .and_then(|v| v.as_str()),
            Some("lossless")
        );
    }

    #[test]
    fn site_local_adapter_can_be_registered() {
        let mut reg = AdapterRegistry::with_starters();
        let n = reg.len();
        reg.register(AdapterSpec {
            id: "site_local_adapter".into(),
            name: "Site-local custom".into(),
            class: AdapterClass::Filtering,
            safety: AdapterSafety::Lossless,
            from_format: None,
            to_format: None,
            from_semantic_type: None,
            to_semantic_type: None,
            rationale: None,
            validators: vec![validator("site_local_check")],
        });
        assert_eq!(reg.len(), n + 1);
        assert!(reg.get("site_local_adapter").is_some());
    }

    #[test]
    fn risky_adapter_node_has_higher_risk() {
        let reg = AdapterRegistry::with_starters();
        let lossless = reg.get("bam_index").unwrap().to_task_node();
        let risky = reg.get("liftover_grch37_grch38").unwrap().to_task_node();
        let policy = reg
            .get("variant_normalize_across_refs")
            .unwrap()
            .to_task_node();
        // risk ordering: Negligible < Low < Moderate < High < Clinical
        assert!(matches!(
            lossless.risk,
            crate::workflow_contracts::evidence::RiskClass::Negligible
        ));
        assert!(matches!(
            risky.risk,
            crate::workflow_contracts::evidence::RiskClass::Moderate
        ));
        assert!(matches!(
            policy.risk,
            crate::workflow_contracts::evidence::RiskClass::High
        ));
    }
}
