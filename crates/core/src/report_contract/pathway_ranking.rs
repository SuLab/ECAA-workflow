//! Deterministic top-term selection over a tabular result artifact.
//!
//! # Why this exists
//!
//! "The top enriched pathway" and "the top depleted pathway" are the two
//! sentences a reader most often carries away from an enrichment stage,
//! and nothing in the system defined how they were chosen. A deposited
//! run recorded `top_depleted_pathway` as
//! `GOBP_POSITIVE_REGULATION_OF_SYNAPSE_ASSEMBLY` (NES −2.026, padj
//! 0.00262) while its own `pathway_results.tsv` held
//! `GOBP_POSITIVE_REGULATION_OF_NERVOUS_SYSTEM_DEVELOPMENT` at padj
//! 0.000419 (six times more significant) and
//! `GOBP_EPIDERMIS_MORPHOGENESIS` at NES −2.150 (a larger depletion).
//! The claimed term was argmin of neither. It could not be: the value was
//! written by hand into an agent-authored `result.json`, while the
//! deterministic stage script only ever wrote a `pathway_summary.json`
//! ordered by adjusted p — the two artifacts had no shared rule.
//!
//! This module is that rule, in source, so "top" means one thing
//! everywhere: the stage script, the figure, the narrative, and the
//! invariant that re-derives it.
//!
//! # The rule
//!
//! Over the rows of a result artifact read through its
//! [`ResultSchema`]:
//!
//! 1. **Eligibility.** When the schema declares a [`Significance`] block,
//!    only rows passing its comparator + threshold are candidates. A
//!    declared-but-unresolvable significance column yields `None` from
//!    [`rank_artifact`] — nothing was assessed, which must never be
//!    conflated with "no term qualified".
//! 2. **Sign class.** When a signed-effect column resolves, a candidate
//!    is *enriched* (effect > 0), *depleted* (effect < 0), or
//!    *undirected* (effect is zero, NA, or unparseable). When no effect
//!    column resolves, the modality is unsigned: every candidate is
//!    undirected and there is no direction split at all.
//! 3. **Order, within a sign class.** Most significant first, then larger
//!    |effect|, then entity name ascending, then row order. Stated as a
//!    total order:
//!    `(significance, -|effect|, entity, row_index)` ascending, where
//!    "significance ascending" flips to descending for
//!    [`Comparator::Gt`] (a score where larger is stronger).
//! 4. **Truncation.** The first `top_n` of each class.
//!
//! The tail of that order is not decoration. fgsea's BH adjustment emits
//! many rows sharing one adjusted p to the last representable digit; the
//! deposited run had four terms tied at `padj = 0.0027`. Without the
//! |effect| and entity tiebreaks, "the top depleted pathway" would be
//! whichever of them the writer happened to emit first.
//!
//! # Modality-agnostic by construction
//!
//! Nothing here names a column, a tool, or a domain. Enrichment (NES /
//! padj), differential expression (log2FC / padj), and a score-based
//! artifact where larger is better ([`Comparator::Gt`]) all route through
//! the same code; an artifact with no effect column (variant calling)
//! simply produces one undirected list. All meaning enters through the
//! atom's `result_schema` declaration.
//!
//! Deterministic: pure over its inputs, no clock, no map iteration order,
//! total ordering with no float-comparison ambiguity (`f64::total_cmp`).

use crate::report_contract::report_data::PolicyColumnSynonyms;
use crate::report_contract::result_schema::{Comparator, ResultSchema, Significance};

/// Which end of the significance scale is "more significant".
///
/// Derived from the schema's [`Comparator`]: a `padj < 0.05` declaration
/// means smaller is stronger; a `score > 0.9` declaration means larger is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignificanceOrder {
    /// Smaller values rank first (p-values, adjusted p-values, q-values).
    SmallerIsStronger,
    /// Larger values rank first (scores, likelihoods, confidences).
    LargerIsStronger,
}

impl From<Comparator> for SignificanceOrder {
    fn from(c: Comparator) -> Self {
        match c {
            Comparator::Lt => SignificanceOrder::SmallerIsStronger,
            Comparator::Gt => SignificanceOrder::LargerIsStronger,
        }
    }
}

/// The sign class a ranked row falls into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum SignClass {
    /// Signed effect > 0.
    Enriched,
    /// Signed effect < 0.
    Depleted,
    /// Effect is zero, absent, or the artifact declares no effect column.
    Undirected,
}

/// One row of a result artifact, reduced to the three fields ranking
/// needs plus its position in the source table.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RankedTerm {
    /// Row identifier as read from the declared entity column (pathway
    /// term, gene, variant id, ...).
    pub entity: String,
    /// Signed effect (NES, log2FC, ...). `None` when the artifact declares
    /// no effect column or the cell does not parse as a finite number.
    pub effect: Option<f64>,
    /// Significance value (adjusted p, q-value, score, ...). `None` when
    /// the artifact declares no significance column.
    pub significance: Option<f64>,
    /// Index into the `rows` slice the term was read from — lets a caller
    /// recover the full row without re-parsing, and is the final tiebreak.
    pub row_index: usize,
}

impl RankedTerm {
    /// Sign class of this row. `Undirected` when the effect is absent,
    /// non-finite, or exactly zero — a zero-effect row is genuinely in
    /// neither class, mirroring how `direction_split` counts it in
    /// `n_significant` but in neither `up` nor `down`.
    pub fn sign_class(&self) -> SignClass {
        match self.effect {
            Some(e) if e.is_finite() && e > 0.0 => SignClass::Enriched,
            Some(e) if e.is_finite() && e < 0.0 => SignClass::Depleted,
            _ => SignClass::Undirected,
        }
    }
}

/// Top-N terms per sign class under the module's stated rule.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct PathwayRanking {
    /// Top-N with a positive signed effect. Always empty when
    /// `directional` is false.
    pub enriched: Vec<RankedTerm>,
    /// Top-N with a negative signed effect. Always empty when
    /// `directional` is false.
    pub depleted: Vec<RankedTerm>,
    /// Top-N with no usable direction. For an unsigned artifact this is
    /// the ONLY populated list — the single honest answer to "what are the
    /// top terms" when the data cannot say which way they run.
    pub undirected: Vec<RankedTerm>,
    /// `true` iff a signed-effect column resolved, i.e. iff
    /// `enriched`/`depleted` are meaningful. Callers must not read an
    /// empty `depleted` as "no depleted terms" when this is `false`.
    pub directional: bool,
    /// Maximum number of rows retained in each list.
    #[serde(default)]
    pub retained_per_class: usize,
    /// Number of eligible positive-effect rows before truncation.
    #[serde(default)]
    pub eligible_enriched: usize,
    /// Number of eligible negative-effect rows before truncation.
    #[serde(default)]
    pub eligible_depleted: usize,
    /// Number of eligible undirected rows before truncation.
    #[serde(default)]
    pub eligible_undirected: usize,
}

impl PathwayRanking {
    /// The single top enriched term, or `None` for an unsigned artifact
    /// or one with no positive-effect candidate.
    pub fn top_enriched(&self) -> Option<&RankedTerm> {
        self.enriched.first()
    }

    /// The single top depleted term, or `None` for an unsigned artifact
    /// or one with no negative-effect candidate.
    pub fn top_depleted(&self) -> Option<&RankedTerm> {
        self.depleted.first()
    }
}

/// Resolved column indices for a ranking pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RankingColumns {
    /// Row-identifier column. Always resolved — a ranking with no way to
    /// name its rows is not computable.
    pub entity: usize,
    /// `None` for an unsigned artifact — the direction split is then
    /// skipped entirely rather than guessed.
    pub effect: Option<usize>,
    /// `None` when the artifact declares no significance column.
    pub significance: Option<usize>,
}

/// Resolves a header name to its column index. Never positional.
fn resolve_column(headers: &csv::StringRecord, name: &str) -> Option<usize> {
    headers.iter().position(|h| h == name)
}

fn parse_finite(row: &csv::StringRecord, idx: usize) -> Option<f64> {
    let raw = row.get(idx)?;
    let v: f64 = raw.trim().parse().ok()?;
    v.is_finite().then_some(v)
}

/// Resolves the entity / effect / significance columns for a ranking pass.
///
/// The candidate order — declared name, then declared aliases, then policy
/// synonyms, first header hit wins, with adjusted-p synonyms preferred over
/// raw ones when the atom declared an adjusted-p column — is the same
/// contract [`super::report_data::summarize_artifact`] resolves under, so a
/// ranking and a count computed over one artifact can never disagree about
/// which column is which. Those resolvers are private to `report_data`;
/// promoting them to `pub(super)` and calling them here would remove this
/// duplicate outright.
///
/// Returns `None` when the entity column does not resolve — without a row
/// identifier there is nothing to name as "top".
pub fn resolve_ranking_columns(
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
) -> Option<RankingColumns> {
    let entity = std::iter::once(&schema.entity_column)
        .chain(schema.entity_column_aliases.iter())
        .chain(synonyms.entity.iter())
        .find_map(|name| resolve_column(headers, name))?;

    let effect = schema
        .signed_effect_column
        .iter()
        .chain(schema.signed_effect_aliases.iter())
        .chain(synonyms.effect.iter())
        .find_map(|name| resolve_column(headers, name));

    let significance = schema
        .significance
        .as_ref()
        .and_then(|sig| resolve_significance_column(headers, sig, synonyms));

    Some(RankingColumns {
        entity,
        effect,
        significance,
    })
}

fn resolve_significance_column(
    headers: &csv::StringRecord,
    sig: &Significance,
    synonyms: &PolicyColumnSynonyms,
) -> Option<usize> {
    if let Some(idx) = resolve_column(headers, &sig.column) {
        return Some(idx);
    }
    // When the atom declared an ADJUSTED-p column, adjusted-p synonyms win:
    // a header carrying both `p_value` and `adj_p_value` must not have the
    // adjusted threshold applied to the raw column.
    if crate::claim_verifier::is_adjusted_pvalue_keyword(&sig.column) {
        let (adjusted, raw): (Vec<&String>, Vec<&String>) = synonyms
            .significance
            .iter()
            .partition(|c| crate::claim_verifier::is_adjusted_pvalue_keyword(c));
        adjusted
            .into_iter()
            .chain(raw)
            .find_map(|name| resolve_column(headers, name))
    } else {
        synonyms
            .significance
            .iter()
            .find_map(|name| resolve_column(headers, name))
    }
}

/// Extracts the ranking candidates from a table under resolved columns.
///
/// A row is a candidate iff it passes the declared significance filter.
/// With no `significance` argument every row is a candidate — the reduced
/// contract for an artifact that declares no significance column.
pub fn candidates(
    rows: &[csv::StringRecord],
    cols: &RankingColumns,
    significance: Option<&Significance>,
) -> Vec<RankedTerm> {
    let mut out = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let sig_value = cols.significance.and_then(|idx| parse_finite(row, idx));
        if let (Some(spec), Some(_)) = (significance, cols.significance) {
            let Some(v) = sig_value else {
                // Unparseable / NA significance: the row was not assessed,
                // so it cannot be a "top" anything.
                continue;
            };
            let passes = match spec.comparator {
                Comparator::Lt => v < spec.threshold,
                Comparator::Gt => v > spec.threshold,
            };
            if !passes {
                continue;
            }
        }
        let entity = row.get(cols.entity).unwrap_or_default().trim().to_string();
        out.push(RankedTerm {
            entity,
            effect: cols.effect.and_then(|idx| parse_finite(row, idx)),
            significance: sig_value,
            row_index,
        });
    }
    out
}

/// Orders `terms` under the module's total order and splits them into
/// sign classes, keeping the first `top_n` of each.
///
/// `directional` says whether the source artifact had a signed-effect
/// column at all: `false` funnels every term into `undirected` regardless
/// of any stray per-row effect value, so an unsigned artifact can never
/// grow a spurious direction split.
///
/// With no significance values to order on (an artifact declaring no
/// significance column), the primary key collapses and |effect|
/// descending becomes the ranking — the only ordering the data supports.
pub fn rank_terms(
    terms: &[RankedTerm],
    order: SignificanceOrder,
    directional: bool,
    top_n: usize,
) -> PathwayRanking {
    let mut sorted: Vec<RankedTerm> = terms.to_vec();
    sorted.sort_by(|a, b| compare(a, b, order));

    let mut ranking = PathwayRanking {
        directional,
        retained_per_class: top_n,
        ..Default::default()
    };
    for term in sorted {
        let class = if directional {
            term.sign_class()
        } else {
            SignClass::Undirected
        };
        let bucket = match class {
            SignClass::Enriched => {
                ranking.eligible_enriched += 1;
                &mut ranking.enriched
            }
            SignClass::Depleted => {
                ranking.eligible_depleted += 1;
                &mut ranking.depleted
            }
            SignClass::Undirected => {
                ranking.eligible_undirected += 1;
                &mut ranking.undirected
            }
        };
        if bucket.len() < top_n {
            bucket.push(term);
        }
    }
    ranking
}

/// The total order: significance (per `order`), then |effect| descending,
/// then entity ascending, then row index ascending. `total_cmp` keeps it a
/// total order with no NaN ambiguity.
fn compare(a: &RankedTerm, b: &RankedTerm, order: SignificanceOrder) -> std::cmp::Ordering {
    let sig = match (a.significance, b.significance) {
        (Some(x), Some(y)) => match order {
            SignificanceOrder::SmallerIsStronger => x.total_cmp(&y),
            SignificanceOrder::LargerIsStronger => y.total_cmp(&x),
        },
        // A row with no significance value ranks after one that has it —
        // never ahead of it by accident of `None` sorting first.
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    };
    sig.then_with(|| {
        let ma = a.effect.map(f64::abs).unwrap_or(0.0);
        let mb = b.effect.map(f64::abs).unwrap_or(0.0);
        mb.total_cmp(&ma)
    })
    .then_with(|| a.entity.cmp(&b.entity))
    .then_with(|| a.row_index.cmp(&b.row_index))
}

/// End-to-end: resolve columns, filter to the significant set, rank, split.
///
/// Returns `None` when the entity column cannot be resolved, or when the
/// schema declares a significance column that is absent from the header —
/// in the latter case nothing was assessed, and an empty ranking would
/// read as "no term qualified", which is a different claim.
pub fn rank_artifact(
    rows: &[csv::StringRecord],
    headers: &csv::StringRecord,
    schema: &ResultSchema,
    synonyms: &PolicyColumnSynonyms,
    top_n: usize,
) -> Option<PathwayRanking> {
    let cols = resolve_ranking_columns(headers, schema, synonyms)?;
    if schema.significance.is_some() && cols.significance.is_none() {
        return None;
    }
    let order = schema
        .significance
        .as_ref()
        .map(|s| SignificanceOrder::from(s.comparator))
        .unwrap_or(SignificanceOrder::SmallerIsStronger);
    let terms = candidates(rows, &cols, schema.significance.as_ref());
    Some(rank_terms(&terms, order, cols.effect.is_some(), top_n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(fields: &[&str]) -> csv::StringRecord {
        csv::StringRecord::from(fields.to_vec())
    }

    /// Header + rows lifted from the deposited fgsea run, trimmed to the
    /// terms that matter for the selection argument. Four rows share
    /// `adj_p_value = 0.0027`, which is the real tie fgsea produces.
    fn fixture() -> (csv::StringRecord, Vec<csv::StringRecord>) {
        let headers = record(&["term", "adj_p_value", "NES"]);
        let rows = vec![
            record(&["HALLMARK_ADIPOGENESIS", "0.0000846", "1.9528"]),
            record(&["GOBP_POS_REG_NERVOUS_SYSTEM_DEV", "0.000419", "-1.8386"]),
            record(&["GOBP_POS_REG_SYNAPSE_ASSEMBLY", "0.00262", "-2.0259"]),
            record(&["GOBP_EPIDERMIS_MORPHOGENESIS", "0.0027", "-2.1502"]),
            record(&["GOBP_REG_NERVOUS_SYSTEM_DEV", "0.0027", "-1.5682"]),
            record(&[
                "REACTOME_DOWNSTREAM_SIGNAL_TRANSDUCTION",
                "0.00212",
                "2.1002",
            ]),
            record(&["GOBP_NOT_SIGNIFICANT", "0.90", "-3.0"]),
        ];
        (headers, rows)
    }

    fn signed_schema() -> ResultSchema {
        ResultSchema {
            artifact: "pathway_results.tsv".into(),
            entity_column: "term".into(),
            entity_column_aliases: vec![],
            significance: Some(Significance {
                column: "adj_p_value".into(),
                threshold: 0.25,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: vec![],
            grouping_column: None,
        }
    }

    #[test]
    fn top_depleted_is_argmin_significance_within_the_negative_class() {
        let (headers, rows) = fixture();
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .expect("ranking computable");
        assert!(ranking.directional);
        // The deposited run claimed GOBP_POS_REG_SYNAPSE_ASSEMBLY, which is
        // neither argmin(padj) nor argmin(NES). The rule picks argmin(padj)
        // within the depleted class.
        assert_eq!(
            ranking.top_depleted().unwrap().entity,
            "GOBP_POS_REG_NERVOUS_SYSTEM_DEV"
        );
        assert_eq!(
            ranking.top_enriched().unwrap().entity,
            "HALLMARK_ADIPOGENESIS"
        );
    }

    #[test]
    fn effect_magnitude_breaks_a_significance_tie() {
        let (headers, rows) = fixture();
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        let depleted: Vec<&str> = ranking.depleted.iter().map(|t| t.entity.as_str()).collect();
        // The two rows tied at 0.0027 order by |NES| descending: 2.1502
        // before 1.5682 — not by the order they appear in the table.
        assert_eq!(
            depleted,
            vec![
                "GOBP_POS_REG_NERVOUS_SYSTEM_DEV",
                "GOBP_POS_REG_SYNAPSE_ASSEMBLY",
                "GOBP_EPIDERMIS_MORPHOGENESIS",
                "GOBP_REG_NERVOUS_SYSTEM_DEV",
            ]
        );
    }

    #[test]
    fn entity_name_breaks_a_full_tie_so_row_order_never_decides() {
        let headers = record(&["term", "adj_p_value", "NES"]);
        let forward = vec![
            record(&["ZETA", "0.01", "-2.0"]),
            record(&["ALPHA", "0.01", "-2.0"]),
        ];
        let reverse: Vec<csv::StringRecord> = forward.iter().rev().cloned().collect();
        let syn = PolicyColumnSynonyms::default();
        let a = rank_artifact(&forward, &headers, &signed_schema(), &syn, 10).unwrap();
        let b = rank_artifact(&reverse, &headers, &signed_schema(), &syn, 10).unwrap();
        assert_eq!(a.top_depleted().unwrap().entity, "ALPHA");
        assert_eq!(
            a.depleted.iter().map(|t| &t.entity).collect::<Vec<_>>(),
            b.depleted.iter().map(|t| &t.entity).collect::<Vec<_>>()
        );
    }

    #[test]
    fn insignificant_rows_are_never_top_however_large_their_effect() {
        let (headers, rows) = fixture();
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        // GOBP_NOT_SIGNIFICANT has the largest |NES| in the table (3.0) and
        // padj 0.90.
        assert!(!ranking
            .depleted
            .iter()
            .any(|t| t.entity == "GOBP_NOT_SIGNIFICANT"));
    }

    #[test]
    fn top_n_truncates_each_sign_class_independently() {
        let (headers, rows) = fixture();
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            2,
        )
        .unwrap();
        assert_eq!(ranking.enriched.len(), 2);
        assert_eq!(ranking.depleted.len(), 2);
        assert_eq!(
            ranking.top_depleted().unwrap().entity,
            "GOBP_POS_REG_NERVOUS_SYSTEM_DEV"
        );
    }

    #[test]
    fn unsigned_modality_yields_one_undirected_list_and_no_split() {
        let headers = record(&["variant_id", "adj_p_value"]);
        let rows = vec![
            record(&["chr1:100:A:T", "0.004"]),
            record(&["chr2:200:G:C", "0.001"]),
            record(&["chr3:300:T:A", "0.60"]),
        ];
        let schema = ResultSchema {
            artifact: "variants.tsv".into(),
            entity_column: "variant_id".into(),
            entity_column_aliases: vec![],
            significance: Some(Significance {
                column: "adj_p_value".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: None,
            signed_effect_aliases: vec![],
            grouping_column: None,
        };
        let ranking = rank_artifact(
            &rows,
            &headers,
            &schema,
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        assert!(!ranking.directional);
        assert!(ranking.enriched.is_empty());
        assert!(ranking.depleted.is_empty());
        assert_eq!(
            ranking
                .undirected
                .iter()
                .map(|t| t.entity.as_str())
                .collect::<Vec<_>>(),
            vec!["chr2:200:G:C", "chr1:100:A:T"]
        );
        assert!(ranking.top_enriched().is_none());
        assert!(ranking.top_depleted().is_none());
    }

    #[test]
    fn zero_effect_row_is_undirected_not_forced_into_a_class() {
        let headers = record(&["term", "adj_p_value", "NES"]);
        let rows = vec![
            record(&["FLAT", "0.001", "0.0"]),
            record(&["UP", "0.002", "1.5"]),
        ];
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        assert_eq!(ranking.enriched.len(), 1);
        assert!(ranking.depleted.is_empty());
        assert_eq!(ranking.undirected.len(), 1);
        assert_eq!(ranking.undirected[0].entity, "FLAT");
    }

    #[test]
    fn na_effect_row_is_undirected() {
        let headers = record(&["term", "adj_p_value", "NES"]);
        let rows = vec![record(&["MISSING", "0.001", "NA"])];
        let ranking = rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        assert_eq!(ranking.undirected.len(), 1);
        assert_eq!(ranking.undirected[0].effect, None);
    }

    #[test]
    fn greater_than_comparator_ranks_larger_scores_first() {
        let headers = record(&["feature", "score", "effect"]);
        let rows = vec![
            record(&["A", "0.95", "1.0"]),
            record(&["B", "0.99", "0.5"]),
            record(&["C", "0.80", "2.0"]),
        ];
        let schema = ResultSchema {
            artifact: "scores.tsv".into(),
            entity_column: "feature".into(),
            entity_column_aliases: vec![],
            significance: Some(Significance {
                column: "score".into(),
                threshold: 0.9,
                comparator: Comparator::Gt,
            }),
            signed_effect_column: Some("effect".into()),
            signed_effect_aliases: vec![],
            grouping_column: None,
        };
        let ranking = rank_artifact(
            &rows,
            &headers,
            &schema,
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        // B (0.99) outranks A (0.95); C (0.80) fails the > 0.9 filter.
        assert_eq!(
            ranking
                .enriched
                .iter()
                .map(|t| t.entity.as_str())
                .collect::<Vec<_>>(),
            vec!["B", "A"]
        );
    }

    #[test]
    fn declared_but_absent_significance_column_is_not_computable() {
        let headers = record(&["term", "NES"]);
        let rows = vec![record(&["A", "1.0"])];
        assert!(
            rank_artifact(
                &rows,
                &headers,
                &signed_schema(),
                &PolicyColumnSynonyms::default(),
                10
            )
            .is_none(),
            "unresolvable significance must yield None, never an empty ranking"
        );
    }

    #[test]
    fn absent_entity_column_is_not_computable() {
        let headers = record(&["adj_p_value", "NES"]);
        let rows = vec![record(&["0.01", "1.0"])];
        assert!(rank_artifact(
            &rows,
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10
        )
        .is_none());
    }

    #[test]
    fn schema_without_significance_ranks_on_effect_magnitude() {
        let headers = record(&["term", "NES"]);
        let rows = vec![
            record(&["SMALL", "1.1"]),
            record(&["BIG", "2.9"]),
            record(&["DOWN", "-2.5"]),
        ];
        let schema = ResultSchema {
            artifact: "t.tsv".into(),
            entity_column: "term".into(),
            entity_column_aliases: vec![],
            significance: None,
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: vec![],
            grouping_column: None,
        };
        let ranking = rank_artifact(
            &rows,
            &headers,
            &schema,
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        assert_eq!(ranking.top_enriched().unwrap().entity, "BIG");
        assert_eq!(ranking.top_depleted().unwrap().entity, "DOWN");
    }

    #[test]
    fn declared_aliases_resolve_before_policy_synonyms() {
        let headers = record(&["pathway", "padj", "nes"]);
        let rows = vec![record(&["P1", "0.001", "-2.0"])];
        let schema = ResultSchema {
            artifact: "t.tsv".into(),
            entity_column: "term".into(),
            entity_column_aliases: vec!["pathway".into()],
            significance: Some(Significance {
                column: "adj_p_value".into(),
                threshold: 0.25,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: vec!["nes".into()],
            grouping_column: None,
        };
        let synonyms = PolicyColumnSynonyms {
            entity: vec!["term".into()],
            significance: vec!["padj".into()],
            effect: vec!["log2FoldChange".into()],
        };
        let ranking = rank_artifact(&rows, &headers, &schema, &synonyms, 10).unwrap();
        assert_eq!(ranking.top_depleted().unwrap().entity, "P1");
    }

    #[test]
    fn adjusted_p_synonym_wins_over_raw_p_synonym() {
        // Header carries BOTH; the declared column is an adjusted-p name, so
        // the threshold must land on the adjusted column. Resolving `p_value`
        // would admit the row (0.001 < 0.25) even though its padj is 0.90.
        let headers = record(&["term", "p_value", "adj_p", "NES"]);
        let rows = vec![record(&["P1", "0.001", "0.90", "-2.0"])];
        let schema = ResultSchema {
            artifact: "t.tsv".into(),
            entity_column: "term".into(),
            entity_column_aliases: vec![],
            significance: Some(Significance {
                column: "adj_p_value".into(),
                threshold: 0.25,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("NES".into()),
            signed_effect_aliases: vec![],
            grouping_column: None,
        };
        let synonyms = PolicyColumnSynonyms {
            entity: vec![],
            significance: vec!["p_value".into(), "adj_p".into()],
            effect: vec![],
        };
        let ranking = rank_artifact(&rows, &headers, &schema, &synonyms, 10).unwrap();
        assert!(
            ranking.depleted.is_empty(),
            "padj 0.90 must not pass < 0.25"
        );
    }

    #[test]
    fn empty_table_ranks_to_an_empty_ranking_not_a_failure() {
        let (headers, _) = fixture();
        let ranking = rank_artifact(
            &[],
            &headers,
            &signed_schema(),
            &PolicyColumnSynonyms::default(),
            10,
        )
        .unwrap();
        assert!(ranking.directional);
        assert!(ranking.enriched.is_empty());
        assert!(ranking.depleted.is_empty());
        assert!(ranking.top_enriched().is_none());
    }

    #[test]
    fn ranking_is_stable_across_repeated_runs() {
        let (headers, rows) = fixture();
        let syn = PolicyColumnSynonyms::default();
        let first = rank_artifact(&rows, &headers, &signed_schema(), &syn, 10).unwrap();
        for _ in 0..5 {
            let again = rank_artifact(&rows, &headers, &signed_schema(), &syn, 10).unwrap();
            assert_eq!(first, again);
        }
    }

    #[test]
    fn rank_terms_honours_the_directional_flag() {
        let terms = vec![
            RankedTerm {
                entity: "A".into(),
                effect: Some(2.0),
                significance: Some(0.01),
                row_index: 0,
            },
            RankedTerm {
                entity: "B".into(),
                effect: Some(-2.0),
                significance: Some(0.02),
                row_index: 1,
            },
        ];
        let split = rank_terms(&terms, SignificanceOrder::SmallerIsStronger, true, 10);
        assert_eq!(split.enriched.len(), 1);
        assert_eq!(split.depleted.len(), 1);
        assert!(split.undirected.is_empty());

        // directional=false funnels everything into `undirected`, so a
        // caller can never read a split off an unsigned artifact.
        let flat = rank_terms(&terms, SignificanceOrder::SmallerIsStronger, false, 10);
        assert!(flat.enriched.is_empty());
        assert!(flat.depleted.is_empty());
        assert_eq!(flat.undirected.len(), 2);
    }

    #[test]
    fn significance_order_derives_from_the_comparator() {
        assert_eq!(
            SignificanceOrder::from(Comparator::Lt),
            SignificanceOrder::SmallerIsStronger
        );
        assert_eq!(
            SignificanceOrder::from(Comparator::Gt),
            SignificanceOrder::LargerIsStronger
        );
    }
}
