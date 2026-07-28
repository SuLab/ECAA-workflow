//! Content-first resolution of the two ENTITY COLUMN ROLES a tabular artifact
//! can carry: a stable ACCESSION and the human-readable LABEL it binds to.
//!
//! An "independent annotation table" is any output table that carries two
//! DISJOINT columns of that shape. Which column plays which role cannot be
//! decided by header name alone — `gene` names the accession in a DESeq2 result
//! table (`gene baseMean … symbol`, where `gene` holds ENSG ids) and the label
//! in a symbol-keyed ranking. Deciding by name and by FILE COLUMN ORDER bound
//! `gene` as the label and then found no accession at all, so a package that
//! shipped the annotation table still reported "no independent annotation
//! table".
//!
//! So: CONTENT decides the accession column, names only break ties and cover
//! tables keyed on a non-Ensembl identifier. This mirrors
//! `lib/literature/matrix.py`, which puts `gene`/`gene_id`/`feature` in
//! `ID_COLUMNS` (the identifier role) and deliberately keeps them OUT of
//! `SYMBOL_COLUMNS` — the two sides agree.
//!
//! The resolver lives in core because two Rust readers need exactly the same
//! answer and had drifted into two INVERTED copies of these lists:
//! `crates/harness/src/literature_validators.rs` (the
//! `gene_symbol_ensembl_consistent` obligation and its DR-10 effect map) and
//! [`crate::claim_extractor`] (the VF-13 `independentGeneAnnotationTable`
//! path). `matrix.py` is the third, Python-side reader; the doc comments below
//! name the constant each list corresponds to so a change lands on both sides.
//!
//! Nothing here is gene-specific except the Ensembl accession SHAPE, which is
//! why a peak / variant / pathway-term table resolves through the same code.

use std::path::Path;

/// The NA-family "no value in this cell" sentinels every tabular producer in
/// the stack writes: R / `org.Hs.eg.db` (`NA`), pandas (`NaN`, `None`),
/// JSON-ish exporters (`null`), and hand-written placeholders (`-`, `.`, `?`).
/// An empty (or all-whitespace) cell is absent too. Case-insensitive, trimmed.
///
/// Shared by the column-role content vote below and by the harness's
/// CSV-lenient deserializers / unresolved-symbol test, so "absent" means the
/// same thing in every column of every literature artifact. A value OUTSIDE
/// this set is a real value: a genuinely malformed cell still fails its
/// column's parse rather than silently reading as absent.
pub fn is_absent_sentinel(s: &str) -> bool {
    let t = s.trim();
    t.is_empty()
        || matches!(
            t.to_ascii_lowercase().as_str(),
            "na" | "n/a" | "nan" | "null" | "none" | "-" | "." | "?"
        )
}

/// Leading data rows buffered to decide a column's ROLE by content. Small
/// enough to stay cheap on a 20k-row counts table, large enough that a leading
/// run of NA-family cells cannot decide the vote on its own.
pub const ENTITY_ROLE_SNIFF_ROWS: usize = 32;

/// Column names that name the ACCESSION (row-identifier) role, MOST SPECIFIC
/// FIRST. The order is a deliberate re-ranking of `matrix.py::ID_COLUMNS`, not
/// a copy of it: this list is only consulted when no column's content is
/// accession-shaped, and in that fallback an unambiguous name must outrank the
/// dual-role `gene`/`feature` (which are legal names in BOTH roles and sit
/// last). Not Ensembl-specific — a peak/variant/term table keyed on
/// `region_id`/`variant_id`/`term` resolves here too.
pub const ACCESSION_COLUMN_CANDIDATES: &[&str] = &[
    "ensembl_gene_id",
    "ensembl_id",
    "ensembl_gene",
    "ensembl",
    "gene_id",
    "feature_id",
    "finding_id",
    "variant_id",
    "region_id",
    "peak_id",
    "term",
    "pathway",
    "id",
    // Dual-role names last: also legal LABEL names in a symbol-keyed table.
    "gene",
    "feature",
];

/// Column names that name the entity-LABEL role, most specific first. The
/// unambiguous head of the list is `matrix.py::SYMBOL_COLUMNS`; the dual-role
/// `gene`/`feature` are appended so a symbol-keyed table still resolves, and
/// are admitted only when their own content is NOT accession-shaped.
pub const LABEL_COLUMN_CANDIDATES: &[&str] = &[
    "symbol",
    "gene_symbol",
    "gene_name",
    "hgnc_symbol",
    "entity",
    "entity_label",
    "label",
    "name",
    // Dual-role names last: also legal ACCESSION names in a result table.
    "gene",
    "feature",
];

/// Column names carrying a signed EFFECT for the row, tried in order. Mirrors
/// `matrix.py::EFFECT_COLUMNS`; unambiguous (no name here plays another role),
/// so a plain first-match in file column order is correct. Feeds the DR-10
/// direction/effect concordance signal.
pub const EFFECT_COLUMN_CANDIDATES: &[&str] = &[
    "log2foldchange",
    "log2fc",
    "log2_fold_change",
    "logfc",
    "lfc",
    "nes",
    "effect",
    "estimate",
    "beta",
    "stat",
    "statistic",
    "score",
    "wald",
    "t",
];

/// Artifacts that can never serve as their own INDEPENDENT annotation source:
/// the claims matrices are the tables under test, and their
/// `finding_id`(accession-shaped) + `entity`(label) columns resolve BOTH roles
/// here, which would make a consistency check compare a matrix against itself —
/// a vacuous Pass, forever.
///
/// The list lives beside the resolver because the hazard is created BY the
/// content-first resolution and every caller that hunts for a truth source
/// faces it identically; ENFORCEMENT stays at each call site, because the
/// resolver is a pure function of `(headers, sample)` and never sees a path,
/// and because "is this table independent of the claim under test?" is a
/// property of the caller's question rather than of the table's columns.
pub const CLAIMS_MATRIX_BASENAMES: &[&str] =
    &["claims_evidence_matrix.csv", "prior_claims_matrix.csv"];

/// Is `path` one of the [`CLAIMS_MATRIX_BASENAMES`], i.e. an artifact that must
/// never be adjudicated against itself? Basename-only and case-insensitive; a
/// non-UTF-8 filename is not a claims matrix.
pub fn is_claims_matrix_artifact(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .map(|n| {
            CLAIMS_MATRIX_BASENAMES
                .iter()
                .any(|b| n.eq_ignore_ascii_case(b))
        })
        .unwrap_or(false)
}

/// Ensembl gene-accession SHAPE: `ENS` + optional species infix + `G` + 11
/// digits, optional `.version` (`ENSG00000103196`, `ENSG00000103196.13`,
/// `ENSMUSG00000017167`). ANCHORED, unlike an extract-from-anywhere
/// normalizer: this decides a whole COLUMN's role, and a composite key
/// (`DE_CRISPLD2_ENSG…`) is a label-bearing identifier, not a bare accession
/// column.
pub fn looks_like_ensembl_accession(s: &str) -> bool {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"(?i)^ENS[A-Z]*G[0-9]{11}(?:\.[0-9]+)?$")
            .expect("static accession-shape regex must compile")
    });
    re.is_match(s.trim())
}

/// Open a delimited table, sniffing the delimiter from its CONTENT and using
/// the extension only as the fallback (`.tsv` → tab, else comma). Shared by
/// every table reader that resolves column roles so one file is never read
/// under two different delimiters.
///
/// Content-first because the writers of these tables do violate the naming
/// convention — R's `write.csv()` to a `*.tsv` path is common in agent-authored
/// scripts — and trusting the extension parses such a file as ONE column named
/// after the whole header line. No column role then resolves, and the table is
/// reported as absent rather than as misnamed. See [`crate::table_delimiter`]
/// for the sniff rule, shared with the re-execution comparator and the
/// report-data assembler.
pub fn open_delimited_table(path: &Path) -> Option<csv::Reader<std::fs::File>> {
    let ext_delimiter = if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        b'\t'
    } else {
        b','
    };
    let delimiter = crate::table_delimiter::sniff_delimiter_from_path(path, ext_delimiter);
    csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(path)
        .ok()
}

/// Buffer the first `n` data rows so column roles can be decided by CONTENT.
/// The reader is left positioned AFTER them, so the caller streams the rest and
/// re-reads the buffered rows from the returned slice — the whole table is
/// still visited exactly once.
pub fn sniff_table_rows<R: std::io::Read>(
    rdr: &mut csv::Reader<R>,
    n: usize,
) -> Vec<csv::StringRecord> {
    rdr.records().take(n).flatten().collect()
}

/// Does column `idx` hold Ensembl accessions? A strict MAJORITY vote over the
/// sniffed rows, ignoring absent cells: a real annotation table carries
/// NA-family holes and unresolved loci, and one hole must not flip a column out
/// of its role.
pub fn column_is_accession_shaped(sample: &[csv::StringRecord], idx: usize) -> bool {
    let mut considered = 0usize;
    let mut matched = 0usize;
    for rec in sample {
        let Some(v) = rec.get(idx).map(str::trim) else {
            continue;
        };
        if is_absent_sentinel(v) {
            continue;
        }
        considered += 1;
        if looks_like_ensembl_accession(v) {
            matched += 1;
        }
    }
    considered > 0 && matched * 2 > considered
}

/// Rank of `header` within a candidate list (lower = more specific), or `None`
/// when the list does not name it.
pub fn candidate_rank(header: &str, candidates: &[&str]) -> Option<usize> {
    candidates
        .iter()
        .position(|c| header.eq_ignore_ascii_case(c))
}

/// The accession column. CONTENT WINS: every column whose sniffed values are
/// accession-shaped is eligible REGARDLESS of its header, so a bespoke
/// `locus`/`gene` header still resolves. Among eligible columns the order is
/// total and deterministic — a header naming the accession role first (by
/// specificity), then a header naming no role, then a header naming the LABEL
/// role, then leftmost.
///
/// With no accession-shaped column the table may still be keyed on a
/// non-Ensembl identifier (peak, variant, pathway term), so degrade to the most
/// specific accession-candidate NAME present. Nothing beyond the accession
/// shape is entity-specific, which is what keeps this modality-agnostic.
pub fn find_accession_column(
    headers: &csv::StringRecord,
    sample: &[csv::StringRecord],
) -> Option<usize> {
    let by_content = headers
        .iter()
        .enumerate()
        .filter(|(i, _)| column_is_accession_shaped(sample, *i))
        .min_by_key(|(i, h)| {
            let (tier, rank) = match (
                candidate_rank(h, ACCESSION_COLUMN_CANDIDATES),
                candidate_rank(h, LABEL_COLUMN_CANDIDATES),
            ) {
                (Some(r), _) => (0usize, r),
                (None, None) => (1, 0),
                (None, Some(_)) => (2, 0),
            };
            (tier, rank, *i)
        })
        .map(|(i, _)| i);
    if by_content.is_some() {
        return by_content;
    }
    headers
        .iter()
        .enumerate()
        .filter_map(|(i, h)| candidate_rank(h, ACCESSION_COLUMN_CANDIDATES).map(|r| (r, i)))
        .min()
        .map(|(_, i)| i)
}

/// The entity-LABEL column: the most specific label-candidate NAME that is
/// neither the accession column nor itself accession-shaped. The content veto
/// is what keeps `gene`/`feature` from binding as a label in a table where they
/// hold accessions, and it applies uniformly — a column full of ENSG ids is
/// never an entity label, whatever it is called.
pub fn find_label_column(
    headers: &csv::StringRecord,
    sample: &[csv::StringRecord],
    accession_idx: usize,
) -> Option<usize> {
    headers
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != accession_idx && !column_is_accession_shaped(sample, *i))
        .filter_map(|(i, h)| candidate_rank(h, LABEL_COLUMN_CANDIDATES).map(|r| (r, i)))
        .min()
        .map(|(_, i)| i)
}

/// The two columns an independent annotation table must carry. The roles are
/// DISJOINT by construction (`label_idx != accession_idx`), so one column can
/// never satisfy both and produce a self-referential map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityColumnRoles {
    /// Column holding the human-readable entity label (gene symbol, peak name, …).
    pub label_idx: usize,
    /// Column holding the stable accession that label is asserted to bind to.
    pub accession_idx: usize,
}

/// Resolve both roles over a table's headers plus a content sample. `None` when
/// either role is absent — a table carrying only one of them is honestly "not
/// an annotation table", which the caller reports rather than papering over.
pub fn resolve_entity_column_roles(
    headers: &csv::StringRecord,
    sample: &[csv::StringRecord],
) -> Option<EntityColumnRoles> {
    let accession_idx = find_accession_column(headers, sample)?;
    let label_idx = find_label_column(headers, sample, accession_idx)?;
    Some(EntityColumnRoles {
        label_idx,
        accession_idx,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header line plus data lines, tab-split, into the `(headers, sample)`
    /// argument pair every resolver entry point takes. Literal table lines so a
    /// fixture header reads exactly as the producer wrote it.
    fn table(header: &str, rows: &[&str]) -> (csv::StringRecord, Vec<csv::StringRecord>) {
        let split = |line: &str| csv::StringRecord::from(line.split('\t').collect::<Vec<_>>());
        (split(header), rows.iter().map(|r| split(r)).collect())
    }

    #[test]
    fn accession_shape_is_anchored_and_species_agnostic() {
        assert!(
            looks_like_ensembl_accession("ENSG00000103196"),
            "bare human gene accession"
        );
        assert!(
            looks_like_ensembl_accession("ENSG00000103196.13"),
            "versioned accession"
        );
        assert!(
            looks_like_ensembl_accession("ENSMUSG00000017167"),
            "species infix"
        );
        assert!(
            looks_like_ensembl_accession(" ENSG00000103196 "),
            "surrounding whitespace is trimmed"
        );
        // A composite key is a label-bearing identifier, not a bare accession
        // column, and a symbol is neither.
        assert!(
            !looks_like_ensembl_accession("DE_CRISPLD2_ENSG00000103196"),
            "composite finding id is not a bare accession"
        );
        assert!(
            !looks_like_ensembl_accession("CRISPLD2"),
            "a symbol is not an accession"
        );
        assert!(
            !looks_like_ensembl_accession("ENST00000367714"),
            "a transcript accession is not a gene accession"
        );
    }

    #[test]
    fn content_wins_over_the_header_name() {
        // The defect this module exists to prevent: `gene` holds the ENSG ids
        // and `symbol` holds the labels, so name+column-order resolution bound
        // them backwards and found no accession column at all.
        let (h, rows) = table(
            "gene\tbaseMean\tlog2FoldChange\tstat\tpadj\tsymbol",
            &[
                "ENSG00000103196\t330.3\t4.57\t16.7\t1e-9\tCRISPLD2",
                "ENSG00000152583\t997.4\t3.29\t12.1\t1e-7\tSPARCL1",
            ],
        );
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            Some(EntityColumnRoles {
                label_idx: 5,
                accession_idx: 0,
            }),
            "content must bind `gene` as the accession and `symbol` as the label"
        );
    }

    #[test]
    fn conventional_header_order_is_unaffected() {
        let (h, rows) = table(
            "symbol\tgene_id\tstat",
            &["CRISPLD2\tENSG00000103196\t16.7"],
        );
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            Some(EntityColumnRoles {
                label_idx: 0,
                accession_idx: 1,
            }),
            "symbol+gene_id is the conventional shape"
        );
    }

    #[test]
    fn no_accession_shaped_column_degrades_to_the_name_ranking() {
        // Entrez ids: nothing is accession-shaped, so the unambiguous `gene_id`
        // must take the accession role from the dual-role `gene`, which keeps
        // the label role because its own content is not accession-shaped.
        let (h, rows) = table("gene\tgene_id", &["CRISPLD2\t83716", "SPARCL1\t8404"]);
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            Some(EntityColumnRoles {
                label_idx: 0,
                accession_idx: 1,
            }),
            "name fallback must prefer `gene_id` over the dual-role `gene`"
        );
    }

    #[test]
    fn a_single_role_table_resolves_neither() {
        let (label_only, rows) = table("symbol\tstat", &["CRISPLD2\t16.7"]);
        assert_eq!(
            resolve_entity_column_roles(&label_only, &rows),
            None,
            "a label-only ranking is not an annotation table"
        );
        let (accession_only, rows) = table("gene_id\tvariance", &["ENSG00000129824\t5.74"]);
        assert_eq!(
            resolve_entity_column_roles(&accession_only, &rows),
            None,
            "an accession-only list is not an annotation table"
        );
    }

    #[test]
    fn an_accession_shaped_column_is_never_the_label() {
        // Both columns hold accessions under label-ish names: the content veto
        // must refuse a label rather than manufacture a self-referential pair.
        let (h, rows) = table(
            "gene\tsymbol",
            &[
                "ENSG00000103196\tENSG00000103196",
                "ENSG00000152583\tENSG00000152583",
            ],
        );
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            None,
            "a column of ENSG ids is never an entity label"
        );
    }

    #[test]
    fn absent_cells_do_not_flip_a_column_out_of_its_role() {
        // A real annotation table carries NA-family holes; the majority vote
        // ignores them instead of demoting the column.
        let (h, rows) = table(
            "locus\tsymbol",
            &[
                "ENSG00000103196\tCRISPLD2",
                "NA\tNA",
                "\t",
                "ENSG00000152583\tSPARCL1",
            ],
        );
        assert_eq!(
            find_accession_column(&h, &rows),
            Some(0),
            "a bespoke header still resolves on content, holes and all"
        );
        assert!(
            !column_is_accession_shaped(&rows, 1),
            "the symbol column stays a label"
        );
    }

    #[test]
    fn a_column_of_only_absent_cells_holds_no_role() {
        let (_h, rows) = table("gene_id", &["NA", "", "n/a"]);
        assert!(
            !column_is_accession_shaped(&rows, 0),
            "no considered value ⇒ no content verdict"
        );
    }

    #[test]
    fn non_gene_entities_resolve_through_the_same_path() {
        let (h, rows) = table("region_id\tlabel", &["PEAK_1\tchr1:1000-2000"]);
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            Some(EntityColumnRoles {
                label_idx: 1,
                accession_idx: 0,
            }),
            "nothing outside the accession shape is gene-specific"
        );
    }

    #[test]
    fn candidate_rank_is_case_insensitive_and_specificity_ordered() {
        assert_eq!(
            candidate_rank("Gene_ID", ACCESSION_COLUMN_CANDIDATES),
            Some(4),
            "header matching ignores case"
        );
        assert!(
            candidate_rank("gene", ACCESSION_COLUMN_CANDIDATES)
                > candidate_rank("gene_id", ACCESSION_COLUMN_CANDIDATES),
            "the dual-role name must rank after the unambiguous one"
        );
        assert!(
            candidate_rank("gene", LABEL_COLUMN_CANDIDATES)
                > candidate_rank("symbol", LABEL_COLUMN_CANDIDATES),
            "same ordering on the label side"
        );
        assert_eq!(
            candidate_rank("baseMean", ACCESSION_COLUMN_CANDIDATES),
            None,
            "an unrelated column names no role"
        );
    }

    #[test]
    fn the_claims_matrices_are_recognized_by_basename() {
        assert!(is_claims_matrix_artifact(Path::new(
            "runtime/outputs/x/claims_evidence_matrix.csv"
        )));
        assert!(is_claims_matrix_artifact(Path::new(
            "Prior_Claims_Matrix.CSV"
        )));
        assert!(
            !is_claims_matrix_artifact(Path::new("annotation/symbol_map.tsv")),
            "a real annotation table is admissible"
        );
    }

    #[test]
    fn the_claims_matrix_shape_would_otherwise_resolve_both_roles() {
        // Why the exclusion above exists: `finding_id` is accession-shaped and
        // `entity` is a label, so the resolver happily answers on the artifact
        // UNDER TEST. Only the call-site basename guard stops the vacuous pass.
        let (h, rows) = table(
            "finding_id\tentity\tentity_kind",
            &["ENSG00000197142\tCRISPLD2\tgene"],
        );
        assert_eq!(
            resolve_entity_column_roles(&h, &rows),
            Some(EntityColumnRoles {
                label_idx: 1,
                accession_idx: 0,
            }),
            "the resolver cannot self-police: it never sees a path"
        );
    }

    #[test]
    fn table_open_and_sniff_visit_each_row_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotation.tsv");
        std::fs::write(
            &path,
            "symbol\tensembl_gene_id\nCRISPLD2\tENSG00000103196\nSPARCL1\tENSG00000152583\n",
        )
        .unwrap();
        let mut rdr = open_delimited_table(&path).expect("readable table");
        let headers = rdr.headers().expect("header row").clone();
        // Sniff ONE row, then stream the rest: chaining the buffered row back on
        // must yield the whole table exactly once.
        let sample = sniff_table_rows(&mut rdr, 1);
        assert_eq!(sample.len(), 1, "one row buffered");
        let all: Vec<csv::StringRecord> = sample
            .iter()
            .cloned()
            .chain(rdr.records().flatten())
            .collect();
        assert_eq!(all.len(), 2, "no row lost and none duplicated");
        assert_eq!(
            resolve_entity_column_roles(&headers, &sample),
            Some(EntityColumnRoles {
                label_idx: 0,
                accession_idx: 1,
            }),
            "roles resolve from the sniffed sample, not from a second read"
        );
    }
}
