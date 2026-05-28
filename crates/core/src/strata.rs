//! Modality-stratum registry.
//!
//! Maps each modality id to one of eight analysis-family strata
//! (`transcriptomics`, `single_cell`, `epigenomics`, `genomics`,
//! `spatial`, `proteomics`, `metagenomics`, `clinical`). The
//! stratum is injected into `WORKFLOW.json::meta.modality_stratum`
//! at emit time so the scorer, cross-version diff, and eval harness
//! can partition results by family without re-reading the full
//! modality manifest.
//!
//! The authoritative source of truth is `config/strata.yaml`, which
//! is embedded at compile time via `include_str!`. Callers that need
//! to load an overriding file from disk can call
//! [`StrataRegistry::from_yaml`] with the bytes they read.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const EMBEDDED_STRATA_YAML: &str = include_str!("../../../config/strata.yaml");

/// One stratum entry loaded from `config/strata.yaml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct StratumDefinition {
    /// Human-readable name surfaced in UI and scorer reports.
    pub display_name: String,
    /// One-line description of the modality family.
    pub description: String,
    /// Modality ids that belong to this stratum.
    pub modalities: Vec<String>,
}

/// Wire shape of `config/strata.yaml` — the top-level `strata:` map.
#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct StrataFile {
    strata: BTreeMap<String, StratumDefinition>,
}

/// Indexed registry: `modality_id → stratum_id`.
///
/// Built once at startup from the embedded YAML and optionally
/// overridden per test or operator via [`StrataRegistry::from_yaml`].
#[derive(Debug, Clone, Default)]
pub struct StrataRegistry {
    /// Inverted index: `modality_id → stratum_id`.
    index: BTreeMap<String, String>,
    /// All stratum definitions keyed by stratum id.
    pub definitions: BTreeMap<String, StratumDefinition>,
}

impl StrataRegistry {
    /// Build from the YAML embedded at compile time.
    /// Panics in debug builds if the embedded YAML is malformed (should
    /// never happen in CI since the file is checked in). In release builds
    /// a malformed embed returns an empty registry rather than crashing.
    pub fn from_embedded() -> Self {
        Self::from_yaml(EMBEDDED_STRATA_YAML.as_bytes()).unwrap_or_else(|e| {
            debug_assert!(false, "embedded strata.yaml is malformed: {e}");
            Self::default()
        })
    }

    /// Build from caller-supplied YAML bytes. Used in tests and for
    /// operator-supplied overrides via `SWFC_STRATA_YAML`.
    pub fn from_yaml(bytes: &[u8]) -> Result<Self> {
        let text = std::str::from_utf8(bytes).context("strata.yaml is not valid UTF-8")?;
        let file: StrataFile = serde_yml::from_str(text).context("parsing strata.yaml")?;
        let mut index = BTreeMap::new();
        for (stratum_id, def) in &file.strata {
            for modality_id in &def.modalities {
                index.insert(modality_id.clone(), stratum_id.clone());
            }
        }
        Ok(Self {
            index,
            definitions: file.strata,
        })
    }

    /// Return the stratum id for `modality_id`, or `None` when the
    /// modality is not in the registry (e.g. an ad-hoc modality added
    /// after the strata file was last updated).
    pub fn stratum_for(&self, modality_id: &str) -> Option<&str> {
        self.index.get(modality_id).map(String::as_str)
    }

    /// Return the `StratumDefinition` for `stratum_id`, or `None`.
    pub fn definition(&self, stratum_id: &str) -> Option<&StratumDefinition> {
        self.definitions.get(stratum_id)
    }

    /// All stratum ids in sorted order.
    pub fn stratum_ids(&self) -> impl Iterator<Item = &str> {
        self.definitions.keys().map(String::as_str)
    }
}

/// One-shot convenience: return the stratum id for `modality_id` using the
/// embedded registry. Calls [`StrataRegistry::from_embedded`] on each
/// invocation — callers that need repeated lookups should build and hold a
/// [`StrataRegistry`].
pub fn modality_stratum(modality_id: &str) -> Option<String> {
    StrataRegistry::from_embedded()
        .stratum_for(modality_id)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_yaml_parses() {
        let reg = StrataRegistry::from_embedded();
        // Eight strata must be present.
        assert_eq!(
            reg.definitions.len(),
            8,
            "expected 8 strata, got {:?}",
            reg.definitions.keys().collect::<Vec<_>>()
        );
    }

    #[test]
    fn all_21_modalities_covered() {
        let reg = StrataRegistry::from_embedded();
        let expected = [
            "bulk_rnaseq",
            "long_read_rnaseq",
            "ribo_seq",
            "single_cell_rnaseq",
            "single_cell_vdj",
            "crispr_screen_scrnaseq",
            "chip_seq",
            "chip_exo",
            "cut_tag",
            "hi_chip",
            "atac_seq",
            "starr_seq",
            "methylation",
            "variant_calling",
            "gwas",
            "spatial_transcriptomics",
            "proteomics",
            "immunopeptidomics",
            "metagenomics",
            "ehr_clinical_prediction",
            "generic_omics",
        ];
        for id in expected {
            assert!(
                reg.stratum_for(id).is_some(),
                "modality '{}' has no stratum",
                id
            );
        }
    }

    #[test]
    fn known_mappings_correct() {
        let reg = StrataRegistry::from_embedded();
        assert_eq!(reg.stratum_for("bulk_rnaseq"), Some("transcriptomics"));
        assert_eq!(reg.stratum_for("single_cell_rnaseq"), Some("single_cell"));
        assert_eq!(reg.stratum_for("atac_seq"), Some("epigenomics"));
        assert_eq!(reg.stratum_for("variant_calling"), Some("genomics"));
        assert_eq!(reg.stratum_for("spatial_transcriptomics"), Some("spatial"));
        assert_eq!(reg.stratum_for("proteomics"), Some("proteomics"));
        assert_eq!(reg.stratum_for("metagenomics"), Some("metagenomics"));
        assert_eq!(reg.stratum_for("ehr_clinical_prediction"), Some("clinical"));
    }

    #[test]
    fn unknown_modality_returns_none() {
        let reg = StrataRegistry::from_embedded();
        assert_eq!(reg.stratum_for("no_such_modality"), None);
    }

    #[test]
    fn convenience_fn_matches_registry() {
        assert_eq!(
            modality_stratum("bulk_rnaseq").as_deref(),
            Some("transcriptomics")
        );
        assert_eq!(modality_stratum("gwas").as_deref(), Some("genomics"));
        assert!(modality_stratum("unknown").is_none());
    }
}
