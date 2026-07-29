//! Deterministic report-data assembler: reads every confirmatory
//! terminal atom's declared [`ResultSchema`], summarizes its primary
//! result artifact, writes the supplementary tables, joins the
//! literature-contextualization outputs once over the entire collected
//! significant set, and serializes the canonical `report-data.json` the
//! reporting agent narrates over.
//!
//! Never touches the wall clock (threads [`Clock`] per the emit-path
//! determinism contract) and never hardcodes a modality-specific column
//! name — every column reference flows through the caller-supplied
//! `schemas` map.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::clock::Clock;

use super::report_data::{
    build_entity_rows, join_literature, load_policy_column_synonyms, should_spill,
};
use super::{
    rank_artifact, summarize_artifact, write_supplementary, ReportData, ResultArtifactSummary,
    ResultSchema,
};

/// The stage_id of the literature-contextualization atom whose outputs
/// (`claims_evidence_matrix.csv` + `result.json`) feed [`join_literature`].
/// The stage whose `claims_evidence_matrix.csv` + `result.json` feed
/// [`join_literature`]. Shared as the single source of truth with the composer
/// (`composer_v4::report_data_synthesis`), which adds the ordering edge
/// `CONTEXTUALIZE_STAGE_ID -> assemble_report_data` so the assembler never runs
/// before the literature matrix exists — the read-side and the dependency-side
/// therefore cannot drift.
pub const CONTEXTUALIZE_STAGE_ID: &str = "contextualize_findings_with_literature";

/// Number of canonical rows retained per sign class for report top-hit tables.
/// The list is deliberately larger than a typical narrative excerpt so a
/// report may render a shorter prefix without recomputing the order.
pub const REPORT_RANKING_TOP_N: usize = 25;

/// Picks the delimiter for a result artifact by sniffing its CONTENT, using
/// the file extension only as the fallback when the content is ambiguous
/// (`.tsv` → tab, anything else including `.csv` → comma — the historical
/// mapping, preserved so extensionless paths keep behaving as before).
///
/// Content-first matters here because a table written with R's `write.csv()`
/// under a `.tsv` name parses on tab as ONE column named after the entire
/// header line. `crate::finalize::load` then finds no recognizable entity
/// column and drops the table as "not a result table" — a real result silently
/// missing from the report rather than a loud failure. Shares the sniff rule
/// with the re-execution comparator via [`crate::table_delimiter`] so the two
/// can never disagree about how a given table parses.
pub(crate) fn delimiter_for(path: &Path) -> u8 {
    crate::table_delimiter::sniff_delimiter_from_path(path, delimiter_from_extension(path))
}

fn delimiter_from_extension(path: &Path) -> u8 {
    if path.extension().and_then(|e| e.to_str()) == Some("tsv") {
        b'\t'
    } else {
        b','
    }
}

/// Reads a delimited table (TSV or CSV, by extension) into
/// `(headers, rows)`. `pub(crate)` so the reporting-invariants validator's
/// RC-COUNT check can recompute over the same source artifact this
/// assembler read, without duplicating the parsing logic.
pub(crate) fn read_table(path: &Path) -> Result<(csv::StringRecord, Vec<csv::StringRecord>)> {
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter_for(path))
        .has_headers(true)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("opening result artifact {}", path.display()))?;
    let headers = reader
        .headers()
        .with_context(|| format!("reading header row of {}", path.display()))?
        .clone();
    let rows: Vec<csv::StringRecord> = reader
        .records()
        .collect::<Result<Vec<_>, csv::Error>>()
        .with_context(|| format!("parsing rows of {}", path.display()))?;
    Ok((headers, rows))
}

/// Assembles `report-data.json` (+ supplementary tables) for every
/// `(stage_id, schema)` pair in `schemas` and writes it to
/// `package_root/runtime/outputs/reporting/report-data.json`.
///
/// A `stage_id` whose declared artifact isn't present on disk is skipped
/// (not an error) — a report generator degrades gracefully rather than
/// failing the whole assembly over one late/absent stage. When the
/// literature-contextualization stage's output dir is absent, `literature`
/// is `None` (no contextualization ran) rather than attempting the join.
///
/// `clock` is threaded per the architecture rule that no emit-path
/// function reads the wall clock directly; `ReportData` carries no
/// timestamp field today, so it is otherwise unused — reserved so a later
/// timestamped field doesn't reopen this call site to `SystemTime::now()`.
pub fn assemble_report_data(
    package_root: &Path,
    schemas: &BTreeMap<String, ResultSchema>,
    clock: &dyn Clock,
) -> Result<ReportData> {
    let _ = clock;

    // Loaded once from the emitted package's interpretation policy and shared
    // across every artifact's column resolution — data-driven tolerant column
    // matching (see [`load_policy_column_synonyms`]). Absent policy → empty →
    // declared-name-only resolution.
    let synonyms = load_policy_column_synonyms(package_root);

    let outputs_dir = package_root.join("runtime").join("outputs");
    let mut artifacts: Vec<ResultArtifactSummary> = Vec::new();
    // Every collected significant EntityRow across all artifacts, plus the
    // (artifact index, start, end) range it came from, so the literature
    // join's mutations can be scattered back onto the right artifact.
    let mut all_sig_entities = Vec::new();
    let mut ranges: Vec<(usize, usize, usize)> = Vec::new();

    for (stage_id, schema) in schemas {
        let stage_dir = outputs_dir.join(stage_id);
        let artifact_path = stage_dir.join(&schema.artifact);
        if !artifact_path.exists() {
            continue;
        }

        let (headers, rows) = read_table(&artifact_path)?;
        let stats = summarize_artifact(&rows, &headers, schema, &synonyms);
        let ranking = rank_artifact(&rows, &headers, schema, &synonyms, REPORT_RANKING_TOP_N);

        let stem = Path::new(&schema.artifact)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(schema.artifact.as_str());
        let (sig_rel, full_rel) = write_supplementary(
            &stage_dir,
            stem,
            &headers,
            &rows,
            &stats.significant_row_indices,
        )
        .with_context(|| format!("writing supplementary tables for {stage_id}"))?;

        let spilled = should_spill(stats.significant_row_indices.len());
        let entities = if spilled {
            Vec::new()
        } else {
            build_entity_rows(
                &rows,
                &headers,
                schema,
                &synonyms,
                &stats.significant_row_indices,
            )
        };

        let start = all_sig_entities.len();
        all_sig_entities.extend(entities.iter().cloned());
        let end = all_sig_entities.len();
        ranges.push((artifacts.len(), start, end));

        artifacts.push(ResultArtifactSummary {
            stage_id: stage_id.clone(),
            artifact: schema.artifact.clone(),
            n_total: stats.n_total,
            n_significant: stats.n_significant,
            direction_split: stats.direction_split,
            effect_distribution: stats.effect_distribution,
            grouped_significant: stats.grouped_significant,
            ranking,
            significant_entities: entities,
            significant_table_path: format!("runtime/outputs/{stage_id}/{sig_rel}"),
            full_table_path: format!("runtime/outputs/{stage_id}/{full_rel}"),
            spilled_to_attachment_only: spilled,
        });
    }

    let contextualize_dir = outputs_dir.join(CONTEXTUALIZE_STAGE_ID);
    let literature = if contextualize_dir.is_dir() {
        let matrix_path = contextualize_dir.join("claims_evidence_matrix.csv");
        let result_json_path = contextualize_dir.join("result.json");
        let rollup = join_literature(&mut all_sig_entities, &matrix_path, &result_json_path);
        for (artifact_idx, start, end) in &ranges {
            artifacts[*artifact_idx].significant_entities = all_sig_entities[*start..*end].to_vec();
        }
        Some(rollup)
    } else {
        None
    };

    let report = ReportData {
        artifacts,
        literature,
    };

    let reporting_dir = outputs_dir.join("reporting");
    std::fs::create_dir_all(&reporting_dir)
        .with_context(|| format!("creating {}", reporting_dir.display()))?;
    let report_json_path = reporting_dir.join("report-data.json");
    let pretty = serde_json::to_string_pretty(&report).context("serializing report-data.json")?;
    std::fs::write(&report_json_path, pretty)
        .with_context(|| format!("writing {}", report_json_path.display()))?;

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FrozenClock;
    use crate::report_contract::{Comparator, Significance};

    fn de_schema() -> ResultSchema {
        ResultSchema {
            artifact: "de_results.tsv".into(),
            entity_column: "gene".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("log2FoldChange".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        }
    }

    fn stage_de_results(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("de_results.tsv"),
            "gene\tlog2FoldChange\tpadj\n\
             ENSG1\t5.0\t0.001\n\
             ENSG2\t-4.8\t0.002\n\
             ENSG3\t0.1\t0.9\n",
        )
        .unwrap();
    }

    fn stage_claims_matrix(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("claims_evidence_matrix.csv"),
            "finding_id,entity,entity_kind,prior_pmid,concordance_flag,direction_from_prior,lfc,padj,evidence_quote\n\
             ENSG1,GONE,gene,555,same_direction,up,5.0,0.001,prior quote\n\
             ENSG2,GTWO,gene,,no_prior_finding,,-4.8,0.002,\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("result.json"),
            r#"{"cited_pmids": ["555"], "excluded_nonsig": {}}"#,
        )
        .unwrap();
    }

    #[test]
    fn assembles_report_data_with_supplementary_tables_and_literature() {
        let tmp = tempfile::tempdir().unwrap();
        let outputs = tmp.path().join("runtime").join("outputs");
        stage_de_results(&outputs.join("differential_expression"));
        stage_claims_matrix(&outputs.join("contextualize_findings_with_literature"));

        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());

        let clock = FrozenClock::default();
        let report = assemble_report_data(tmp.path(), &schemas, &clock).unwrap();

        // Hand count: 2 of 3 rows pass padj < 0.05.
        assert_eq!(report.artifacts.len(), 1);
        let artifact = &report.artifacts[0];
        assert_eq!(artifact.stage_id, "differential_expression");
        assert_eq!(artifact.n_total, 3);
        assert_eq!(artifact.n_significant, Some(2));
        assert!(!artifact.spilled_to_attachment_only);
        assert_eq!(artifact.significant_entities.len(), 2);
        let ranking = artifact
            .ranking
            .as_ref()
            .expect("resolved schema must produce a canonical ranking");
        assert_eq!(
            ranking.top_enriched().map(|term| term.entity.as_str()),
            Some("ENSG1")
        );
        assert_eq!(
            ranking.top_depleted().map(|term| term.entity.as_str()),
            Some("ENSG2")
        );

        let report_json_path = outputs.join("reporting").join("report-data.json");
        assert!(report_json_path.exists());
        let on_disk: super::ReportData =
            serde_json::from_str(&std::fs::read_to_string(&report_json_path).unwrap()).unwrap();
        assert_eq!(on_disk, report);

        let sig_table = outputs
            .join("differential_expression")
            .join("de_results.significant.tsv");
        let full_table = outputs
            .join("differential_expression")
            .join("de_results.full.tsv");
        assert!(sig_table.exists());
        assert!(full_table.exists());
        assert_eq!(
            std::fs::read_to_string(&sig_table).unwrap().lines().count(),
            3
        );
        assert_eq!(
            std::fs::read_to_string(&full_table)
                .unwrap()
                .lines()
                .count(),
            4
        );

        // Rollup built from the matrix rows: 1 same_direction (entity GONE) +
        // 1 no_prior_finding (novel).
        let lit = report
            .literature
            .as_ref()
            .expect("contextualize dir present");
        assert_eq!(lit.concordant.len(), 1);
        // LitFinding.entity is the matrix's `entity` column, not the report-data
        // row identifier (which matched by finding_id).
        assert_eq!(lit.concordant[0].entity, "GONE");
        assert_eq!(lit.novel_count, 1);

        let by_entity: BTreeMap<_, _> = artifact
            .significant_entities
            .iter()
            .map(|e| (e.entity.clone(), e.literature.clone()))
            .collect();
        assert_eq!(
            by_entity["ENSG1"],
            crate::report_contract::LiteratureStatus::Concordant {
                pmid: "555".to_string()
            }
        );
        assert_eq!(
            by_entity["ENSG2"],
            crate::report_contract::LiteratureStatus::Novel
        );
    }

    #[test]
    fn skips_missing_artifact_and_produces_no_literature_without_contextualize_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // Declare a schema but never write the artifact.
        let mut schemas = BTreeMap::new();
        schemas.insert("differential_expression".to_string(), de_schema());

        let clock = FrozenClock::default();
        let report = assemble_report_data(tmp.path(), &schemas, &clock).unwrap();

        assert!(report.artifacts.is_empty());
        assert!(report.literature.is_none());

        let report_json_path = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("reporting")
            .join("report-data.json");
        assert!(
            report_json_path.exists(),
            "report-data.json is always written"
        );
    }

    #[test]
    fn threads_grouped_significant_onto_summary_when_grouping_declared() {
        let tmp = tempfile::tempdir().unwrap();
        let outputs = tmp.path().join("runtime").join("outputs");
        let pe = outputs.join("pathway_enrichment");
        std::fs::create_dir_all(&pe).unwrap();
        std::fs::write(
            pe.join("pathway_results.tsv"),
            "pathway\tcollection\tpadj\n\
             P1\tHALLMARK\t0.01\n\
             P2\tGO_BP\t0.001\n\
             P3\tGO_BP\t0.02\n\
             P4\tKEGG\t0.9\n",
        )
        .unwrap();

        let schema = ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "pathway".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: None,
            signed_effect_aliases: Vec::new(),
            grouping_column: Some("collection".into()),
        };
        let mut schemas = BTreeMap::new();
        schemas.insert("pathway_enrichment".to_string(), schema);

        let clock = FrozenClock::default();
        let report = assemble_report_data(tmp.path(), &schemas, &clock).unwrap();
        let grouped = report.artifacts[0]
            .grouped_significant
            .as_ref()
            .expect("grouping_column resolved → grouped_significant present");
        assert_eq!(
            grouped,
            &vec![
                crate::report_contract::report_data::GroupCount {
                    group: "GO_BP".into(),
                    n_significant: 2
                },
                crate::report_contract::report_data::GroupCount {
                    group: "HALLMARK".into(),
                    n_significant: 1
                },
            ]
        );
        let ranking = report.artifacts[0]
            .ranking
            .as_ref()
            .expect("unsigned pathway artifact still has one canonical list");
        assert!(!ranking.directional);
        assert_eq!(
            ranking
                .undirected
                .iter()
                .map(|term| term.entity.as_str())
                .collect::<Vec<_>>(),
            vec!["P2", "P1", "P3"]
        );
    }
}
