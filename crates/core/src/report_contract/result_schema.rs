//! `ResultSchema` — declares how a terminal analytical atom's primary
//! result artifact is read by the report-data assembler + reporting
//! invariants validator. Modality-agnostic: all domain-specific meaning
//! (entity/significance/effect column names) enters only through these
//! declarations.

/// Declares how to read a terminal atom's primary tabular result
/// artifact. `artifact` is a path relative to the stage's output
/// directory (e.g. `de_results.tsv`); `entity_column` names the row
/// identifier (gene, pathway, variant_id, ...). `significance` and
/// `signed_effect_column` are both optional — a schema with neither
/// still yields the reduced (unsigned, unfiltered) contract.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ResultSchema {
    pub artifact: String,
    pub entity_column: String,
    /// Additional accepted header names for the entity column (e.g. an atom
    /// whose canonical `entity_column` is `gene` but whose agent may emit the
    /// row identifier under a synonym like `gene_id`/`gene_name`/`symbol`):
    /// resolution tries `entity_column` then each alias, in order. Data-driven
    /// — the candidate names live in the atom's declaration, never hardcoded in
    /// the assembler. Empty (the default) means only `entity_column` is
    /// accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entity_column_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub significance: Option<Significance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub signed_effect_column: Option<String>,
    /// Additional accepted header names for the signed-effect column (e.g. a
    /// DESeq2-native name + its ECAA-canonical alias): resolution tries
    /// `signed_effect_column` then each alias, in order. Data-driven — the
    /// candidate names live in the atom's declaration, never hardcoded in the
    /// assembler. Empty (the default) means only `signed_effect_column` is
    /// accepted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signed_effect_aliases: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub grouping_column: Option<String>,
}

/// Names the column + threshold + comparator the assembler uses to
/// split a result artifact into "significant" vs the full set.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, ts_rs::TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct Significance {
    pub column: String,
    pub threshold: f64,
    pub comparator: Comparator,
}

/// How `Significance::threshold` is compared against a row's value in
/// `Significance::column`. `Lt` (e.g. `padj < 0.05`) and `Gt` (e.g. a
/// score `> 0.9`) cover the comparators declared atoms need today;
/// `#[non_exhaustive]` leaves room to add more without a breaking change.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    ts_rs::TS,
    schemars::JsonSchema,
)]
#[ts(export)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum Comparator {
    Lt,
    Gt,
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_signed_result_schema_from_yaml() {
        let y = r#"
artifact: de_results.tsv
entity_column: gene
significance: { column: padj, threshold: 0.05, comparator: lt }
signed_effect_column: log2FoldChange
"#;
        let s: super::ResultSchema = serde_yaml_ng::from_str(y).unwrap();
        assert_eq!(s.artifact, "de_results.tsv");
        assert_eq!(s.entity_column, "gene");
        assert_eq!(s.significance.as_ref().unwrap().threshold, 0.05);
        assert_eq!(
            s.significance.as_ref().unwrap().comparator,
            super::Comparator::Lt
        );
        assert_eq!(s.signed_effect_column.as_deref(), Some("log2FoldChange"));
    }

    #[test]
    fn unsigned_schema_has_no_effect_column() {
        let y = "artifact: variants.vcf\nentity_column: variant_id\n";
        let s: super::ResultSchema = serde_yaml_ng::from_str(y).unwrap();
        assert!(s.signed_effect_column.is_none());
        assert!(s.significance.is_none());
        assert!(s.signed_effect_aliases.is_empty());
        assert!(s.entity_column_aliases.is_empty());
    }

    #[test]
    fn parses_entity_column_aliases_from_yaml() {
        let y = r#"
artifact: de_results.tsv
entity_column: gene
entity_column_aliases: [gene_id, gene_name, symbol]
"#;
        let s: super::ResultSchema = serde_yaml_ng::from_str(y).unwrap();
        assert_eq!(s.entity_column, "gene");
        assert_eq!(s.entity_column_aliases, vec!["gene_id", "gene_name", "symbol"]);
    }

    #[test]
    fn parses_signed_effect_aliases_from_yaml() {
        let y = r#"
artifact: de_results.tsv
entity_column: gene
signed_effect_column: log2FoldChange
signed_effect_aliases: [log2FC, logFC]
"#;
        let s: super::ResultSchema = serde_yaml_ng::from_str(y).unwrap();
        assert_eq!(s.signed_effect_column.as_deref(), Some("log2FoldChange"));
        assert_eq!(s.signed_effect_aliases, vec!["log2FC", "logFC"]);
    }
}
