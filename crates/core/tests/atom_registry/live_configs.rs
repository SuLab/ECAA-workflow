//! Live-config regression net. Three assertions covering every YAML
//! file the runtime touches:
//!
//! 1. Every `<name>.yaml` under `config/stage-atoms/` loads as a valid
//! `AtomDefinition` via `AtomRegistry::load_from_dir`.
//! 2. Every `<name>.yaml` under `config/archetypes/` loads as a valid
//! `ArchetypeDefinition` via `ArchetypeRegistry::load_from_dir`.
//! 3. The two registries are non-empty.
//!
//! The sibling `taxonomy_validation::all_live_taxonomies_validate` test
//! covers `config/stage-taxonomies/` already. Together these three
//! tests fail loudly on any silent schema drift in the live config tree
//! — the failure mode that produced the "no DAG built"
//! incident, where a taxonomy YAML was committed missing a
//! schema-required field and the chat path silently fell through to
//! `emit_package` before tripping the precondition gate.

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use std::path::{Path, PathBuf};

fn config_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

#[test]
fn every_live_atom_loads() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry::load_from_dir must succeed for every file in config/stage-atoms/");
    assert!(
        !reg.is_empty(),
        "AtomRegistry loaded zero atoms — config/stage-atoms/ likely missing"
    );
}

#[test]
fn every_live_archetype_loads() {
    let reg = ArchetypeRegistry::load_from_dir(&config_root().join("archetypes")).expect(
        "ArchetypeRegistry::load_from_dir must succeed for every file in config/archetypes/",
    );
    assert!(
        !reg.is_empty(),
        "ArchetypeRegistry loaded zero archetypes — config/archetypes/ likely missing"
    );
}

#[test]
fn qc_preprocessing_separates_metrics_from_filtered_counts() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load the QC preprocessing contract");
    let qc = reg
        .get("qc_preprocessing")
        .expect("qc_preprocessing must be present");

    let metrics = qc
        .outputs
        .first()
        .expect("QC metrics must remain the primary plotting and validation output");
    assert_eq!(metrics.name, "qc_metrics");
    assert_eq!(metrics.semantic_type.stable_id(), "ecaax:qc_metrics");
    assert_eq!(
        metrics.physical_format.as_ref().map(|f| f.iri.as_str()),
        Some("format:3475")
    );

    let filtered = qc
        .outputs
        .iter()
        .find(|port| port.name == "filtered_count_matrix")
        .expect("QC must expose the filtered matrix to downstream normalization");
    assert_eq!(filtered.semantic_type.stable_id(), "data:3917");
    assert_eq!(
        filtered.physical_format.as_ref().map(|f| f.iri.as_str()),
        Some("format:3475")
    );
    assert_eq!(filtered.statistical_state.as_deref(), Some("raw_counts"));
    assert_eq!(filtered.normalization_state.as_deref(), Some("raw"));

    assert!(
        qc.description
            .contains("do not query a live annotation service"),
        "QC instructions must keep replay-critical annotation lookup inside the retained package"
    );
}

#[test]
fn differential_expression_default_call_set_matches_result_schema() {
    let reg = AtomRegistry::load_from_dir(&config_root().join("stage-atoms"))
        .expect("AtomRegistry must load the differential-expression contract");
    let de = reg
        .get("differential_expression")
        .expect("differential_expression must be present");

    let effect_threshold = de
        .parameters
        .iter()
        .find(|parameter| parameter.name == "log2fc_threshold")
        .expect("differential_expression must declare log2fc_threshold");
    assert_eq!(
        effect_threshold.default,
        Some(serde_json::json!(0.0)),
        "the default call set must not add an effect-size criterion that is absent from result_schema.significance"
    );

    let significance = de
        .result_schema
        .as_ref()
        .and_then(|schema| schema.significance.as_ref())
        .expect("differential_expression must declare its significance rule");
    assert_eq!(significance.column, "padj");
    assert_eq!(significance.threshold, 0.05);

    let shrinkage_columns = de
        .non_determinism
        .iter()
        .find(|ack| ack.artifact == "de_results.tsv")
        .and_then(|ack| ack.columns.as_ref())
        .expect("adaptive-shrinkage declaration must name its affected columns");
    let mut accepted_effect_headers = vec![de
        .result_schema
        .as_ref()
        .and_then(|schema| schema.signed_effect_column.as_deref())
        .expect("differential_expression must declare a signed effect")
        .to_string()];
    accepted_effect_headers.extend(
        de.result_schema
            .as_ref()
            .expect("result schema present")
            .signed_effect_aliases
            .iter()
            .cloned(),
    );
    // Acknowledged non-determinism excuses a replay divergence, so it must name
    // the SEPARATELY-REPORTED shrunken estimate and never the primary one.
    // Listing the primary `log2FoldChange` here would bucket a genuine change in
    // the headline effect as AcknowledgedNonDeterminism instead of failing,
    // while the retained stat/pvalue/padj it is coupled to still had to match.
    // Every accepted spelling of the effect header needs its shrunken form
    // covered, so an executor choosing a valid alias cannot dodge the band.
    for header in &accepted_effect_headers {
        let shrunken = format!("{header}_shrunken");
        assert!(
            shrinkage_columns.contains(&shrunken),
            "adaptive-shrinkage replay declaration must cover shrunken effect header {shrunken}"
        );
        assert!(
            !shrinkage_columns.contains(header),
            "primary effect header {header} must stay coupled to the retained statistic and \
             p-values; declaring it non-deterministic would excuse a real divergence"
        );
    }
}
