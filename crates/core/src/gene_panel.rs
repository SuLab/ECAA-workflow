//! `GenePanel` definition.
//!
//! Curated marker-gene metadata loaded from `config/gene-panels/*.yaml`.
//! A panel is either a flat list (`genes:`) or a contrast-grouped map
//! (`contrasts:`) — never both. Schema sidecar (`_gene_panel.schema.json`)
//! enforces the `oneOf` discipline at registry-load time.
//!
//! Composer / discover_* consumers pick the format their stage needs;
//! the registry just hands back the structured data. No domain logic
//! lives in this module.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// One gene-panel definition. Authored as YAML under `config/gene-panels/`,
/// consumed by [`crate::gene_panel_registry::GenePanelRegistry`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct GenePanel {
    /// Stable id, must match the filename stem.
    pub id: String,

    /// Free-form description; surfaces in SME prompts and CONTEXT.md.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub description: Option<String>,

    /// Citation / provenance. Free-form; used in SME-facing docs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub source: Option<String>,

    /// Flat list of gene symbols. Set when the panel has no
    /// directional contrast (e.g. `pain_pathway_canonical`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub genes: Option<Vec<String>>,

    /// Map of directional bucket name → entries. Set when the panel
    /// is split by contrast (e.g. `up_in_degeneration` /
    /// `up_in_healthy` for `ivd_positive_controls`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub contrasts: Option<BTreeMap<String, Vec<GeneEntry>>>,
}

/// One gene entry inside a contrast bucket. The `rationale` is
/// SME-facing prose explaining why the gene belongs in this bucket
/// (e.g. "Matrix metalloproteinase; canonical cartilage degradation
/// marker").
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct GeneEntry {
    /// Symbol.
    pub symbol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    /// Rationale.
    pub rationale: Option<String>,
}

impl GenePanel {
    /// Returns the union of all gene symbols in the panel, regardless
    /// of contrast bucket. Useful for "is gene X mentioned anywhere in
    /// this panel?" lookups without forcing the consumer to walk both
    /// shapes.
    pub fn all_symbols(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        if let Some(genes) = &self.genes {
            for g in genes {
                out.push(g.as_str());
            }
        }
        if let Some(contrasts) = &self.contrasts {
            for entries in contrasts.values() {
                for entry in entries {
                    out.push(entry.symbol.as_str());
                }
            }
        }
        out
    }

    /// True when the panel uses the flat-list format.
    pub fn is_flat(&self) -> bool {
        self.genes.is_some()
    }

    /// True when the panel uses the contrast-grouped format.
    pub fn is_contrasted(&self) -> bool {
        self.contrasts.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_panel() -> GenePanel {
        GenePanel {
            id: "pain_pathway_canonical".into(),
            description: Some("desc".into()),
            source: None,
            genes: Some(vec!["SCN9A".into(), "TRPV1".into()]),
            contrasts: None,
        }
    }

    fn contrasted_panel() -> GenePanel {
        let mut contrasts: BTreeMap<String, Vec<GeneEntry>> = BTreeMap::new();
        contrasts.insert(
            "up_in_degeneration".into(),
            vec![GeneEntry {
                symbol: "MMP13".into(),
                rationale: Some("matrix metalloproteinase".into()),
            }],
        );
        contrasts.insert(
            "up_in_healthy".into(),
            vec![GeneEntry {
                symbol: "COL2A1".into(),
                rationale: None,
            }],
        );
        GenePanel {
            id: "sample_contrasted_panel".into(),
            description: None,
            source: None,
            genes: None,
            contrasts: Some(contrasts),
        }
    }

    #[test]
    fn flat_panel_classifies_as_flat() {
        let p = flat_panel();
        assert!(p.is_flat());
        assert!(!p.is_contrasted());
    }

    #[test]
    fn contrasted_panel_classifies_as_contrasted() {
        let p = contrasted_panel();
        assert!(!p.is_flat());
        assert!(p.is_contrasted());
    }

    #[test]
    fn all_symbols_unions_flat_panel() {
        let p = flat_panel();
        assert_eq!(p.all_symbols(), vec!["SCN9A", "TRPV1"]);
    }

    #[test]
    fn all_symbols_unions_contrasted_panel() {
        let p = contrasted_panel();
        // BTreeMap iteration is sorted; up_in_degeneration < up_in_healthy.
        assert_eq!(p.all_symbols(), vec!["MMP13", "COL2A1"]);
    }
}
