//! Deterministic renderer for the COMPLETE significant-entities table.
//!
//! `report-data.json` is the single, RC-COUNT-validated source of truth for the
//! significant set. The reporting/final_reporting agents narrate over it but
//! transcribe the exhaustive table unreliably (thousands of rows). This module
//! renders that table deterministically from `report-data.json` and (see the
//! harness finalize step) injects it into the terminal report, so completeness
//! is guaranteed regardless of agent behavior. RC-TABLE remains the backstop.

use crate::report_contract::{LiteratureStatus, ReportData};

/// Marker opening the system-owned complete-table block in a report.
pub const FULL_TABLE_START: &str = "<!-- ECAA:full-significant-tables START -->";
/// Marker closing the system-owned complete-table block in a report.
pub const FULL_TABLE_END: &str = "<!-- ECAA:full-significant-tables END -->";

fn fmt_effect(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{x:.4}"),
        None => "—".to_string(),
    }
}

fn fmt_significance(v: Option<f64>) -> String {
    match v {
        // Tiny p/padj values: scientific, locale-independent, byte-stable.
        Some(x) if x != 0.0 && x.abs() < 1e-4 => format!("{x:.3e}"),
        Some(x) => format!("{x:.4}"),
        None => "—".to_string(),
    }
}

fn fmt_literature(l: &LiteratureStatus) -> String {
    match l {
        LiteratureStatus::Concordant { pmid } => format!("concordant (PMID:{pmid})"),
        LiteratureStatus::Discordant { pmid } => format!("discordant (PMID:{pmid})"),
        LiteratureStatus::Unverifiable { pmid } => format!("unverifiable (PMID:{pmid})"),
        LiteratureStatus::Novel => "novel".to_string(),
        LiteratureStatus::NotAssessed => "not_assessed".to_string(),
        // `LiteratureStatus` is `#[non_exhaustive]`; a future status renders
        // as an em dash rather than failing the whole table render.
        _ => "—".to_string(),
    }
}

/// Render the complete significant-entities table(s) as a marker-delimited
/// markdown block, or `None` when no artifact has an inlinable non-empty
/// significant set. Deterministic: pure function of `report_data`, fixed row
/// order (as stored in `report-data.json`), locale-independent number
/// formatting. Modality-agnostic — it renders whatever `EntityRow` values the
/// assembler resolved (gene ids, peak ids, variant loci, pathway names, …) with
/// no domain-specific assumptions.
pub fn significant_entities_section(report_data: &ReportData) -> Option<String> {
    let mut body = String::new();
    let mut any = false;
    for a in &report_data.artifacts {
        if a.spilled_to_attachment_only {
            let n = a
                .n_significant
                .unwrap_or(a.significant_entities.len() as u64);
            body.push_str(&format!(
                "\n### Complete significant entities — {} (spilled)\n\n\
                 The full significant set ({n} entities) is too large to inline; \
                 see the attached table `{}`.\n",
                a.stage_id, a.significant_table_path
            ));
            any = true;
            continue;
        }
        if a.significant_entities.is_empty() {
            continue;
        }
        any = true;
        body.push_str(&format!(
            "\n### Complete significant entities — {}\n\n",
            a.stage_id
        ));
        body.push_str("| Entity | Effect | Significance | Literature |\n");
        body.push_str("| --- | --- | --- | --- |\n");
        for row in &a.significant_entities {
            body.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                row.entity,
                fmt_effect(row.effect),
                fmt_significance(row.significance),
                fmt_literature(&row.literature),
            ));
        }
    }
    if !any {
        return None;
    }
    Some(format!(
        "{FULL_TABLE_START}\n\
         ## Complete significant-entities tables\n\n\
         _Generated deterministically from report-data.json._\n\
         {body}{FULL_TABLE_END}\n"
    ))
}

/// Insert `block` (a [`significant_entities_section`] output) into `report_text`.
/// If a marker block already exists it is REPLACED (idempotent re-injection);
/// otherwise `block` is appended. Pure; never touches the filesystem.
pub fn inject_full_tables(report_text: &str, block: &str) -> String {
    if let (Some(s), Some(e)) = (
        report_text.find(FULL_TABLE_START),
        report_text.find(FULL_TABLE_END),
    ) {
        let end = e + FULL_TABLE_END.len();
        let mut out = String::with_capacity(report_text.len() + block.len());
        out.push_str(&report_text[..s]);
        out.push_str(block.trim_end_matches('\n'));
        out.push_str(&report_text[end..]);
        return out;
    }
    let mut out = report_text.trim_end().to_string();
    out.push_str("\n\n");
    out.push_str(block);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report_contract::{EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary};

    fn artifact(
        stage: &str,
        entities: &[(&str, f64, f64)],
        spilled: bool,
    ) -> ResultArtifactSummary {
        ResultArtifactSummary {
            stage_id: stage.into(),
            artifact: "result.tsv".into(),
            n_total: 100,
            n_significant: Some(entities.len() as u64),
            direction_split: None,
            effect_distribution: None,
            grouped_significant: None,
            ranking: None,
            significant_entities: entities
                .iter()
                .map(|(e, ef, s)| EntityRow {
                    entity: (*e).into(),
                    effect: Some(*ef),
                    significance: Some(*s),
                    literature: LiteratureStatus::Novel,
                })
                .collect(),
            significant_table_path: format!("runtime/outputs/{stage}/result.significant.tsv"),
            full_table_path: format!("runtime/outputs/{stage}/result.full.tsv"),
            spilled_to_attachment_only: spilled,
        }
    }

    #[test]
    fn renders_every_significant_entity_as_a_table_row() {
        let rd = ReportData {
            artifacts: vec![artifact(
                "differential_expression",
                &[
                    ("ENSG_A", 1.5, 0.001),
                    ("ENSG_B", -2.0, 0.0004),
                    ("ENSG_C", 0.8, 0.02),
                ],
                false,
            )],
            literature: None,
        };
        let s = significant_entities_section(&rd).expect("section rendered");
        assert!(
            s.starts_with(FULL_TABLE_START),
            "block is marker-wrapped: {s}"
        );
        assert!(s.trim_end().ends_with(FULL_TABLE_END));
        for e in ["ENSG_A", "ENSG_B", "ENSG_C"] {
            assert!(s.contains(e), "entity {e} must be a rendered row: {s}");
        }
        assert_eq!(
            s.matches("| ENSG_").count(),
            3,
            "one row per significant entity"
        );
        assert!(
            s.contains("differential_expression"),
            "section is labeled by stage_id"
        );
    }

    #[test]
    fn generic_non_gene_entities_render_identically() {
        let rd = ReportData {
            artifacts: vec![artifact(
                "variant_calling",
                &[("chrM:750A>G", 0.98, 1e-9), ("chrM:1438A>G", 0.75, 3e-5)],
                false,
            )],
            literature: None,
        };
        let s = significant_entities_section(&rd).expect("section rendered");
        assert!(
            s.contains("chrM:750A>G") && s.contains("chrM:1438A>G"),
            "renders non-gene entities without domain assumptions: {s}"
        );
    }

    #[test]
    fn spilled_artifact_links_attachment_instead_of_inlining() {
        let mut a = artifact("differential_expression", &[("ENSG_A", 1.0, 0.01)], true);
        a.n_significant = Some(300_001);
        let rd = ReportData {
            artifacts: vec![a],
            literature: None,
        };
        let s = significant_entities_section(&rd).expect("section rendered");
        assert!(
            s.contains("result.significant.tsv"),
            "spilled → link the attachment: {s}"
        );
        assert!(
            !s.contains("| ENSG_A |"),
            "spilled → NOT inlined as a table row: {s}"
        );
    }

    #[test]
    fn no_inlinable_artifacts_returns_none() {
        let rd = ReportData {
            artifacts: vec![],
            literature: None,
        };
        assert!(significant_entities_section(&rd).is_none());
    }

    #[test]
    fn multiple_artifacts_each_get_a_section() {
        let rd = ReportData {
            artifacts: vec![
                artifact("differential_expression", &[("ENSG_A", 1.0, 0.01)], false),
                artifact(
                    "pathway_enrichment",
                    &[("HALLMARK_HYPOXIA", 2.1, 0.001)],
                    false,
                ),
            ],
            literature: None,
        };
        let s = significant_entities_section(&rd).unwrap();
        assert!(s.contains("differential_expression") && s.contains("pathway_enrichment"));
        assert!(s.contains("ENSG_A") && s.contains("HALLMARK_HYPOXIA"));
    }

    #[test]
    fn inject_appends_when_no_marker_present() {
        let report = "# Report\n\nSome narrative.\n";
        let block = format!("{FULL_TABLE_START}\ncontent A\n{FULL_TABLE_END}\n");
        let out = inject_full_tables(report, &block);
        assert!(
            out.starts_with("# Report"),
            "keeps original narrative: {out}"
        );
        assert!(out.contains("content A"), "appends the block: {out}");
        assert_eq!(out.matches(FULL_TABLE_START).count(), 1);
    }

    #[test]
    fn inject_replaces_existing_marker_block_and_is_idempotent() {
        let report = format!(
            "# Report\n\nNarrative.\n\n{FULL_TABLE_START}\nOLD\n{FULL_TABLE_END}\n\nTail.\n"
        );
        let block = format!("{FULL_TABLE_START}\nNEW\n{FULL_TABLE_END}\n");
        let once = inject_full_tables(&report, &block);
        assert!(
            once.contains("NEW") && !once.contains("OLD"),
            "replaces old block: {once}"
        );
        assert!(
            once.contains("Narrative.") && once.contains("Tail."),
            "preserves surrounding text: {once}"
        );
        assert_eq!(
            once.matches(FULL_TABLE_START).count(),
            1,
            "exactly one block"
        );
        let twice = inject_full_tables(&once, &block);
        assert_eq!(
            twice, once,
            "idempotent: re-injecting the same block is a no-op"
        );
    }
}
