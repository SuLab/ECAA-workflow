//! Phase C of the literature-atom plan — runner implementations for the five
//! literature validator obligations registered in
//! `ecaa_workflow_core::validation_obligations::literature_obligations`.
//!
//! Runners are pure functions over `(artifact_path, evidence_manifest_path)`
//! that return Ok(()) on success or Err(ValidationFailureCause::LiteratureClaim)
//! on failure. The harness post-task validator dispatcher calls them in
//! sequence; the first failure transitions the task to
//! BlockerKind::ValidationFailed with the structured cause attached.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use ecaa_workflow_core::blocker::{LiteratureClaimFailureKind, ValidationFailureCause};
// Entity-column ROLE resolution (accession ↔ label) and the shared absent-cell
// predicate live in core: this file and `core::claim_extractor` must resolve the
// same roles from the same candidate lists, and when each carried its own copy
// they drifted into two mutually INVERTED lists.
use ecaa_workflow_core::entity_columns::{
    find_accession_column, is_absent_sentinel, is_claims_matrix_artifact, open_delimited_table,
    resolve_entity_column_roles, sniff_table_rows, EFFECT_COLUMN_CANDIDATES,
    ENTITY_ROLE_SNIFF_ROWS,
};
use serde::{Deserialize, Serialize};

/// CSV-lenient `u64`: the `method_landscape.csv` shape emits an EMPTY
/// `evidence_quote_offset` on `curated_baseline` candidate rows (which carry no
/// evidence), and per-row NA-family sentinels elsewhere. A bare `u64` field
/// rejects both and fails the WHOLE `load_rows` parse, which the offset-reading
/// validators then mis-report as a spurious table-wide failure (stranding the
/// keystone `survey_method_landscape` task and every downstream stage). Treat
/// every absent-value sentinel as 0.
fn de_u64_lenient<'de, D>(d: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    parse_u64_lenient(&s).ok_or_else(|| {
        serde::de::Error::custom(format!("invalid unsigned integer: {:?}", s.trim()))
    })
}

/// The accept-set behind [`de_u64_lenient`], factored out so the parse-failure
/// diagnostic ([`locate_unparseable_column`]) tests cells with exactly the same
/// rule the deserializer applies — the two cannot drift apart.
fn parse_u64_lenient(s: &str) -> Option<u64> {
    let t = s.trim();
    if is_absent_sentinel(t) {
        return Some(0);
    }
    t.parse::<u64>().ok()
}

/// The accept-set behind [`de_bool_lenient`]; see [`parse_u64_lenient`].
fn parse_bool_lenient(s: &str) -> Option<bool> {
    let t = s.trim();
    if is_absent_sentinel(t) {
        return Some(false);
    }
    match t {
        "true" | "True" | "TRUE" | "1" => Some(true),
        "false" | "False" | "FALSE" | "0" => Some(false),
        _ => None,
    }
}

/// CSV-lenient `bool`: `curated_baseline` rows emit an EMPTY `redistributable`
/// (and may emit an empty `verified`), and a producer that assessed nothing for
/// a row writes the NA-family sentinel its language spells absence with — a
/// contextualize step wrote `redistributable=NA` on 4029 `not_assessed` rows. A
/// bare `bool` rejects both, ONE such cell fails the whole CSV parse, and every
/// load_rows-based obligation then reported a row-0 failure against a table that
/// was fine. Absent (empty or NA-family, see [`is_absent_sentinel`]) reads as
/// `false` — not marked redistributable / not verified, the conservative
/// reading; the usual true/false tokens are accepted; anything else is still an
/// error.
fn de_bool_lenient<'de, D>(d: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let s = String::deserialize(d)?;
    parse_bool_lenient(&s)
        .ok_or_else(|| serde::de::Error::custom(format!("invalid bool literal: {:?}", s.trim())))
}

/// Canonical normalization applied to source text before substring-match.
/// Pinned by name in `evidence/manifest.json::extracted_text_normalization`.
pub fn collapse_whitespace_lowercase_v1(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(ch.to_ascii_lowercase());
            prev_ws = false;
        }
    }
    out.trim().to_string()
}

fn decode_xml_character_references(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|digits| u32::from_str_radix(digits, 16).ok())
                .and_then(char::from_u32)
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|digits| digits.parse::<u32>().ok())
                        .and_then(char::from_u32)
                }),
        };
        if let Some(ch) = decoded {
            out.push(ch);
        } else {
            out.push_str(&rest[..=end]);
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn xml_visible_text(s: &str, include_abstract_labels: bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut tag = String::new();
    let mut in_tag = false;
    let mut chars = s.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_tag {
            tag.push(ch);
            if ch == '>' {
                if include_abstract_labels {
                    if let Some(label) = abstract_text_label(&tag) {
                        out.push(' ');
                        out.push_str(&label);
                        out.push(':');
                    }
                    if is_closing_title_tag(&tag) {
                        out.push(':');
                    }
                }
                out.push(' ');
                tag.clear();
                in_tag = false;
            }
            continue;
        }
        if ch == '<'
            && chars
                .peek()
                .is_some_and(|next| next.is_ascii_alphabetic() || matches!(next, '/' | '!' | '?'))
        {
            tag.push(ch);
            in_tag = true;
        } else {
            out.push(ch);
        }
    }
    if in_tag {
        out.push_str(&tag);
    }
    decode_xml_character_references(&out)
}

fn is_closing_title_tag(tag: &str) -> bool {
    tag.strip_prefix('<')
        .and_then(|body| body.strip_suffix('>'))
        .is_some_and(|body| body.trim().eq_ignore_ascii_case("/title"))
}

fn abstract_text_label(tag: &str) -> Option<String> {
    let body = tag
        .strip_prefix('<')?
        .trim_start()
        .strip_suffix('>')?
        .trim_end();
    if body.starts_with('/') || body.starts_with('!') || body.starts_with('?') {
        return None;
    }
    let name_end = body
        .find(|ch: char| ch.is_ascii_whitespace() || ch == '/')
        .unwrap_or(body.len());
    if !body[..name_end].eq_ignore_ascii_case("AbstractText") {
        return None;
    }

    let attrs = &body[name_end..];
    let lower = attrs.to_ascii_lowercase();
    let mut offset = 0usize;
    while let Some(found) = lower[offset..].find("label") {
        let start = offset + found;
        let before_ok = start == 0
            || lower.as_bytes()[start - 1].is_ascii_whitespace()
            || lower.as_bytes()[start - 1] == b'/';
        let after_name = start + "label".len();
        let after_ok = after_name == lower.len()
            || lower.as_bytes()[after_name].is_ascii_whitespace()
            || lower.as_bytes()[after_name] == b'=';
        if before_ok && after_ok {
            let mut rest = &attrs[after_name..];
            rest = rest.trim_start();
            rest = rest.strip_prefix('=')?.trim_start();
            let quote = rest.chars().next()?;
            if quote != '"' && quote != '\'' {
                return None;
            }
            let value = &rest[quote.len_utf8()..];
            let end = value.find(quote)?;
            let decoded = decode_xml_character_references(&value[..end]);
            let decoded = decoded.trim();
            return (!decoded.is_empty()).then(|| decoded.to_string());
        }
        offset = after_name;
    }
    None
}

fn quote_matches_snapshot(raw: &str, quote: &str) -> bool {
    let normalized_source = collapse_whitespace_lowercase_v1(raw);
    let normalized_quote = collapse_whitespace_lowercase_v1(quote);
    if normalized_quote.is_empty() {
        return false;
    }
    if normalized_source.contains(&normalized_quote) {
        return true;
    }

    let xml_quote = collapse_whitespace_lowercase_v1(&xml_visible_text(quote, true));
    let labelled_source = collapse_whitespace_lowercase_v1(&xml_visible_text(raw, true));
    if !xml_quote.is_empty() && labelled_source.contains(&xml_quote) {
        return true;
    }

    let plain_source = collapse_whitespace_lowercase_v1(&xml_visible_text(raw, false));
    let plain_quote = collapse_whitespace_lowercase_v1(&xml_visible_text(quote, false));
    !plain_quote.is_empty() && plain_source.contains(&plain_quote)
}

// Serde deserialization target for `claims_matrix.csv`; many fields are read
// only via reflection-style validators below and are flagged as dead by the
// compiler. Preserve the full shape so the deserializer fails on schema drift.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct ClaimsMatrixRow {
    #[serde(default)]
    pub finding_id: Option<String>,
    // Optional so the same reader parses BOTH the claims-matrix shape
    // (entity/entity_kind/pmid) AND the method_landscape shape
    // (axis/candidate_method/source_ref) survey_method_landscape emits.
    // Required `String` here made every load_rows-based obligation
    // (evidence_quote_substring_match, redistributable_or_marked, …) fail to
    // parse the method_landscape CSV and report a spurious
    // EvidenceArtifactMissing at row 0.
    // `entity_id` is the contextualize atom's spelling of the entity key
    // (gene/peak/variant id); accept it as an alias so the finding_id fallback
    // can match the row's entity against an upstream PK.
    #[serde(default, alias = "entity_id")]
    pub entity: String,
    #[serde(default)]
    pub entity_kind: String,
    #[serde(default)]
    pub pmid: Option<String>,
    // Raw cell, NOT Vec<String>: the csv crate cannot deserialize a delimited
    // single cell (e.g. "20921232|22279750|...") into a Vec struct-field — a Vec
    // field greedily consumes the rest of the record and derails the row ("expected
    // field, got end of row"). Storing the raw string keeps the parse robust; use
    // `prior_pmid_list()` to split it (pipe / comma / semicolon / whitespace / JSON).
    #[serde(default)]
    pub prior_pmids: Option<String>,
    // Typed-locator columns. Absent on legacy PMID-only rows (which
    // anchor via `pmid`/`prior_pmids`); present on locator-generalized
    // rows where `source_ref_kind` selects the dispatch branch.
    #[serde(default)]
    pub source_ref_kind: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub source_class: Option<String>,
    #[serde(default)]
    pub evidence_role: Option<String>,
    #[serde(default)]
    pub version_context: Option<String>,
    // agents spell the concordance column `concordance` (claude) or
    // `concordance_flag` (canonical); accept both.
    #[serde(default, alias = "concordance")]
    pub concordance_flag: Option<String>,
    // The LAST hard-required field, and the recurring parse-breaker: a single
    // renamed/absent column here failed the WHOLE CSV parse, bailing every
    // obligation at row 0 (codex/claude each picked a different spelling —
    // `evidence_quote_excerpt`, etc.). Default + alias so ClaimsMatrixRow now has
    // NO hard-required field: the CSV always parses and each obligation evaluates
    // on what's present (a no_prior_finding row legitimately has an empty quote).
    #[serde(default, alias = "evidence_quote_excerpt", alias = "quote")]
    pub evidence_quote: String,
    // `curated_baseline` method_landscape rows emit an empty offset/redistributable/
    // verified; lenient deserializers map "" to 0/false so a no-evidence candidate
    // row does not fail the whole CSV parse (see de_u64_lenient / de_bool_lenient).
    // codex names the start offset `quote_start`; defaulted so a CSV that omits
    // it (offset-free containment match) still parses.
    #[serde(default, deserialize_with = "de_u64_lenient", alias = "quote_start")]
    pub evidence_quote_offset: u64,
    // codex spells these `source_type` / `source_sha256`; default+alias so its
    // richer claims-matrix schema parses (the obligations key on the values, not
    // the column name).
    #[serde(default, alias = "source_type")]
    pub source_kind: String,
    #[serde(default, alias = "source_sha256")]
    pub source_hash: String,
    #[serde(default)]
    pub retrieval_ts: String,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub redistributable: bool,
    #[serde(default, deserialize_with = "de_bool_lenient")]
    pub verified: bool,
}

impl ClaimsMatrixRow {
    /// Split the raw `prior_pmids` cell into a PMID list, tolerating every
    /// delimiter agents use: a JSON array (`["a","b"]`), or pipe / comma /
    /// semicolon / whitespace separation. Empty/absent → empty list.
    fn prior_pmid_list(&self) -> Vec<String> {
        let raw = match &self.prior_pmids {
            Some(s) if !s.trim().is_empty() && s.trim() != "[]" => s.trim(),
            _ => return Vec::new(),
        };
        if let Ok(v) = serde_json::from_str::<Vec<String>>(raw) {
            return v
                .into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
        }
        raw.split(['|', ',', ';', ' ', '\t', '\n'])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// A row that does NOT assert a concordance direction: `no_prior_finding` (prior
/// work searched, none found for the entity), `not_assessed` (retrieval not
/// performed — entity outside the searched set), or `unverifiable` (prior work
/// found but its direction is not determinable). The evidence-backing obligations
/// (`pmid_resolves` / `evidence_quote_substring_match` / `redistributable_or_marked`)
/// exist to substantiate an ASSERTED concordance (`same_direction` /
/// `opposite_direction`), so they skip non-asserting rows — a row that makes no
/// concordance claim cannot have an unbacked one. `concordance_flag_in_closed_set`
/// still validates that the flag itself is in the closed vocabulary.
fn row_makes_no_concordance_claim(row: &ClaimsMatrixRow) -> bool {
    matches!(
        row.concordance_flag.as_deref(),
        Some("no_prior_finding") | Some("not_assessed") | Some("unverifiable")
    )
}

// Serde shape mirror of `evidence-manifest.json`; `schema_version` is read by
// load_manifest's downstream validators on schema drift but the wrapper struct
// itself does not consume every field directly.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceManifest {
    // Unused by the validators; tolerate absent or non-numeric (codex writes a
    // string) by ignoring the value entirely.
    #[serde(default)]
    #[allow(dead_code)]
    pub schema_version: serde_json::Value,
    // Accept `sources` (codex's hand-rolled top-level key) as an alias for the
    // canonical `entries`. Default to empty: a summary manifest with no per-source
    // entries is legitimate — contextualize reuses the upstream review_prior_work
    // claims (no new fetch), and a run with no ASSERTED concordance rows has no
    // evidence to manifest. An absent `entries` must not fail load_manifest; the
    // per-row gates already skip non-asserting rows.
    #[serde(default, alias = "sources")]
    pub entries: Vec<EvidenceEntry>,
}

// Serde shape for entries in `evidence-manifest.json`; preserves the full
// per-PMID record so validators downstream can inspect license/redistributable
// flags even when this binary doesn't read them at compile time.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct EvidenceEntry {
    // Optional so non-PMID locator entries (DOI/arXiv/URL) validate;
    // legacy PMID entries continue to carry it.
    #[serde(default)]
    pub pmid: Option<String>,
    #[serde(default)]
    pub source_ref_kind: Option<String>,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub source_class: Option<String>,
    #[serde(default)]
    pub evidence_role: Option<String>,
    #[serde(default)]
    pub version_context: Option<String>,
    /// Batched PubMed efetch: the PMIDs a single shared XML snapshot covers
    /// (one efetch request fetches many PMIDs). Locator indexing keys on each
    /// so a claim row citing any batch member resolves to this snapshot.
    /// codex spells this `pmids`; accept both.
    #[serde(default, alias = "pmids")]
    pub pmids_in_batch: Vec<String>,
    // codex names this `source_type`; the canonical helper uses `source_kind`.
    // Defaulted + aliased so codex's hand-rolled manifest deserializes (the
    // per-row legal gate reads the CSV's source_kind, not this entry field).
    #[serde(default, alias = "source_type")]
    pub source_kind: String,
    // codex names the snapshot path `source_text_path`; the canonical helper
    // uses `path`. Accept both. Defaulted: a manifest entry can legitimately omit
    // a snapshot path (search-only / no_prior_finding summary entries that cite a
    // PMID but downloaded no source text). Without the default, ONE path-less entry
    // failed the whole manifest parse and bailed every manifest-based obligation
    // (pmid_resolves / source_resolves / evidence_quote / redistributable) at row 0
    // with a spurious EvidenceArtifactMissing — the same class the all-defaulted
    // ClaimsMatrixRow fields above were hardened against.
    #[serde(default, alias = "source_text_path")]
    pub path: String,
    // Secondary provenance metadata the validators store but do not gate on.
    // Defaulted so a leaner hand-rolled manifest (codex omits these / spells
    // sha256 without the `_binary` suffix) still deserializes and resolves.
    #[serde(default, alias = "sha256")]
    pub sha256_binary: String,
    #[serde(default)]
    pub sha256_extracted_text: String,
    #[serde(default)]
    pub extracted_text_normalization: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub retrieval_ts: String,
    #[serde(default)]
    pub retrieval_query_id: String,
    #[serde(default)]
    pub redistributable: bool,
    #[serde(default)]
    pub license: String,
}

/// Structured detail of a claims-table deserialization failure: WHICH data row
/// and WHICH column the csv reader choked on, plus the reader's own message.
///
/// Kept structured (rather than the flattened `String` this used to be) so the
/// runner-dispatch layer can report an honest `table_parse_error` instead of the
/// row-0 `evidence_artifact_missing` fallback every runner used to emit. A table
/// that will not parse is not a missing evidence artifact, and reporting it as
/// one sent SMEs hunting for files that were never absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableParseFailure {
    /// 0-based index of the DATA row that failed to deserialize (header
    /// excluded), matching the `row_index` every literature cause reports.
    pub row_index: u64,
    /// Header name of the offending column, when the reader identified one.
    pub column: Option<String>,
    /// The csv reader's own message (carries the line / byte position).
    pub detail: String,
}

/// Harness-local widening of the core `ValidationFailureCause` taxonomy.
///
/// `ValidationFailureCause` (`crates/ecaa-types/src/blocker.rs`) is a closed,
/// wire-facing enum this crate must not extend, and its
/// `LiteratureClaimFailureKind` has no member for "the artifact table itself did
/// not parse". So every literature runner reported an unparseable table as
/// `evidence_artifact_missing` at row 0: a real deposit surfaced six REQUIRED
/// contextualization obligations that way, all six caused by a single
/// unparseable cell, none of them missing any evidence file.
///
/// This mirror carries the same `cause_kind` tag and the same `LiteratureClaim`
/// payload (so the rendered message is unchanged for claim failures) and adds
/// the honest [`Self::TableParseError`]. When the core enum gains a matching
/// variant this type collapses into a `From` conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cause_kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum LiteratureFailureCause {
    /// Mirror of `ValidationFailureCause::LiteratureClaim` — an obligation
    /// failed on a specific row of a literature artifact.
    LiteratureClaim {
        row_index: u64,
        artifact: String,
        kind: LiteratureClaimFailureKind,
    },
    /// The artifact table could not be deserialized, so NO obligation could be
    /// evaluated over it. Names the offending row and column so the producer can
    /// fix the cell instead of auditing evidence files.
    TableParseError {
        row_index: u64,
        artifact: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        column: Option<String>,
        detail: String,
    },
}

impl LiteratureFailureCause {
    /// The row the cause is anchored to, for the `row N: <cause>` message prefix.
    fn row_index(&self) -> u64 {
        match self {
            Self::LiteratureClaim { row_index, .. } | Self::TableParseError { row_index, .. } => {
                *row_index
            }
        }
    }
}

impl From<ValidationFailureCause> for LiteratureFailureCause {
    fn from(cause: ValidationFailureCause) -> Self {
        match cause {
            ValidationFailureCause::LiteratureClaim {
                row_index,
                artifact,
                kind,
            } => Self::LiteratureClaim {
                row_index,
                artifact,
                kind,
            },
        }
    }
}

impl TableParseFailure {
    /// Render as the honest [`LiteratureFailureCause::TableParseError`].
    fn into_cause(self, artifact: &str) -> LiteratureFailureCause {
        LiteratureFailureCause::TableParseError {
            row_index: self.row_index,
            artifact: artifact.to_string(),
            column: self.column,
            detail: self.detail,
        }
    }
}

/// The artifact name a failure cause reports: the CSV's file name, falling back
/// to the full path when it has none.
fn artifact_name(csv_path: &Path) -> String {
    match csv_path.file_name() {
        Some(f) => f.to_string_lossy().to_string(),
        None => csv_path.to_string_lossy().to_string(),
    }
}

/// The `ClaimsMatrixRow` columns that carry a TYPED (non-string) value, each
/// paired with the predicate that accepts a cell. Every other column
/// deserializes as `String` and cannot fail, so a claims table that does not
/// parse broke on one of these — which is what lets
/// [`locate_unparseable_column`] name the column when the csv reader cannot.
/// Aliases are listed alongside their canonical spelling because the producer
/// may have written either.
type TypedColumnPredicate = fn(&str) -> bool;

const TYPED_COLUMNS: &[(&str, TypedColumnPredicate)] = &[
    ("evidence_quote_offset", cell_parses_as_u64),
    ("quote_start", cell_parses_as_u64),
    ("redistributable", cell_parses_as_bool),
    ("verified", cell_parses_as_bool),
];

fn cell_parses_as_u64(cell: &str) -> bool {
    parse_u64_lenient(cell).is_some()
}

fn cell_parses_as_bool(cell: &str) -> bool {
    parse_bool_lenient(cell).is_some()
}

/// Name the column that broke a row.
///
/// The csv reader reports a field INDEX only for errors it raises itself; an
/// error raised inside a `deserialize_with` parser — which is how every lenient
/// column parser here rejects a malformed cell — carries none (csv's
/// `serde::de::Error::custom` sets `field: None`). So re-read the offending
/// record and name the first typed column whose cell no lenient parser accepts.
/// Runs only on the failure path.
fn locate_unparseable_column(
    csv_path: &Path,
    headers: &[String],
    row_index: u64,
) -> Option<String> {
    let mut rdr = csv::Reader::from_path(csv_path).ok()?;
    let record = rdr.records().nth(row_index as usize)?.ok()?;
    for (name, accepts) in TYPED_COLUMNS {
        let Some(i) = headers.iter().position(|h| h.trim() == *name) else {
            continue;
        };
        if let Some(cell) = record.get(i) {
            if !accepts(cell) {
                return Some((*name).to_string());
            }
        }
    }
    None
}

/// Translate a `csv::Error` into a [`TableParseFailure`], naming the DATA row
/// and the offending column.
fn table_parse_failure(csv_path: &Path, e: &csv::Error, headers: &[String]) -> TableParseFailure {
    // `Position::record` counts the header as record 0, so the first data row
    // is record 1; report the 0-based data-row index the causes use elsewhere.
    let data_row = |pos: &Option<csv::Position>| -> u64 {
        match pos {
            Some(p) => p.record().saturating_sub(1),
            None => 0,
        }
    };
    let detail = e.to_string();
    match e.kind() {
        csv::ErrorKind::Deserialize { pos, err, .. } => {
            let row_index = data_row(pos);
            let column = match err.field().and_then(|i| headers.get(i as usize)) {
                Some(name) => Some(name.clone()),
                None => locate_unparseable_column(csv_path, headers, row_index),
            };
            TableParseFailure {
                row_index,
                column,
                detail,
            }
        }
        csv::ErrorKind::UnequalLengths { pos, .. } => TableParseFailure {
            row_index: data_row(pos),
            column: None,
            detail,
        },
        _ => TableParseFailure {
            row_index: 0,
            column: None,
            detail,
        },
    }
}

/// Deserialize the whole claims table.
///
/// Deliberately NOT per-row tolerant: one malformed row still fails the whole
/// table, because a half-parsed literature matrix would silently drop claim rows
/// from every gate that reads it. The fix for the row-0 false positives is to
/// report the failure HONESTLY (which row, which column — see
/// [`table_parse_failure`] and [`probe_claims_table`]) and to stop rejecting
/// legitimate absent-value cells (see [`is_absent_sentinel`]), not to swallow
/// malformed ones.
fn load_rows(csv_path: &Path) -> Result<Vec<ClaimsMatrixRow>, TableParseFailure> {
    let mut rdr = csv::Reader::from_path(csv_path).map_err(|e| TableParseFailure {
        row_index: 0,
        column: None,
        detail: e.to_string(),
    })?;
    // Snapshot the header row so a field INDEX in the reader's error can be
    // reported as a column NAME. Cloned because `deserialize()` needs `&mut`.
    let headers: Vec<String> = match rdr.headers() {
        Ok(h) => h.iter().map(|s| s.to_string()).collect(),
        // A header-read failure re-surfaces on the deserialize below; carry an
        // empty header list so the column name is simply absent.
        Err(_) => Vec::new(),
    };
    rdr.deserialize()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| table_parse_failure(csv_path, &e, &headers))
}

/// Parse-probe the claims table an obligation's runner reads through
/// [`load_rows`], returning the honest
/// [`LiteratureFailureCause::TableParseError`] when it does not deserialize.
///
/// Called by the runner-dispatch layer ahead of the runners that read the table
/// this way, so the SME-visible validator message names the offending row and
/// column instead of the row-0 `evidence_artifact_missing` those runners must
/// still return through the closed core cause type.
pub fn probe_claims_table(csv_path: &Path) -> Result<(), LiteratureFailureCause> {
    match load_rows(csv_path) {
        Ok(_) => Ok(()),
        Err(failure) => Err(failure.into_cause(&artifact_name(csv_path))),
    }
}

/// Map a claims-table parse failure onto the CLOSED core cause taxonomy for the
/// pure-fn runners, whose `Result<(), (u64, ValidationFailureCause)>` signature
/// cannot carry [`LiteratureFailureCause::TableParseError`].
///
/// The honest cause is emitted by the dispatch layer (see
/// [`probe_claims_table`]); this shim is the degraded rendering for direct
/// callers of the pure fns. It preserves the FAILING row index — the old code
/// hardcoded row 0, which read as "row 0's evidence artifact is missing" for a
/// failure that had nothing to do with row 0 or with any artifact. When
/// `ValidationFailureCause` gains a `TableParseError` variant, this shim becomes
/// a `From` conversion and the degrade disappears.
fn table_parse_core_cause(
    artifact: &str,
    failure: TableParseFailure,
) -> (u64, ValidationFailureCause) {
    let row_index = failure.row_index;
    (
        row_index,
        ValidationFailureCause::LiteratureClaim {
            row_index,
            artifact: artifact.to_string(),
            kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
        },
    )
}

fn load_manifest(manifest_path: &Path) -> Result<EvidenceManifest, String> {
    let bytes = fs::read(manifest_path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Derive a package root from an evidence/manifest directory. Evidence dirs are
/// `<package>/runtime/outputs/<task_id>/evidence`; the package root is the
/// ancestor immediately above `runtime/outputs/<task_id>/evidence`. Walk up
/// looking for the `runtime/outputs` boundary and return its parent. Falls back
/// to the dir's grandparent's parent (best effort) when the canonical layout
/// isn't present, so the jail boundary is always well-defined. This is purely
/// deterministic path arithmetic (no filesystem reads).
fn package_root_from_evidence_dir(evidence_dir: &Path) -> std::path::PathBuf {
    // Look for the `.../runtime/outputs` segment and return its parent (the
    // package root). Iterate ancestors; the first whose own tail two components
    // are `runtime/outputs` is the jail's `runtime/outputs` dir.
    let mut anc = Some(evidence_dir);
    while let Some(dir) = anc {
        let is_outputs = dir.file_name().map(|f| f == "outputs").unwrap_or(false)
            && dir
                .parent()
                .and_then(|p| p.file_name())
                .map(|f| f == "runtime")
                .unwrap_or(false);
        if is_outputs {
            // dir == <package>/runtime/outputs → parent of `runtime` is root.
            if let Some(pkg) = dir.parent().and_then(|p| p.parent()) {
                return pkg.to_path_buf();
            }
        }
        anc = dir.parent();
    }
    // Non-canonical layout: `<evidence_dir>/../../..` is the best-effort root
    // (evidence → task → outputs → root). Use `.` if the dir is too shallow.
    evidence_dir
        .parent()
        .and_then(|p| p.parent())
        .and_then(|p| p.parent())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

/// Resolve an evidence-manifest entry's `path` to an on-disk file, JAILED to the
/// package's `runtime/outputs` subtree. The path may be evidence-dir-relative
/// ("pmid_X.xml", the claims-matrix convention) OR task-dir-relative with an
/// "evidence/" prefix (what agent_literature_fetch.py and the agent's PMC fetch
/// write) OR a cross-task sibling reference
/// ("../review_prior_work/evidence/snapshots/<hash>") that
/// contextualize_findings_with_literature writes when it dedups by reusing an
/// upstream literature task's snapshots. Joining a prefixed path straight onto
/// the evidence dir doubles it (evidence/evidence/…) and a `../sibling` path
/// anchored at the evidence dir lands one level too shallow; both spuriously
/// report the artifact missing.
///
/// Candidates tried, in order: the direct evidence-dir join, the
/// "evidence/"-stripped form, the TASK-dir anchor (evidence_dir's parent) for
/// cross-task `../` references, and a basename fallback across the common
/// `["", "sources", "raw", "snapshots"]` subdirs (executor-spelling variance:
/// codex nests under sources/, the canonical helper writes flat, contextualize
/// reuses snapshots/).
///
/// SECURITY (H10): absolute or `..`-bearing `entry_path` is rejected outright
/// (mirrors `required_artifacts::required_artifact_relative_path`); the
/// task-dir-parent candidate alone services legitimate cross-task `../`
/// references. A candidate is RETURNED only if it exists AND, after
/// canonicalize, lives under the jail (`package_root.join("runtime/outputs")`);
/// a candidate that escapes that subtree is rejected even if it exists.
/// Returns the direct evidence-dir join when nothing resolves in-jail, so the
/// caller's `.exists()` check fails cleanly (no escape).
fn resolve_evidence_file(
    package_root: &Path,
    evidence_dir: &Path,
    entry_path: &str,
) -> std::path::PathBuf {
    // Effective jail boundary (deepest existing first): the package's
    // runtime/outputs subtree in the standard layout (permits cross-task
    // `../<sibling-task>/evidence/...` dedup references), falling back to the
    // evidence dir's parent then the evidence dir itself for non-standard /
    // test layouts that have no runtime/outputs tree. Candidate construction
    // already forbids absolute paths, `..`, and ancestor-walking, so this
    // boundary only needs to defend against symlink escape.
    let jail_canon = [
        package_root.join("runtime/outputs"),
        evidence_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| evidence_dir.to_path_buf()),
        evidence_dir.to_path_buf(),
    ]
    .iter()
    .find_map(|j| j.canonicalize().ok());

    // Reject absolute or parent-traversing entry paths outright (mirrors
    // required_artifacts::required_artifact_relative_path).
    let rel = Path::new(entry_path);
    let entry_is_safe = !rel.is_absolute()
        && !rel
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if entry_is_safe {
        candidates.push(evidence_dir.join(entry_path));
        if let Some(stripped) = entry_path.strip_prefix("evidence/") {
            candidates.push(evidence_dir.join(stripped));
        }
        // Task-dir anchor: a path written relative to the task dir
        // (evidence_dir's parent), not the evidence dir.
        if let Some(task_dir) = evidence_dir.parent() {
            candidates.push(task_dir.join(entry_path));
        }
    }
    // Basename fallback — REQUIRED for executor-spelling variance (codex nests
    // under sources/, the canonical helper writes flat, contextualize reuses
    // snapshots/). Kept, but resolved only within the evidence dir's subtree.
    if let Some(base) = rel.file_name() {
        for sub in ["", "sources", "raw", "snapshots"] {
            candidates.push(if sub.is_empty() {
                evidence_dir.join(base)
            } else {
                evidence_dir.join(sub).join(base)
            });
        }
    }
    // Cross-task snapshot reuse: contextualize_findings_with_literature cites
    // snapshots downloaded by an upstream literature task (review_prior_work /
    // survey_method_landscape) but writes the manifest path relative to its own
    // (empty) evidence dir ("snapshots/<sha256>") rather than the explicit
    // "../<task>/evidence/..." form the cross-task branch above expects. Snapshots
    // are content-addressed by sha256, so the basename uniquely identifies the
    // file: locate it under any sibling task's evidence subtree. The canonicalized
    // jail check below confirms the hit lives under <package>/runtime/outputs, so
    // this widens resolution without widening the escape surface.
    if let Some(base) = rel.file_name() {
        let outputs = package_root.join("runtime/outputs");
        if let Ok(entries) = std::fs::read_dir(&outputs) {
            for sib in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                candidates.push(sib.join("evidence").join("snapshots").join(base));
                candidates.push(sib.join("evidence").join(base));
            }
        }
    }

    for cand in &candidates {
        if !cand.exists() {
            continue;
        }
        let Ok(c) = cand.canonicalize() else { continue };
        match &jail_canon {
            // In-jail: accept.
            Some(j) if c.starts_with(j) => return cand.clone(),
            // No boundary canonicalized (degenerate layout): the candidate is
            // still safe by construction (no `..` / absolute / ancestor-walk).
            None => return cand.clone(),
            _ => {}
        }
    }
    // Nothing resolved in-jail: return the direct join so the caller's
    // `.exists()` check fails cleanly (no escape).
    evidence_dir.join(entry_path)
}

// ============================================================================
// Runner 1: pmid_resolves
// ============================================================================

/// Validates that every PMID in `claims_matrix.csv` exists in the evidence manifest
/// and that the referenced evidence file is present on disk.
pub fn run_pmid_resolves(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    let manifest_pmids: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| (e.pmid.clone().unwrap_or_default(), e))
        .collect();

    let pmid_re = regex::Regex::new(r"^[1-9][0-9]{6,8}$").unwrap();

    for (i, row) in rows.iter().enumerate() {
        // Non-asserting rows (no_prior_finding / unverifiable) make no concordance
        // claim, so their (often absent) cited PMIDs are not load-bearing evidence.
        if row_makes_no_concordance_claim(row) {
            continue;
        }
        // Collect candidate PMIDs from row (upstream uses `pmid`, downstream uses `prior_pmids`).
        let prior = row.prior_pmid_list();
        let pmids: Vec<&String> = row.pmid.iter().chain(prior.iter()).collect();
        // no_prior_finding rows legitimately have zero pmids; that's not a failure here.
        for pmid in pmids {
            if !pmid_re.is_match(pmid) {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::PmidMalformed,
                    },
                ));
            }
            if !manifest_pmids.contains_key(pmid) {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::PmidNotFound,
                    },
                ));
            }
            let entry = manifest_pmids[pmid];
            let ev_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
            let evidence_path =
                resolve_evidence_file(&package_root_from_evidence_dir(ev_dir), ev_dir, &entry.path);
            if !evidence_path.exists() {
                return Err((
                    i as u64,
                    ValidationFailureCause::LiteratureClaim {
                        row_index: i as u64,
                        artifact: artifact.clone(),
                        kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
                    },
                ));
            }
        }
    }
    Ok(())
}

// ============================================================================
// Runner 1b: source_resolves (locator-generalized successor to pmid_resolves)
// ============================================================================

/// Build a `LiteratureClaim` failure cause for a row.
fn lit_fail(
    row_index: u64,
    artifact: &str,
    kind: LiteratureClaimFailureKind,
) -> ValidationFailureCause {
    ValidationFailureCause::LiteratureClaim {
        row_index,
        artifact: artifact.to_string(),
        kind,
    }
}

/// Marker source_class for offline / route-failure / thin-literature
/// fallback rows. These carry no locator and are skipped by the
/// locator-resolution validator (and held out of the corroboration tier),
/// so the survey task completes rather than blocking.
const CURATED_BASELINE_CLASS: &str = "curated_baseline";

/// A locator-resolution view of one row, parsed by header name so the
/// validator works against BOTH the claims-matrix shape
/// (`entity`/`entity_kind`/`pmid`) and the method_landscape shape
/// (`axis`/`candidate_method`).
struct SourceRow {
    source_ref_kind: String,
    source_ref: String,
    source_class: String,
    pmid: String,
    prior_pmids: Vec<String>,
}

/// Collect the locator strings a row anchors against, dispatched on its
/// resolved locator kind. Legacy PMID rows pull from `pmid`/`prior_pmids`;
/// non-PMID rows pull from the typed `source_ref` column.
fn source_row_refs(row: &SourceRow, kind: &str) -> Vec<String> {
    if kind == "pmid" {
        let mut refs: Vec<String> = Vec::new();
        if !row.pmid.is_empty() {
            refs.push(row.pmid.clone());
        }
        refs.extend(row.prior_pmids.iter().cloned());
        refs
    } else if row.source_ref.is_empty() {
        Vec::new()
    } else {
        vec![row.source_ref.clone()]
    }
}

/// Generalized successor to `run_pmid_resolves`. For each row, dispatch on
/// `source_ref_kind`:
///   - `pmid` (or absent — legacy) → PMID well-formedness + manifest
///     presence + artifact-on-disk check (byte-identical to
///     `run_pmid_resolves`).
///   - `doi` / `arxiv` / `url` → require a manifest entry whose `source_ref`
///     (falling back to `pmid` for older manifests) matches AND whose
///     snapshot file exists on disk. Any miss → `SourceUnresolvable`.
///
/// Rows whose `source_class` is `curated_baseline` carry no locator (offline /
/// route-failure / thin-literature fallback) and are SKIPPED — neither
/// resolved nor failed — so the survey task never blocks on them. This
/// targets the fallback class explicitly and does not relax the locator
/// checks for any real-locator row.
///
/// The CSV is read by header name so this works against both the
/// claims-matrix shape and the method_landscape shape.
pub fn run_source_resolves(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let by_ref: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| {
            let key = e
                .source_ref
                .clone()
                .or_else(|| e.pmid.clone())
                .unwrap_or_default();
            (key, e)
        })
        .collect();
    let pmid_re = regex::Regex::new(r"^[1-9][0-9]{6,8}$").unwrap();
    let ev_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let package_root = package_root_from_evidence_dir(ev_dir);

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let prior_pmids: Vec<String> = {
            let raw = col(&rec, "prior_pmids");
            if raw.trim().is_empty() {
                Vec::new()
            } else {
                // Legacy column may be a JSON array or a delimited string.
                serde_json::from_str::<Vec<String>>(&raw).unwrap_or_else(|_| {
                    raw.split([',', ';', ' '])
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect()
                })
            }
        };
        let row = SourceRow {
            source_ref_kind: col(&rec, "source_ref_kind"),
            source_ref: col(&rec, "source_ref"),
            source_class: col(&rec, "source_class"),
            pmid: col(&rec, "pmid"),
            prior_pmids,
        };

        // Curated-baseline fallback rows carry no locator: skip them.
        if row.source_class == CURATED_BASELINE_CLASS {
            continue;
        }

        let kind = if row.source_ref_kind.is_empty() {
            "pmid"
        } else {
            row.source_ref_kind.as_str()
        };
        let refs = source_row_refs(&row, kind);
        for r in refs {
            if kind == "pmid" && !pmid_re.is_match(&r) {
                return Err((
                    i as u64,
                    lit_fail(
                        i as u64,
                        &artifact,
                        LiteratureClaimFailureKind::PmidMalformed,
                    ),
                ));
            }
            let Some(entry) = by_ref.get(&r) else {
                let fk = if kind == "pmid" {
                    LiteratureClaimFailureKind::PmidNotFound
                } else {
                    LiteratureClaimFailureKind::SourceUnresolvable
                };
                return Err((i as u64, lit_fail(i as u64, &artifact, fk)));
            };
            if !resolve_evidence_file(&package_root, ev_dir, &entry.path).exists() {
                let fk = if kind == "pmid" {
                    LiteratureClaimFailureKind::EvidenceArtifactMissing
                } else {
                    LiteratureClaimFailureKind::SourceUnresolvable
                };
                return Err((i as u64, lit_fail(i as u64, &artifact, fk)));
            }
        }
    }
    Ok(())
}

// ============================================================================
// Runner 2: evidence_quote_substring_match
// ============================================================================

/// Validates that each `evidence_quote` in `claims_matrix.csv` is a
/// verbatim substring of the normalized evidence text for its PMID.
pub fn run_evidence_quote_substring_match(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    // Key the manifest by every locator an entry exposes (pmid AND the typed
    // source_ref) so this runner resolves claims-matrix rows (pmid) and
    // method_landscape rows (source_ref) uniformly.
    let mut manifest_by_locator: BTreeMap<String, &EvidenceEntry> = BTreeMap::new();
    for e in &manifest.entries {
        if let Some(p) = &e.pmid {
            if !p.is_empty() {
                manifest_by_locator.insert(p.clone(), e);
            }
        }
        if let Some(sr) = &e.source_ref {
            if !sr.is_empty() {
                manifest_by_locator.insert(sr.clone(), e);
            }
        }
        // Batched PubMed efetch: one snapshot covers many PMIDs (no singular
        // pmid/source_ref). Index every batch member so a claim row citing any
        // of them resolves to the shared snapshot.
        for p in &e.pmids_in_batch {
            if !p.is_empty() {
                manifest_by_locator.insert(p.clone(), e);
            }
        }
    }
    let manifest_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let package_root = package_root_from_evidence_dir(manifest_dir);

    for (i, row) in rows.iter().enumerate() {
        // Non-asserting rows (no_prior_finding / unverifiable) carry no asserted
        // quote to substantiate; skip (an unverifiable row's quote is exploratory).
        if row_makes_no_concordance_claim(row) {
            continue;
        }
        // no_prior_finding rows have source_kind == "none" and empty quote; skip.
        if row.source_kind == "none" {
            continue;
        }
        // A verified=false row self-declares its quote as unverified/tentative
        // (the producer's contract: it could not confirm a verbatim match), so
        // it is not a QuoteNotInSource violation — skip rather than hard-fail.
        if !row.verified {
            continue;
        }

        // Resolve the row's locator: pmid (claims-matrix) or the typed
        // source_ref (method_landscape). Rows with neither are skipped
        // (handled by other obligations).
        let locator = row
            .pmid
            .clone()
            .or_else(|| row.prior_pmid_list().into_iter().next())
            .or_else(|| row.source_ref.clone());
        let locator = match locator {
            Some(l) if !l.is_empty() => l,
            _ => continue,
        };

        let entry = manifest_by_locator.get(&locator).ok_or_else(|| {
            (
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::PmidNotFound,
                },
            )
        })?;

        // The manifest `path` may be evidence-dir-relative ("pmid_X.xml", the
        // claims-matrix convention) OR task-dir-relative with an "evidence/"
        // prefix (what the bundled agent_literature_fetch.py helper writes).
        // Resolve against both so the read succeeds either way — joining a
        // prefixed path straight onto the evidence dir doubles it
        // (evidence/evidence/…) and spuriously reports EvidenceArtifactMissing.
        let evidence_path = resolve_evidence_file(&package_root, manifest_dir, &entry.path);
        let raw = fs::read_to_string(&evidence_path).map_err(|_| {
            (
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
                },
            )
        })?;

        if !quote_matches_snapshot(&raw, &row.evidence_quote) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::QuoteNotInSource,
                },
            ));
        }
        // The substring match above is the authoritative verification that the
        // quote is verbatim-present in the source — that is precisely what this
        // obligation (`evidence_quote_substring_match`) is named for. The
        // declared `evidence_quote_offset` is forensic metadata and is NOT
        // hard-failed: producers compute it in the extracted-text frame while
        // the snapshot is stored as the fetched markup (e.g. a PMC-XML record
        // whose ~1KB header offsets every position), so the declared and
        // recomputed offsets legitimately diverge by the per-source header
        // length and no fixed tolerance can reconcile them. Blocking a task on
        // that divergence — when the quote itself is proven present — is a
        // false positive, so the offset is left to forensic inspection only.
    }
    Ok(())
}

// ============================================================================
// Runner 3: redistributable_or_marked
// ============================================================================

/// Source-kind classes whose redistributability is known from the class itself,
/// so a claim row need not carry an explicit `redistributable=true` mark to pass
/// the legal gate. Only the explicitly open-access PMC class qualifies.
/// PubMed abstracts are commonly supplied by publishers and are not NLM-owned,
/// while generic PMC availability does not establish an open license.
/// `external_pdf_local_only` is deliberately EXCLUDED (a locally-stored PDF is
/// not redistributable). Matched by the class PREFIX so executor-specific
/// `pmc_oa_*` spellings are covered without enumerating every variant, while
/// unrelated kinds that merely contain the token are not.
fn source_kind_is_inherently_redistributable(source_kind: &str) -> bool {
    // Scoped to an explicit PMC open-access class. PubMed delivery and generic
    // PMC availability are not license determinations, so `pubmed_*`, `pmc_*`
    // without the `pmc_oa_` marker, metadata aggregators (openalex/crossref),
    // and generic `abstract_only` still require an explicit redistributable
    // mark. Anchoring also prevents unrelated kinds such as
    // `camphor_db_export` from spoofing the legal gate (critical-analysis M8).
    let sk = source_kind.to_ascii_lowercase();
    if sk.starts_with("external_pdf") {
        return false;
    }
    sk.starts_with("pmc_oa_")
}

/// Validates that every row in `claims_matrix.csv` references a redistributable source
/// or is explicitly marked as non-redistributable in the `redistributable` column.
pub fn run_redistributable_or_marked(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    // A leaner claims matrix (e.g. the bulk_rnaseq contextualize schema) omits the
    // per-row `source_kind`/`redistributable` columns, so an asserting row that
    // cites a PMID has source_kind="" and would fail the row-only legal gate below
    // even though its evidence IS redistributable. Honor the manifest: a row whose
    // cited PMID resolves to a manifest entry marked redistributable (or whose
    // source class is explicitly PMC open access) passes the gate.
    // Best-effort: an unreadable manifest leaves the map empty (row-only behavior).
    let manifest = load_manifest(manifest_path).ok();
    let manifest_pmids: BTreeMap<String, &EvidenceEntry> = manifest
        .as_ref()
        .map(|m| {
            m.entries
                .iter()
                .filter_map(|e| e.pmid.clone().map(|p| (p, e)))
                .collect()
        })
        .unwrap_or_default();
    let pmid_redistributable = |row: &ClaimsMatrixRow| -> bool {
        let mut pmids = row.prior_pmid_list();
        if let Some(p) = row.pmid.as_deref() {
            if !p.trim().is_empty() {
                pmids.push(p.trim().to_string());
            }
        }
        pmids.iter().any(|p| {
            manifest_pmids
                .get(p)
                .map(|e| {
                    e.redistributable || source_kind_is_inherently_redistributable(&e.source_kind)
                })
                .unwrap_or(false)
        })
    };
    for (i, row) in rows.iter().enumerate() {
        if row.source_kind == "none" {
            continue;
        }
        // Non-asserting rows (no_prior_finding / unverifiable) make no concordance
        // claim, and source-less rows carry no prior literature by definition (empty
        // source_kind / source_ref_kind / pmid). There is no asserted source to
        // subject to the legal gate, so skip (mirrors the curated-baseline carve-out
        // below). A row that DID assert + cite a source carries a non-empty
        // source_kind and is gated normally.
        if row_makes_no_concordance_claim(row)
            || (row.source_kind.is_empty()
                && row.source_ref_kind.as_deref().unwrap_or("").is_empty()
                && row.pmid.as_deref().unwrap_or("").is_empty())
        {
            continue;
        }
        // `curated_baseline` candidate rows are offline / thin-literature
        // placeholders carrying no real source (the locator validator
        // `run_source_resolves` already skips them). They legitimately have an
        // empty `redistributable` column and a placeholder `source_kind`
        // (`curated_candidate`) that is not a redistributable corpus class, so
        // the legal gate must not subject them to the source_kind match below
        // (which would otherwise fall through to a spurious FAIL).
        if row.source_class.as_deref() == Some(CURATED_BASELINE_CLASS) {
            continue;
        }
        // The legal gate stays meaningful: it is keyed by `source_kind`, never
        // blanket-true. External PDFs stored locally MUST NOT be marked
        // redistributable; paper-class OA / abstract sources (incl. the
        // OpenAlex/Crossref-surfaced OA records the canonical helper emits, all
        // legal in literature_evidence_manifest.schema.json) MUST be marked
        // redistributable to pass. The earlier table only matched three v1
        // source_kinds and fell through to a spurious FAIL for the v2 enum
        // values the producer legitimately uses.
        let consistent = match (row.source_kind.as_str(), row.redistributable) {
            // External PDFs are stored locally only — true is a contradiction.
            ("external_pdf_local_only", false) => true,
            ("external_pdf_local_only", true) => false,
            // Tool-documentation pages are pages, not redistributed corpus —
            // either marking is legal.
            ("doc_page", _) => true,
            // A literature source explicitly marked redistributable passes.
            (_, true) => true,
            // Explicit PMC OA classes are redistributable by class. PubMed
            // abstracts and generic PMC records must carry a manifest or row
            // marker with the applicable license/policy basis; delivery through
            // NLM alone is not a copyright determination. The legal gate stays
            // strict for every unrecognised or unmarked class.
            (sk, false) if source_kind_is_inherently_redistributable(sk) => true,
            (_, false) => false,
        };
        // Manifest-backed redistributability for leaner claims schemas (rows that
        // cite a PMID but omit source_kind): the cited evidence is redistributable
        // per the manifest even though the row can't prove it on its own.
        let consistent = consistent || pmid_redistributable(row);
        if !consistent {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::RedistributableTagInconsistent,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 4: claim_row_has_finding_id (downstream only)
// ============================================================================

/// Validates that each literature claim row in `claims_matrix.csv` references a
/// `finding_id` that exists in the upstream `findings_csv_path`.
pub fn run_claim_row_has_finding_id(
    csv_path: &Path,
    findings_csv_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    // Load findings table primary keys (first column or `id` column). The
    // findings file is delimiter-sniffed: the analysis atoms emit TAB-separated
    // `.tsv` (de_results.tsv, peak_calls.tsv, variant_calls.tsv) — reading those
    // with the comma default collapses every row into one field, so the bare
    // gene/peak id never lands in `known` and every claim row spuriously orphans.
    let delimiter = if findings_csv_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("tsv"))
        .unwrap_or(false)
    {
        b'\t'
    } else {
        b','
    };
    let mut findings_rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .from_path(findings_csv_path)
        .map_err(|_| {
            (
                0,
                ValidationFailureCause::LiteratureClaim {
                    row_index: 0,
                    artifact: artifact.clone(),
                    kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
                },
            )
        })?;
    let headers = findings_rdr.headers().cloned().map_err(|_| {
        (
            0,
            ValidationFailureCause::LiteratureClaim {
                row_index: 0,
                artifact: artifact.clone(),
                kind: LiteratureClaimFailureKind::EvidenceArtifactMissing,
            },
        )
    })?;
    // Collect known finding identifiers from EVERY id-like column, not just one
    // PK: producers key the claims-matrix finding_id off whichever upstream
    // column is handy — ensembl id, bare gene/protein symbol, feature name, or a
    // stage-prefixed composite. Matching against all of them (with pk_col=0 as a
    // fallback when none are recognized) resolves a finding_id that names the row
    // by any of its identifiers, instead of orphaning on a column-choice mismatch.
    let id_cols: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| {
            matches!(
                h.to_ascii_lowercase().as_str(),
                "id" | "gene_id"
                    | "peak_id"
                    | "variant_id"
                    | "ensembl_id"
                    | "ensembl_gene_id"
                    | "gene_symbol"
                    | "gene_name"
                    | "feature"
                    | "name"
                    | "symbol"
                    | "entity_id"
                    | "protein"
                    | "protein_id"
                    | "uniprot"
                    | "uniprot_id"
            )
        })
        .map(|(i, _)| i)
        .collect();
    let id_cols = if id_cols.is_empty() { vec![0] } else { id_cols };

    let mut known: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rec in findings_rdr.records().flatten() {
        for &ci in &id_cols {
            if let Some(v) = rec.get(ci) {
                if !v.is_empty() {
                    known.insert(v.to_string());
                }
            }
        }
    }

    for (i, row) in rows.iter().enumerate() {
        let fid = match &row.finding_id {
            Some(s) => s.clone(),
            None => continue,
        };
        // A finding_id resolves when it matches an upstream PK exactly, OR when
        // it's a conventionally-prefixed/entity-keyed form of one. Agents form
        // a stable finding identifier from the analysis-stage findings (e.g.
        // `DE_FBgn0000043` for DE gene `FBgn0000043`, or carry the bare
        // `entity_id`); the underlying finding is the same row. Accept:
        //   - exact PK match;
        //   - PK after stripping a leading `<STAGE>_` prefix (DE_/PE_/…);
        //   - the row's own `entity_id` matching a PK (some atoms key on it).
        // Strip a leading stage/namespace prefix delimited by `_` (DE_/PE_/…) OR
        // `:` (`de:`/`peak:`/`var:` — the colon-namespaced convention this
        // producer also uses, e.g. `de:GPNMB` for upstream PK `GPNMB`). Either
        // spelling names the same upstream finding row.
        let strip_us = fid.split_once('_').map(|(_, rest)| rest).unwrap_or(&fid);
        let strip_colon = fid.split_once(':').map(|(_, rest)| rest).unwrap_or(&fid);
        // Multi-segment finding ids embed the upstream PK as a delimited segment,
        // e.g. `DE_BRIX1_ENSG00000113460.13` (stage_gene_ensembl) whose upstream PK
        // is the bare `ENSG00000113460.13`. Resolve if ANY `_`/`:`-delimited segment
        // is a known PK — upstream PKs are unique gene/ensembl/peak/variant ids, so a
        // segment hit names the same finding rather than a coincidental collision.
        let segment_hit = fid
            .split([':', '_'])
            .any(|seg| !seg.is_empty() && known.contains(seg));
        let resolved = known.contains(&fid)
            || known.contains(strip_us)
            || known.contains(strip_colon)
            || segment_hit
            || (!row.entity.is_empty() && known.contains(&row.entity));
        if !resolved {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::FindingIdOrphan,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 5: concordance_flag_in_closed_set (downstream only)
// ============================================================================

/// Validates that every `concordance_flag` value in `claims_matrix.csv` belongs
/// to the closed set defined by `LiteratureClaimFailureKind`.
pub fn run_concordance_flag_in_closed_set(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    let closed = [
        "same_direction",
        "opposite_direction",
        "no_prior_finding",
        "not_assessed",
        "unverifiable",
    ];
    for (i, row) in rows.iter().enumerate() {
        let flag = match &row.concordance_flag {
            Some(f) => f.as_str(),
            None => continue,
        };
        if !closed.contains(&flag) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::InvalidConcordanceFlag,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 5b: direction_supported_by_quote (downstream only)
// ============================================================================

/// Directional cue substrings. A concordance direction (same_direction /
/// opposite_direction) is only supportable when the cited evidence_quote
/// itself states a direction. The atom claim_boundary
/// (config/stage-atoms/contextualize_findings_with_literature.yaml) is
/// explicit: same/opposite require the prior's effect direction; a quote that
/// names no direction mandates `unverifiable`. Matched against the
/// `collapse_whitespace_lowercase_v1`-normalized quote, so every needle here is
/// lowercase. Substrings (not whole words) so morphological variants are
/// covered: `induc` matches induced/induces/induction; `repress` matches
/// repressed/repression; `upregulat`/`downregulat` match the -e/-ed/-ion forms;
/// `elevat`/`reduc` match elevated/elevation/reduced/reduction.
const DIRECTIONAL_CUES: &[&str] = &[
    "increase",
    "decrease",
    "induc",
    "repress",
    "elevat",
    "reduc",
    "higher",
    "lower",
    "upregulat",
    "downregulat",
    "up-regulat",
    "down-regulat",
    "overexpress",
    "underexpress",
    "enrich",
    "deplet",
    "suppress",
    "activat",
    "inhibit",
    "gain",
    "loss",
    "positively correlat",
    "negatively correlat",
];

/// A directional concordance flag — `same_direction` / `opposite_direction` —
/// asserts that the prior literature reports a direction matching (or opposing)
/// this dataset's effect sign. That assertion is only supported when the cited
/// `evidence_quote` actually states a direction.
fn flag_asserts_direction(flag: Option<&str>) -> bool {
    matches!(flag, Some("same_direction") | Some("opposite_direction"))
}

/// True iff the normalized quote contains any directional cue. The standalone
/// tokens `up` / `down` are matched as whole words (space-or-edge delimited) so
/// they don't spuriously fire inside unrelated words (e.g. "upstream",
/// "downstream", "boundary"); the morphological stems above use plain
/// containment.
fn quote_states_direction(normalized_quote: &str) -> bool {
    if DIRECTIONAL_CUES
        .iter()
        .any(|c| normalized_quote.contains(c))
    {
        return true;
    }
    // Whole-word `up` / `down` (the bare directional adverbs). Split on spaces;
    // the quote is already whitespace-collapsed and lowercased.
    normalized_quote
        .split(' ')
        .any(|w| w == "up" || w == "down")
}

/// Validates that every row carrying a directional concordance flag
/// (`same_direction` / `opposite_direction`) cites an `evidence_quote` that
/// itself states a direction. This enforces the atom claim_boundary: the
/// concordance-matrix builder assigns same/opposite from THIS dataset's log2FC
/// sign, and nothing else stops it from doing so when the quote names no
/// direction (the circular IRS2/MFGE8 "replication" assigned from a directionless
/// panel-membership quote). A directional flag backed by a directionless quote
/// is an unsupported claim and the row fails with
/// `DirectionNotSupportedByQuote`.
///
/// Non-asserting rows (`no_prior_finding` / `unverifiable`) are NOT subject to
/// the check — they make no directional claim, which is exactly the verdict the
/// boundary mandates for a directionless quote.
pub fn run_direction_supported_by_quote(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let rows = load_rows(csv_path).map_err(|f| table_parse_core_cause(&artifact, f))?;
    for (i, row) in rows.iter().enumerate() {
        if !flag_asserts_direction(row.concordance_flag.as_deref()) {
            continue;
        }
        let normalized_quote = collapse_whitespace_lowercase_v1(&row.evidence_quote);
        if !quote_states_direction(&normalized_quote) {
            return Err((
                i as u64,
                ValidationFailureCause::LiteratureClaim {
                    row_index: i as u64,
                    artifact,
                    kind: LiteratureClaimFailureKind::DirectionNotSupportedByQuote,
                },
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 6: claim_support_satisfied
// ============================================================================

/// Column-name → index map for a CSV, so validators that key off the
/// `method_landscape.csv` shape (axis/candidate_method/source_class/...) work
/// without forcing the full `ClaimsMatrixRow` serde struct, which requires
/// the prior-claims columns the method-landscape table does not carry.
fn header_index(headers: &csv::StringRecord) -> BTreeMap<String, usize> {
    headers
        .iter()
        .enumerate()
        .map(|(i, h)| (h.to_string(), i))
        .collect()
}

const PAPER_CLASSES: [&str; 2] = ["primary_literature", "conference_proceedings"];

/// Read `claimSupportRules.minimumIndependentSources` from a package's
/// `source-discovery-policy.json`. The CSV lives at
/// `<package>/runtime/outputs/<task_id>/method_landscape.csv`; the policy at
/// `<package>/policies/source-discovery-policy.json`. We walk up from the CSV
/// dir to find a `policies/source-discovery-policy.json`. Absent/unreadable →
/// default of 2. The JSON is read directly (no schema validation) so the
/// harness validator stays self-contained.
fn minimum_independent_sources(csv_path: &Path) -> u64 {
    const DEFAULT_MIN: u64 = 2;
    let mut cur = csv_path.parent();
    while let Some(dir) = cur {
        let candidate = dir.join("policies/source-discovery-policy.json");
        if candidate.exists() {
            if let Ok(bytes) = fs::read(&candidate) {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                    if let Some(n) = v
                        .get("claimSupportRules")
                        .and_then(|r| r.get("minimumIndependentSources"))
                        .and_then(|n| n.as_u64())
                    {
                        return n;
                    }
                }
            }
            return DEFAULT_MIN;
        }
        cur = dir.parent();
    }
    DEFAULT_MIN
}

/// Validates `source-discovery-policy.json::claimSupportRules` against
/// `method_landscape.csv` with **per-axis, de-ranking** semantics: an axis is
/// recommendable when it carries ≥1 adequately-corroborated default — a
/// candidate with ≥1 verified paper-class source (source_class ∈
/// {primary_literature, conference_proceedings}) AND ≥`minimumIndependentSources`
/// distinct verified sources. A literature-eligible candidate that falls short
/// is simply NOT a valid default (de-ranked); it does not fail the survey as
/// long as its axis still has a corroborated alternative. This avoids one weak
/// peripheral candidate (e.g. an unused filtering tool with a single citation)
/// blocking an entire otherwise-recommendable survey.
///
/// Two cases remain hard failures:
///   (a) a candidate EXPLICITLY tier-marked `defaultRecommended` (when the
///       optional `tier` column is present) that is not adequately supported —
///       the survey makes a specific unsupported recommendation. Corroboration
///       here is PER-CANDIDATE: the marked default must itself carry ≥1 verified
///       paper-class source AND ≥`minimumIndependentSources` distinct verified
///       sources; and
///   (b) a literature-eligible axis (≥1 candidate with a verified paper-class
///       source) whose candidate set, TAKEN TOGETHER, cites fewer than
///       `minimumIndependentSources` DISTINCT verified paper-class sources.
///       Corroboration is an AXIS-level property — the axis's runtime
///       method-choice is what is being grounded — so it may be distributed
///       across candidates: thin per-candidate retrieval (one abstract per
///       method) still corroborates the axis when independent methods cite
///       independent papers. An axis grounded by only a single paper across all
///       its candidates is genuinely under-corroborated and fails; an axis with
///       no literature grounding at all (fully `curated_baseline` /
///       tool-doc fallback) is not eligible and imposes no obligation.
///
/// The method_landscape schema does not define a `tier` column today
/// (`additionalProperties: false`), so in practice (a) never fires and the
/// check reduces to axis-level corroboration (b). Tool-doc-only candidates are
/// never literature_eligible, so they impose no corroboration obligation.
///
/// `manifest_path` is unused (the corroboration policy is read from the
/// package, not the evidence manifest); the two-arg signature matches the
/// other runners so the shared `runner_dispatch` can call it.
pub fn run_claim_support_satisfied(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let min_sources = minimum_independent_sources(csv_path);

    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    // Per (axis, candidate): track first row index, whether tier marks it a
    // default, paper-class verified count, and the set of distinct verified
    // source identities.
    #[derive(Default)]
    struct Acc {
        first_row: u64,
        tier_default: bool,
        paper_class_verified: u64,
        /// Distinct verified sources of ANY class — drives per-candidate
        /// corroboration for an explicit `defaultRecommended` (case a).
        verified_sources: std::collections::BTreeSet<String>,
        /// Distinct verified PAPER-class source identities — unioned across an
        /// axis's candidates for axis-level corroboration (case b).
        paper_sources: std::collections::BTreeSet<String>,
        seen: bool,
    }
    let mut acc: BTreeMap<(String, String), Acc> = BTreeMap::new();

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let axis = col(&rec, "axis");
        let cand = col(&rec, "candidate_method");
        if axis.is_empty() || cand.is_empty() {
            continue;
        }
        let class = col(&rec, "source_class");
        let verified = col(&rec, "verified") == "true";
        let tier = col(&rec, "tier");
        // Distinct-source identity: prefer the locator, fall back to the hash.
        let source_id = {
            let r = col(&rec, "source_ref");
            if !r.is_empty() {
                r
            } else {
                col(&rec, "source_hash")
            }
        };

        let e = acc.entry((axis, cand)).or_default();
        if !e.seen {
            e.first_row = i as u64;
            e.seen = true;
        }
        if tier == "defaultRecommended" {
            e.tier_default = true;
        }
        if verified {
            let is_paper = PAPER_CLASSES.contains(&class.as_str());
            if is_paper {
                e.paper_class_verified += 1;
            }
            if !source_id.is_empty() {
                if is_paper {
                    e.paper_sources.insert(source_id.clone());
                }
                e.verified_sources.insert(source_id);
            }
        }
    }

    // De-ranking semantics: a literature-eligible candidate that is
    // under-corroborated is simply NOT a valid default (it is de-ranked, not a
    // failure) as long as its axis still carries an adequately-corroborated
    // alternative — one weak peripheral candidate must not block a whole survey
    // whose axes are otherwise recommendable. Two hard failures remain:
    //   (a) a candidate EXPLICITLY tier-marked `defaultRecommended` that is not
    //       adequately supported — the survey is making a specific unsupported
    //       recommendation; and
    //   (b) an axis that presents ≥1 literature-eligible candidate but carries
    //       NO adequately-corroborated default — that axis cannot be recommended
    //       at all.
    // A "valid default" carries ≥1 paper-class verified source AND
    // ≥`min_sources` distinct verified sources. `acc` is a BTreeMap, so every
    // iteration below is deterministic.
    let is_valid_default = |a: &Acc| -> bool {
        a.paper_class_verified >= 1 && (a.verified_sources.len() as u64) >= min_sources
    };

    // (a) explicit defaultRecommended that is unsupported — fail at its first row.
    for a in acc.values() {
        if a.tier_default && !is_valid_default(a) {
            return Err((
                a.first_row,
                lit_fail(
                    a.first_row,
                    &artifact,
                    LiteratureClaimFailureKind::InsufficientCorroboration,
                ),
            ));
        }
    }

    // (b) per-axis corroboration: a literature-eligible axis (≥1 candidate with
    // a verified paper-class source) must cite ≥`min_sources` DISTINCT verified
    // paper-class sources across its candidate set — corroboration is an
    // axis-level property and may be distributed across candidates. This lets
    // thin per-candidate retrieval (one abstract per method) still corroborate
    // an axis when independent methods cite independent papers, while an axis
    // grounded by only a single paper across ALL its candidates still fails.
    // Non-eligible axes (fully curated_baseline / tool-doc fallback) carry no
    // obligation. `acc` is a BTreeMap, so iteration is deterministic.
    let mut axis_paper_sources: BTreeMap<&str, std::collections::BTreeSet<String>> =
        BTreeMap::new();
    let mut axis_eligible_row: BTreeMap<&str, u64> = BTreeMap::new();
    for ((axis, _cand), a) in &acc {
        if a.paper_class_verified >= 1 {
            axis_eligible_row
                .entry(axis.as_str())
                .and_modify(|r| *r = (*r).min(a.first_row))
                .or_insert(a.first_row);
        }
        let set = axis_paper_sources.entry(axis.as_str()).or_default();
        for s in &a.paper_sources {
            set.insert(s.clone());
        }
    }
    for (axis, first_row) in &axis_eligible_row {
        let distinct = axis_paper_sources
            .get(*axis)
            .map(|s| s.len() as u64)
            .unwrap_or(0);
        if distinct < min_sources {
            return Err((
                *first_row,
                lit_fail(
                    *first_row,
                    &artifact,
                    LiteratureClaimFailureKind::InsufficientCorroboration,
                ),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 7: method_quote_mentions_candidate
// ============================================================================

/// Require every paper-class method-landscape row to carry a retained quote
/// that explicitly names the candidate method (or a canonical compound-method
/// alias). A query hit plus a verbatim generic background sentence is not a
/// machine-auditable evidence link for the candidate.
pub fn run_method_quote_mentions_candidate(
    csv_path: &Path,
    _manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());
    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        let class = col(&rec, "source_class");
        if !PAPER_CLASSES.contains(&class.as_str()) {
            continue;
        }
        let candidate = col(&rec, "candidate_method");
        let quote = col(&rec, "evidence_quote");
        if candidate.is_empty()
            || !ecaa_workflow_core::method_landscape::evidence_quote_mentions_candidate(
                &quote, &candidate,
            )
        {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::CandidateNotInEvidenceQuote,
                ),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 8: doc_page_matches_tool (+ version_context guard)
// ============================================================================

/// Validates each `source_class == tool_documentation` row in
/// `method_landscape.csv`: (1) the snapshot named by the row's matching
/// evidence-manifest entry, after `collapse_whitespace_lowercase_v1`
/// normalization, must reference the `candidate_method` token — else
/// `DocPageToolMismatch`; (2) the row's `version_context` must be present and
/// non-empty — else `VersionContextMissing`. Non-tool-doc rows are skipped.
pub fn run_doc_page_matches_tool(
    csv_path: &Path,
    manifest_path: &Path,
) -> Result<(), (u64, ValidationFailureCause)> {
    let artifact = csv_path
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| csv_path.to_string_lossy().to_string());

    let mut rdr = csv::Reader::from_path(csv_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let headers = rdr.headers().cloned().map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    let idx = header_index(&headers);
    let col = |rec: &csv::StringRecord, name: &str| -> String {
        idx.get(name)
            .and_then(|i| rec.get(*i))
            .unwrap_or("")
            .to_string()
    };

    let manifest = load_manifest(manifest_path).map_err(|_| {
        (
            0,
            lit_fail(
                0,
                &artifact,
                LiteratureClaimFailureKind::EvidenceArtifactMissing,
            ),
        )
    })?;
    // Index manifest entries by locator (source_ref, falling back to pmid).
    let by_ref: BTreeMap<String, &EvidenceEntry> = manifest
        .entries
        .iter()
        .map(|e| {
            let key = e
                .source_ref
                .clone()
                .or_else(|| e.pmid.clone())
                .unwrap_or_default();
            (key, e)
        })
        .collect();
    let ev_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let package_root = package_root_from_evidence_dir(ev_dir);

    for (i, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|_| {
            (
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::EvidenceArtifactMissing,
                ),
            )
        })?;
        if col(&rec, "source_class") != "tool_documentation" {
            continue;
        }
        let candidate = col(&rec, "candidate_method");
        let version_context = col(&rec, "version_context");
        let source_ref = col(&rec, "source_ref");

        // Version-context guard: tool-doc method claims must carry one.
        if version_context.trim().is_empty() {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::VersionContextMissing,
                ),
            ));
        }

        // Snapshot relevance: the doc page must name the candidate tool.
        let Some(entry) = by_ref.get(&source_ref) else {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::SourceUnresolvable,
                ),
            ));
        };
        let raw = fs::read_to_string(resolve_evidence_file(&package_root, ev_dir, &entry.path))
            .map_err(|_| {
                (
                    i as u64,
                    lit_fail(
                        i as u64,
                        &artifact,
                        LiteratureClaimFailureKind::EvidenceArtifactMissing,
                    ),
                )
            })?;
        let normalized = collapse_whitespace_lowercase_v1(&raw);
        let token = collapse_whitespace_lowercase_v1(&candidate);
        if token.is_empty() || !normalized.contains(&token) {
            return Err((
                i as u64,
                lit_fail(
                    i as u64,
                    &artifact,
                    LiteratureClaimFailureKind::DocPageToolMismatch,
                ),
            ));
        }
    }
    Ok(())
}

// ============================================================================
// Runner 8: gene_symbol_ensembl_consistent (Workstream B — committed,
//           INDEPENDENT gene-symbol↔Ensembl identity cross-check)
// ============================================================================
//
// `ValidatorOutcome` is imported at the trait-wrappers section below
// (`use crate::validators::{ValidatorOutcome, ValidatorRunner};`), which is
// module-scoped and so is in scope here too.

/// An unresolved / missing gene-symbol sentinel — the value the contextualize
/// step (or an org.Hs.eg.db lookup) writes when a locus has no symbol. It is
/// NOT a real symbol, so it carries no symbol↔Ensembl binding to validate:
/// BOTH the truth-map loader and the per-row consistency check skip it.
/// Without this, several unresolved loci that all share `"NA"` collapse to the
/// first `NA → Ensembl` binding in the truth map, and every other `NA` row then
/// false-flags as a cross-gene wrong-binding against that arbitrary first
/// Ensembl (the 2026-07-23 himes deposit domain-validation failure).
fn is_unresolved_gene_symbol(s: &str) -> bool {
    // NA-family sentinels, shared with the CSV-lenient deserializers so absence
    // is spelled the same way in every column …
    is_absent_sentinel(s)
        // … plus the word-form "no symbol resolved" placeholders other
        // annotation steps write. Kept local to the symbol reader: these are
        // real (if useless) strings, not absent values, so a typed column must
        // still reject them.
        || matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "unresolved" | "unmapped" | "unknown" | "unassigned"
        )
}

// ============================================================================
// Entity-label ↔ accession COLUMN ROLES.
// ============================================================================
//
// The resolver itself — the accession SHAPE predicate, the accession/label/
// effect candidate lists, the per-column content vote, the tie-break ranking and
// `resolve_entity_column_roles` — lives in
// `ecaa_workflow_core::entity_columns`, imported at the top of this file.
// `core::claim_extractor` reads the same roles for its VF-13 annotation table,
// and while each side carried its own lists they drifted into two INVERTED
// copies: one bound `gene` (holding ENSG accessions) as the entity LABEL and
// then found no accession column at all.
//
// What stays here is the harness's use of those roles: the truth-table loaders,
// the discovery scan, and the obligation runner.

/// Read a delimited table into a `(label -> accession)` map, resolving the two
/// columns by [`resolve_entity_column_roles`] (content-first, name-fallback).
/// Returns `None` when the file is unreadable, carries only one of the two
/// roles, or yields no usable binding — the caller treats that as "no
/// independent annotation source" (soft-skip).
fn load_symbol_ensembl_map(path: &Path) -> Option<BTreeMap<String, String>> {
    let mut rdr = open_delimited_table(path)?;
    let headers = rdr.headers().ok()?.clone();
    let sample = sniff_table_rows(&mut rdr, ENTITY_ROLE_SNIFF_ROWS);
    let roles = resolve_entity_column_roles(&headers, &sample)?;
    let mut map: BTreeMap<String, String> = BTreeMap::new();
    for rec in sample.iter().cloned().chain(rdr.records().flatten()) {
        let (Some(sym), Some(ens)) = (rec.get(roles.label_idx), rec.get(roles.accession_idx))
        else {
            continue;
        };
        let (sym, ens) = (sym.trim(), ens.trim());
        if is_unresolved_gene_symbol(sym) || ens.is_empty() {
            continue;
        }
        // First binding wins (deterministic over the BTreeMap on re-read); the
        // independent annotation table is label-unique by construction.
        map.entry(sym.to_string())
            .or_insert_with(|| ens.to_string());
    }
    if map.is_empty() {
        None
    } else {
        Some(map)
    }
}

// ============================================================================
// DR-10: paralog / segmental-duplication-aware classification of a
//        gene-symbol↔Ensembl disagreement.
// ============================================================================
//
// A raw symbol↔Ensembl disagreement is not always a real error. Human
// segmental-duplication / alt-haplotype loci (LRRC37A, GOLGA8*, SMN, …) carry
// near-identical paralogs whose reads cross-map, so a quantifier legitimately
// reassigns a symbol to a paralog's Ensembl id across releases. A citation
// attached to such a paralog is a BENIGN ambiguity (warn) — but ONLY when
// (a) both identifiers fall in the same curated family, (b) the DE
// direction/effect is concordant, and (c) the target is not a pseudogene.
// A true cross-gene wrong-binding at an unrelated locus (the historical
// CRISPLD2→ACSL5), a pseudogene target of an expression claim, and a
// same-family pair with OPPOSITE clinical meaning (SMN1↔SMN2) all stay
// required failures. The curated table lives in the embedded
// `paralog_families.json`, keyed to a pinned Ensembl release.

#[derive(Debug, Clone, Deserialize)]
struct ParalogMember {
    symbol: String,
    ensembl: String,
    #[serde(default)]
    pseudogene: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct ParalogFamily {
    #[allow(dead_code)]
    family_id: String,
    #[serde(default)]
    opposite_clinical_meaning: bool,
    members: Vec<ParalogMember>,
}

#[derive(Debug, Clone, Deserialize)]
struct ParalogTable {
    #[serde(default)]
    #[allow(dead_code)]
    ensembl_release: String,
    families: Vec<ParalogFamily>,
}

/// Curated paralog/family table, parsed once from the embedded JSON.
fn paralog_table() -> &'static ParalogTable {
    static TABLE: std::sync::OnceLock<ParalogTable> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        serde_json::from_str(include_str!("paralog_families.json"))
            .expect("embedded paralog_families.json must parse")
    })
}

/// Normalize an Ensembl gene accession: extract the first `ENSG<digits>`
/// token (dropping any `.version` suffix), uppercased. Handles bare ids
/// (`ENSG00000103196`), versioned ids (`ENSG00000103196.13`), and composite
/// finding ids (`DE_CRISPLD2_ENSG00000103196.13`). Returns `None` when the
/// string carries no Ensembl gene accession.
fn norm_ensembl(s: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)ENSG[0-9]{6,}").unwrap());
    re.find(s).map(|m| m.as_str().to_ascii_uppercase())
}

/// Extract candidate gene SYMBOL tokens from a composite finding id such as
/// `DE_LRRC37A2_ENSG00000238083`: split on `_`/`:`/whitespace and keep the
/// segments that look like a bare symbol (alphanumeric, not an Ensembl
/// accession, not the `DE`/stage prefix). Used so a family match can key on
/// the symbol the finding id names even when the release re-versioned the
/// accession.
fn extract_symbol_tokens(finding_id: &str) -> Vec<String> {
    finding_id
        .split(|c: char| c == '_' || c == ':' || c.is_whitespace())
        .map(|t| t.trim())
        .filter(|t| {
            !t.is_empty()
                && !t.eq_ignore_ascii_case("DE")
                && norm_ensembl(t).is_none()
                && t.chars().any(|c| c.is_ascii_alphabetic())
        })
        .map(|t| t.to_ascii_uppercase())
        .collect()
}

/// Backstop pseudogene heuristic (the curated table's per-member `pseudogene`
/// flag is authoritative; this only fires for symbols absent from the table).
/// Recognizes the classic parent-symbol + `P` processed-pseudogene naming
/// (`GOLGA6L3P` — a digit immediately before a trailing `P`) and the TP53TG
/// pseudogene-like cluster. Errs toward `true` (which keeps a case as a
/// required failure), so a false positive over-blocks rather than under-blocks.
fn is_pseudogene_symbol_heuristic(sym: &str) -> bool {
    let up = sym.to_ascii_uppercase();
    if up.starts_with("TP53TG") {
        return true;
    }
    // Trailing `P` preceded by a digit: `...3P` (processed-pseudogene marker).
    let bytes = up.as_bytes();
    if bytes.len() >= 2 && bytes[bytes.len() - 1] == b'P' && bytes[bytes.len() - 2].is_ascii_digit()
    {
        return true;
    }
    false
}

/// The curated family (if any) that lists `symbol` as a member, plus that
/// member's pseudogene flag. Case-insensitive on the symbol.
fn family_for_symbol(symbol: &str) -> Option<(&'static ParalogFamily, bool)> {
    let up = symbol.to_ascii_uppercase();
    for fam in &paralog_table().families {
        for m in &fam.members {
            if m.symbol.eq_ignore_ascii_case(&up) {
                return Some((fam, m.pseudogene));
            }
        }
    }
    None
}

/// `true` when `claimed_ens` (a normalized Ensembl id, if extractable) OR any
/// `claimed_syms` token names a member of `fam` — i.e. the claimed identifier
/// is a same-family paralog rather than an unrelated locus.
fn claimed_is_same_family(
    fam: &ParalogFamily,
    claimed_ens: Option<&str>,
    claimed_syms: &[String],
) -> bool {
    fam.members.iter().any(|m| {
        let ens_hit = match (claimed_ens, norm_ensembl(&m.ensembl)) {
            (Some(c), Some(me)) => c == me,
            _ => false,
        };
        let sym_hit = claimed_syms
            .iter()
            .any(|s| s.eq_ignore_ascii_case(&m.symbol));
        ens_hit || sym_hit
    })
}

/// Is the DE direction/effect of the claimed paralog concordant with the true
/// gene? Authoritative signal: same sign of the effect column for both
/// Ensembl ids in the independent annotation table. When the claimed
/// paralog's effect is not present in the table, fall back to the claims-row
/// `concordance_flag` (`same_direction` ⇒ concordant, `opposite_direction` ⇒
/// discordant). When neither can establish concordance, return `false` —
/// DR-10 requires POSITIVE concordance to downgrade, so an indeterminate
/// direction stays a required failure.
fn direction_concordant(
    claimed_ens: Option<&str>,
    truth_ens: &str,
    concordance_flag: Option<&str>,
    effects: &BTreeMap<String, f64>,
) -> bool {
    let truth_eff = effects.get(truth_ens);
    let claimed_eff = claimed_ens.and_then(|c| effects.get(c));
    if let (Some(&ce), Some(&te)) = (claimed_eff, truth_eff) {
        return (ce > 0.0 && te > 0.0) || (ce < 0.0 && te < 0.0) || (ce == 0.0 && te == 0.0);
    }
    matches!(
        concordance_flag.map(|f| f.trim().to_ascii_lowercase()),
        Some(f) if f == "same_direction"
    )
}

/// Severity of one classified symbol↔Ensembl disagreement (DR-10).
#[derive(Debug, Clone, PartialEq, Eq)]
enum MismatchSeverity {
    /// A benign same-family paralog ambiguity — recorded as a warning, does
    /// NOT block the deposit.
    Warn,
    /// A real disagreement that must block the deposit (unrelated locus,
    /// pseudogene target, opposite clinical meaning, or discordant direction).
    Required,
}

/// Classify a single disagreement into warn-vs-required per DR-10.
fn classify_mismatch(
    symbol: &str,
    claimed_display: &str,
    claimed_ens: Option<&str>,
    truth_ens: &str,
    concordance_flag: Option<&str>,
    effects: &BTreeMap<String, f64>,
) -> (MismatchSeverity, String) {
    let Some((fam, member_is_pseudogene)) = family_for_symbol(symbol) else {
        // Not in any curated paralog family → treat as an unrelated-locus
        // cross-gene wrong-binding (the CRISPLD2→ACSL5 class).
        return (
            MismatchSeverity::Required,
            format!(
                "{symbol}: finding_id {claimed_display} != annotation {truth_ens} \
                 (cross-gene wrong-binding at an unrelated locus; not in the curated paralog table)"
            ),
        );
    };

    // (c) Pseudogene target of an expression claim — never downgraded.
    if member_is_pseudogene || is_pseudogene_symbol_heuristic(symbol) {
        return (
            MismatchSeverity::Required,
            format!(
                "{symbol}: finding_id {claimed_display} != annotation {truth_ens} \
                 (target is a pseudogene; expression claim not adjudicable)"
            ),
        );
    }

    let claimed_syms = extract_symbol_tokens(claimed_display);
    if !claimed_is_same_family(fam, claimed_ens, &claimed_syms) {
        return (
            MismatchSeverity::Required,
            format!(
                "{symbol}: finding_id {claimed_display} != annotation {truth_ens} \
                 (claimed id is outside {symbol}'s paralog family; cross-gene wrong-binding)"
            ),
        );
    }

    // (a) same family established. Same-family pairs with opposite clinical
    // meaning (SMN1↔SMN2) are genuine wrong-bindings — never downgraded.
    if fam.opposite_clinical_meaning {
        return (
            MismatchSeverity::Required,
            format!(
                "{symbol}: finding_id {claimed_display} != annotation {truth_ens} \
                 (same family but members carry opposite clinical meaning)"
            ),
        );
    }

    // (b) direction/effect concordance is required for the downgrade.
    if !direction_concordant(claimed_ens, truth_ens, concordance_flag, effects) {
        return (
            MismatchSeverity::Required,
            format!(
                "{symbol}: finding_id {claimed_display} != annotation {truth_ens} \
                 (same-family paralog but DE direction/effect is not concordant)"
            ),
        );
    }

    (
        MismatchSeverity::Warn,
        format!(
            "{symbol}: finding_id {claimed_display} bound to a same-family paralog of \
             annotation {truth_ens} (concordant direction, non-pseudogene) — benign \
             segmental-duplication ambiguity, downgraded to a warning (DR-10)"
        ),
    )
}

/// Read an `(ensembl -> effect)` map from the independent annotation table so
/// the paralog direction/effect concordance check (DR-10) has a signal. The
/// accession column comes from the same content-first resolution the annotation
/// loader uses ([`find_accession_column`]), so a DE table keyed on a bare `gene`
/// column still yields a signal instead of silently returning empty. The effect
/// column is the first header present in [`EFFECT_COLUMN_CANDIDATES`]; keys are
/// normalized Ensembl ids. Returns an empty map when the file has no
/// Ensembl+effect pairing (the caller then falls back to `concordance_flag`).
fn load_ensembl_effect_map(path: &Path) -> BTreeMap<String, f64> {
    let mut out: BTreeMap<String, f64> = BTreeMap::new();
    let Some(mut rdr) = open_delimited_table(path) else {
        return out;
    };
    let Ok(headers) = rdr.headers().cloned() else {
        return out;
    };
    let sample = sniff_table_rows(&mut rdr, ENTITY_ROLE_SNIFF_ROWS);
    let Some(ens_idx) = find_accession_column(&headers, &sample) else {
        return out;
    };
    let Some(eff_idx) = headers.iter().position(|h| {
        EFFECT_COLUMN_CANDIDATES
            .iter()
            .any(|n| h.eq_ignore_ascii_case(n))
    }) else {
        return out;
    };
    for rec in sample.iter().cloned().chain(rdr.records().flatten()) {
        let (Some(ens), Some(eff)) = (rec.get(ens_idx), rec.get(eff_idx)) else {
            continue;
        };
        let (Some(ens), Ok(eff)) = (norm_ensembl(ens), eff.trim().parse::<f64>()) else {
            continue;
        };
        out.entry(ens).or_insert(eff);
    }
    out
}

/// Locate the independent entity-label↔Ensembl annotation TABLE (path), shared
/// with [`gene_symbol_ensembl_consistent`] so the paralog effect map is loaded
/// from the same file.
///
/// ROLE SATISFACTION IS THE PRIMARY KEY: a table qualifies iff
/// [`load_symbol_ensembl_map`] builds a non-empty map from it, found by a
/// deterministic sorted scan. A basename is not evidence about a table's
/// columns — preferring one by name put an agent-chosen `ranked_genes.tsv`
/// (whose columns are `symbol`+`stat`, no accession at all, and which no atom
/// declares) ahead of the table that actually carried both roles. The only
/// remaining preference is a CONTENT one, applied among qualifying tables.
/// Relative path of the annotation map declared by
/// `contextualize_findings_with_literature` and `pathway_enrichment`. A
/// declared artifact beats an agent-invented filename because it is the one the
/// exporter is guaranteed to carry — an agent-named table under
/// `intermediates/` exists in the working package but is pruned from every
/// deposit, so preferring it would make the working package and its own deposit
/// disagree about which source produced the verdict.
const CANONICAL_ANNOTATION_RELPATH: &str = "annotation/symbol_map.tsv";

/// Output dir of the stage that writes the claims matrix. A truth source under
/// it is NOT independent of the claim under test.
const CLAIMS_PRODUCER_STAGE_DIR: &str = "contextualize_findings_with_literature";

fn is_canonical_annotation_artifact(path: &Path) -> bool {
    path.to_string_lossy()
        .replace('\\', "/")
        .ends_with(CANONICAL_ANNOTATION_RELPATH)
}

/// True when `path` was produced by the same stage that wrote the claims
/// matrix. Such a table is derived from the same identity join, so a wrong join
/// yields a map that AGREES with the matrix and the obligation passes
/// vacuously — the precise failure mode it exists to catch. Not fatal (it is
/// still better than no adjudication), but it must be disclosed.
fn produced_by_claims_producer(path: &Path) -> bool {
    path.components()
        .any(|c| c.as_os_str() == CLAIMS_PRODUCER_STAGE_DIR)
}

fn find_symbol_ensembl_table(outputs: &Path) -> Option<std::path::PathBuf> {
    let qualifying = scan_for_symbol_ensembl_tables(outputs);
    // Rank among tables that all satisfy the two-role requirement. Independence
    // outranks everything: a map from a stage other than the claims producer can
    // actually falsify the claim. Then the declared canonical artifact, so the
    // working package and its exported deposit resolve the SAME truth source.
    // Then an effect-bearing table, so the paralog concordance check (DR-10)
    // keeps a direction signal. `min_by_key` returns the first of equal minima
    // and the scan is sorted, so the choice is deterministic.
    qualifying
        .iter()
        .min_by_key(|p| {
            (
                u8::from(produced_by_claims_producer(p)),
                u8::from(!is_canonical_annotation_artifact(p)),
                u8::from(!table_carries_effect_signal(p)),
            )
        })
        .cloned()
}

/// The qualifying table to read per-Ensembl effect sizes from, chosen
/// SEPARATELY from the truth source: the declared annotation map deliberately
/// carries no effect column, so binding both roles to one table would silence
/// DR-10 whenever the declared map wins. Never smear a measurement into an
/// annotation table to satisfy one reader.
fn find_ensembl_effect_table(outputs: &Path) -> Option<std::path::PathBuf> {
    scan_for_symbol_ensembl_tables(outputs)
        .into_iter()
        .find(|p| table_carries_effect_signal(p))
}

/// Does this table carry an effect column, i.e. can it feed the DR-10
/// direction/effect concordance signal? Header-only, so ranking candidate
/// tables never re-reads their bodies.
fn table_carries_effect_signal(path: &Path) -> bool {
    let Some(mut rdr) = open_delimited_table(path) else {
        return false;
    };
    let Ok(headers) = rdr.headers() else {
        return false;
    };
    headers.iter().any(|h| {
        EFFECT_COLUMN_CANDIDATES
            .iter()
            .any(|n| h.eq_ignore_ascii_case(n))
    })
}

/// Scan every task output dir — and each of its IMMEDIATE subdirs — for EVERY
/// `.tsv`/`.csv` from which [`load_symbol_ensembl_map`] builds a non-empty
/// label→Ensembl map, returning their PATHS in deterministic (sorted) order so
/// re-reads rank the same tables identically. Empty when no table in the package
/// pairs an entity label with an accession (the validator then soft-skips).
///
/// One subdir level, not a named one: producers write an annotation table under
/// whichever convention they use (`intermediates/`, `annotation/`, …), and
/// enumerating the level is convention-agnostic where a hardcoded subdir name
/// silently misses the table — the same failure mode as hardcoding a basename.
fn scan_for_symbol_ensembl_tables(outputs_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(outputs_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Ok(sub) = std::fs::read_dir(&p) {
                    dirs.extend(sub.flatten().map(|s| s.path()).filter(|s| s.is_dir()));
                }
                dirs.push(p);
            }
        }
    }
    dirs.sort();
    let mut found: Vec<std::path::PathBuf> = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        files.sort();
        for f in files {
            let is_table = f
                .extension()
                .and_then(|x| x.to_str())
                .map(|x| x.eq_ignore_ascii_case("tsv") || x.eq_ignore_ascii_case("csv"))
                .unwrap_or(false);
            // The artifact under test can never be its own truth source: its
            // `finding_id` + `entity` columns DO resolve both roles, so the
            // exclusion has to happen where a path is in hand — the resolver
            // never sees one (`core::entity_columns::CLAIMS_MATRIX_BASENAMES`).
            let is_self = is_claims_matrix_artifact(&f);
            if is_table && !is_self && load_symbol_ensembl_map(&f).is_some() {
                found.push(f);
            }
        }
    }
    found
}

/// The synthetic `validate_*` task dir whose `result.json` carries the
/// gene-symbol obligation verdict for the deposit-readiness domain rollup
/// (DR-2). It is a dedicated dir (not a real DAG task's) so the write is
/// race-free with the contextualize step's own `validate_*` companion, and
/// its `validate_` prefix means `deposit_readiness::scan_domain_validation`
/// already scans it.
pub const GENE_SYMBOL_VALIDATE_TASK: &str = "validate_gene_symbol_ensembl_consistent";

/// Persist the gene-symbol obligation verdict into
/// `runtime/outputs/validate_gene_symbol_ensembl_consistent/result.json` so a
/// REQUIRED failure rolls up into `domain_validation`/`deposit_ready` (DR-2):
/// `scan_domain_validation` reads `validation_passed` + `required_failures`
/// there. Deterministic (sorted, no wall-clock) and idempotent; a clean pass
/// REMOVES any stale verdict so a fixed re-run does not retain an old failure.
/// Best-effort: a write failure is swallowed (the obligation report row is the
/// primary record; this is the rollup bridge).
fn record_gene_symbol_domain_verdict(
    package_root: &Path,
    required_failures: &[String],
    warnings: &[String],
) {
    let dir = package_root
        .join("runtime/outputs")
        .join(GENE_SYMBOL_VALIDATE_TASK);
    let result_path = dir.join("result.json");

    // Clean pass (nothing to record) → drop any stale verdict and return.
    if required_failures.is_empty() && warnings.is_empty() {
        let _ = std::fs::remove_file(&result_path);
        return;
    }
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut req = required_failures.to_vec();
    req.sort();
    req.dedup();
    let mut warn = warnings.to_vec();
    warn.sort();
    warn.dedup();
    let body = serde_json::json!({
        "obligation": "gene_symbol_ensembl_consistent",
        "validation_passed": req.is_empty(),
        "checks_failed": req.len(),
        "required_failures": req,
        "warnings": warn,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&body) {
        let _ = std::fs::write(&result_path, bytes);
    }
}

/// Committed, INDEPENDENT gene-symbol↔Ensembl consistency check. Catches the
/// wrong-gene literature citation the contextualize step produced when an
/// agent-generated script hardcoded a wrong symbol→Ensembl map (CRISPLD2 bound
/// to ENSG00000197142, which is ACSL5). It does NOT trust any map the producer
/// emitted: the ground truth is read from an INDEPENDENT in-package annotation
/// table — ANY other output table that carries both an entity-label column and
/// an Ensembl-accession column, identified by column CONTENT rather than by
/// filename — and the claims matrix's `(entity, finding_id)` pairs are checked
/// against it. The claims matrices themselves are excluded from discovery
/// (`core::entity_columns::CLAIMS_MATRIX_BASENAMES`) so the check can never
/// adjudicate a table against itself.
///
/// DR-10: a disagreement is classified paralog-aware — a benign same-family
/// segmental-duplication paralog (concordant direction, non-pseudogene,
/// non-opposite-clinical-meaning) is downgraded to a WARNING; unrelated-locus
/// cross-gene wrong-bindings, pseudogene targets, opposite-clinical-meaning
/// pairs, and direction-discordant same-family pairs stay REQUIRED failures.
/// DR-2: the verdict is also written into a `validate_*` `result.json` so a
/// required failure rolls up into deposit-readiness `domain_validation`.
///
/// Returns:
///   - [`ValidatorOutcome::Failed`] listing each mismatched
///     `(gene_symbol: finding_id != truth)` when any pair disagrees;
///   - [`ValidatorOutcome::Errored`] (the soft-pass / "could not run" variant the
///     sibling validators use — `has_failures()` does not count it) when no
///     independent annotation source exists in the package, or the claims matrix
///     is absent/unreadable;
///   - [`ValidatorOutcome::Passed`] when every pair is consistent.
pub fn gene_symbol_ensembl_consistent(package_root: &Path) -> ValidatorOutcome {
    let outputs = package_root.join("runtime/outputs");

    // (1) INDEPENDENT truth source: ANY in-package table that pairs an entity
    // label with an Ensembl accession. Both the filename and the header names
    // drift across runs, and the pairing may live in the DE results table, a
    // ranking, or a dedicated annotation table — so discovery is a deterministic
    // sorted scan keyed on what the COLUMNS hold, never on a basename
    // (`find_symbol_ensembl_table`). Robust to filename and header drift without
    // ever fabricating a truth source.
    let Some(truth_path) = find_symbol_ensembl_table(&outputs) else {
        // No table in the package pairs a label with an Ensembl accession, so
        // there is no INDEPENDENT annotation to adjudicate against. Soft-skip
        // (Errored is non-blocking) rather than fabricate a verdict.
        return ValidatorOutcome::Errored {
            reason: "no independent entity-label↔Ensembl annotation table in package \
                     (no output table carries both an entity-label column and an \
                     Ensembl-accession column)"
                .into(),
        };
    };
    let Some(truth) = load_symbol_ensembl_map(&truth_path) else {
        return ValidatorOutcome::Errored {
            reason: "no independent entity-label↔Ensembl annotation table in package \
                     (no output table carries both an entity-label column and an \
                     Ensembl-accession column)"
                .into(),
        };
    };
    // Per-Ensembl DE effect for the paralog direction/effect concordance check
    // (DR-10), from whichever qualifying table carries an effect column — not
    // necessarily the truth source, since the declared annotation map carries
    // none. Empty when no qualifying table has one; the classifier then falls
    // back to the claims-row concordance_flag.
    let effects = find_ensembl_effect_table(&outputs)
        .map(|p| load_ensembl_effect_map(&p))
        .unwrap_or_default();

    // (2) The claims matrix the contextualize step emitted.
    let claims_csv =
        outputs.join("contextualize_findings_with_literature/claims_evidence_matrix.csv");
    let mut rdr = match csv::Reader::from_path(&claims_csv) {
        Ok(r) => r,
        Err(e) => {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "claims_evidence_matrix.csv unreadable at {}: {e}",
                    claims_csv.display()
                ),
            };
        }
    };
    let headers = match rdr.headers() {
        Ok(h) => h.clone(),
        Err(e) => {
            return ValidatorOutcome::Errored {
                reason: format!("claims_evidence_matrix.csv header unreadable: {e}"),
            };
        }
    };
    // Symbol column: the canonical claims schema keys the gene on `entity`
    // (with entity_kind == gene); older/agent-drifted matrices may use
    // `gene_symbol`/`symbol`/`gene`. Accept any of them. A non-gene `entity`
    // (e.g. a pathway) simply won't be present in the gene truth map and is
    // skipped below, so widening the column match never produces false
    // mismatches.
    let sym_idx = headers.iter().position(|h| {
        ["gene_symbol", "entity", "symbol", "gene", "gene_name"]
            .iter()
            .any(|n| h.eq_ignore_ascii_case(n))
    });
    let fid_idx = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("finding_id"));
    // Optional per-row DE-direction signal for the paralog concordance check
    // (DR-10) when the annotation table carries no effect column.
    let flag_idx = headers
        .iter()
        .position(|h| h.eq_ignore_ascii_case("concordance_flag"));
    // Without a symbol + finding_id pairing there is nothing to cross-check;
    // soft-skip (Errored is non-blocking) rather than fail.
    let (Some(sym_idx), Some(fid_idx)) = (sym_idx, fid_idx) else {
        return ValidatorOutcome::Errored {
            reason:
                "claims_evidence_matrix.csv has no symbol (entity/gene_symbol)+finding_id columns to cross-check"
                    .into(),
        };
    };

    // (3) Compare every non-empty gene_symbol's finding_id to the truth
    // Ensembl, classifying each disagreement paralog-aware (DR-10) into a
    // benign same-family WARNING vs a REQUIRED failure.
    let mut required: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    // Disclose a non-independent truth source rather than recording a silent
    // clean pass: adjudicating the claims matrix against a table the SAME stage
    // derived from the SAME identity join cannot falsify a wrong join.
    if produced_by_claims_producer(&truth_path) {
        warnings.push(format!(
            "truth source not independent of the claims-matrix producer: {}",
            truth_path
                .strip_prefix(&outputs)
                .unwrap_or(&truth_path)
                .display()
        ));
    }
    for rec in rdr.records().flatten() {
        let Some(symbol) = rec.get(sym_idx).map(str::trim) else {
            continue;
        };
        // Skip unresolved-symbol rows ("NA", empty, …): an unresolved locus has
        // no real symbol↔Ensembl binding to adjudicate, and several NA rows
        // would otherwise all mismatch the first NA→Ensembl in the truth map.
        if is_unresolved_gene_symbol(symbol) {
            continue;
        }
        let finding_id = rec.get(fid_idx).map(str::trim).unwrap_or("");
        let concordance_flag = flag_idx.and_then(|i| rec.get(i)).map(str::trim);
        if let Some(truth_ens) = truth.get(symbol) {
            // Compare on the normalized Ensembl accession so a composite
            // finding_id (`DE_CRISPLD2_ENSG…`) or a version suffix does not
            // register a spurious mismatch; fall back to a raw compare only
            // when the finding_id carries no Ensembl accession at all.
            let claimed_norm = norm_ensembl(finding_id);
            let truth_norm = norm_ensembl(truth_ens).unwrap_or_else(|| truth_ens.to_string());
            let is_mismatch = match &claimed_norm {
                Some(c) => c != &truth_norm,
                None => finding_id != truth_ens,
            };
            if is_mismatch {
                // Display the RAW finding_id (it may encode a paralog SYMBOL,
                // e.g. `DE_LRRC37A2_ENSG…`, that the family match keys on) while
                // comparing on the normalized accession.
                let (severity, reason) = classify_mismatch(
                    symbol,
                    finding_id,
                    claimed_norm.as_deref(),
                    &truth_norm,
                    concordance_flag,
                    &effects,
                );
                match severity {
                    MismatchSeverity::Required => required.push(reason),
                    MismatchSeverity::Warn => warnings.push(reason),
                }
            }
        }
        // A symbol absent from the independent annotation is not adjudicable
        // here (the truth source may not cover every DE gene); the row's
        // upstream-PK presence is checked by claim_row_has_finding_id.
    }

    // DR-2: bridge the verdict into the deposit-readiness domain rollup.
    record_gene_symbol_domain_verdict(package_root, &required, &warnings);

    if required.is_empty() {
        // Benign paralog ambiguities (if any) were recorded as warnings; the
        // obligation itself does not block.
        ValidatorOutcome::Passed
    } else {
        ValidatorOutcome::Failed {
            message: format!(
                "{} required gene-symbol↔Ensembl mismatch(es) vs independent annotation: {}{}",
                required.len(),
                required.join("; "),
                if warnings.is_empty() {
                    String::new()
                } else {
                    format!(
                        " | {} benign paralog warning(s) (DR-10): {}",
                        warnings.len(),
                        warnings.join("; ")
                    )
                }
            ),
        }
    }
}

// ============================================================================
// ValidatorRunner trait wrappers (Phase D — wire into post-task dispatch)
// ============================================================================
//
// The harness post-task hook (crates/harness/src/main.rs around line 1799)
// dispatches obligations via the ValidatorRunner trait. Wrap each pure-fn
// runner in a trait impl so it can join `default_runners()`. Each wrapper
// (a) builds the csv + manifest paths from the task's artifact_path, (b)
// calls the pure fn, (c) converts the Result into a ValidatorOutcome. The
// structured ValidationFailureCause::LiteratureClaim payload is encoded
// into the failure `message` string as a JSON-serialized fragment so
// downstream consumers (verify endpoint, UI) can recover the typed cause
// when needed. The plan-level intent of attaching the structured cause to
// BlockerKind::ValidationFailed { cause } is reached via the /verify
// endpoint in a later task — the harness-side dispatcher today uses a
// string-only TaskState::Blocked path (see main.rs:1812-1826).

use crate::validators::{ValidatorOutcome, ValidatorRunner};

fn find_literature_csv(artifact_path: &Path) -> Option<std::path::PathBuf> {
    let prior = artifact_path.join("prior_claims_matrix.csv");
    if prior.exists() {
        return Some(prior);
    }
    let claims = artifact_path.join("claims_evidence_matrix.csv");
    if claims.exists() {
        return Some(claims);
    }
    // The survey_method_landscape atom emits method_landscape.csv; the
    // method-landscape validators (claim_support_satisfied, doc_page_matches_tool)
    // resolve their input here.
    let landscape = artifact_path.join("method_landscape.csv");
    if landscape.exists() {
        return Some(landscape);
    }
    None
}

fn cause_to_message<C: Serialize>(cause: &C) -> String {
    serde_json::to_string(cause).unwrap_or_else(|e| format!("cause_serialize_error:{}", e))
}

/// Render a failure cause as the `row N: <json>` validator message.
fn failure_message(cause: LiteratureFailureCause) -> ValidatorOutcome {
    ValidatorOutcome::Failed {
        message: format!("row {}: {}", cause.row_index(), cause_to_message(&cause)),
    }
}

/// Parse-probe the claims table for the obligations whose runner reads it via
/// `load_rows`, so an unparseable table reports `table_parse_error` (naming the
/// row and column) rather than the row-0 `evidence_artifact_missing` the closed
/// core cause type forces on those runners. Returns `None` when the table parses
/// (or when this obligation does not read it that way).
fn claims_table_parse_outcome(csv: &Path, reads_claims_table: bool) -> Option<ValidatorOutcome> {
    if !reads_claims_table {
        return None;
    }
    probe_claims_table(csv).err().map(failure_message)
}

/// `reads_claims_table` marks the obligations whose runner deserializes the
/// whole table through `load_rows`. Scoped rather than unconditional: the
/// header-name readers (`source_resolves`, `claim_support_satisfied`,
/// `doc_page_matches_tool`) legitimately evaluate a table that `ClaimsMatrixRow`
/// cannot deserialize, and must not start failing on a column they never read.
fn runner_dispatch<F>(
    artifact_path: &Path,
    require_manifest: bool,
    reads_claims_table: bool,
    run: F,
) -> ValidatorOutcome
where
    F: FnOnce(&Path, &Path) -> Result<(), (u64, ValidationFailureCause)>,
{
    let Some(csv) = find_literature_csv(artifact_path) else {
        return ValidatorOutcome::Errored {
            reason: format!(
                "no literature CSV at {} (looked for prior_claims_matrix.csv and claims_evidence_matrix.csv)",
                artifact_path.display()
            ),
        };
    };
    let manifest = artifact_path.join("evidence/manifest.json");
    if require_manifest && !manifest.exists() {
        return ValidatorOutcome::Errored {
            reason: format!("evidence/manifest.json missing at {}", manifest.display()),
        };
    }
    if let Some(parse_failure) = claims_table_parse_outcome(&csv, reads_claims_table) {
        return parse_failure;
    }
    match run(&csv, &manifest) {
        Ok(()) => ValidatorOutcome::Passed,
        Err((_row_index, cause)) => failure_message(cause.into()),
    }
}

/// `ValidatorRunner` wrapping `run_pmid_resolves` for the `pmid_resolves` obligation.
pub struct PmidResolvesRunner;
impl ValidatorRunner for PmidResolvesRunner {
    fn obligation_id(&self) -> &'static str {
        "pmid_resolves"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, true, run_pmid_resolves)
    }
}

/// `ValidatorRunner` wrapping `run_source_resolves` for the `source_resolves`
/// obligation — the locator-generalized successor to `pmid_resolves`.
pub struct SourceResolvesRunner;
impl ValidatorRunner for SourceResolvesRunner {
    fn obligation_id(&self) -> &'static str {
        "source_resolves"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, false, run_source_resolves)
    }
}

/// `ValidatorRunner` wrapping `run_evidence_quote_substring_match` for the `evidence_quote_substring_match` obligation.
pub struct EvidenceQuoteSubstringMatchRunner;
impl ValidatorRunner for EvidenceQuoteSubstringMatchRunner {
    fn obligation_id(&self) -> &'static str {
        "evidence_quote_substring_match"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(
            artifact_path,
            true,
            true,
            run_evidence_quote_substring_match,
        )
    }
}

/// `ValidatorRunner` wrapping `run_redistributable_or_marked` for the `redistributable_or_marked` obligation.
pub struct RedistributableOrMarkedRunner;
impl ValidatorRunner for RedistributableOrMarkedRunner {
    fn obligation_id(&self) -> &'static str {
        "redistributable_or_marked"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, true, run_redistributable_or_marked)
    }
}

/// `ValidatorRunner` wrapping `run_claim_row_has_finding_id` for the `claim_row_has_finding_id` obligation.
pub struct ClaimRowHasFindingIdRunner;
impl ValidatorRunner for ClaimRowHasFindingIdRunner {
    fn obligation_id(&self) -> &'static str {
        "claim_row_has_finding_id"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        // This validator needs the upstream findings CSV, which
        // isn't yet plumbed through the ValidatorRunner trait signature
        // (it only receives the current task's artifact_path). Look for a
        // sibling findings CSV under runtime/outputs/<upstream>/ relative
        // to artifact_path; if not findable, soft-skip with Errored so
        // failures don't block tasks pending upstream-path threading.
        let Some(csv) = find_literature_csv(artifact_path) else {
            return ValidatorOutcome::Errored {
                reason: format!(
                    "no claims_evidence_matrix.csv at {}",
                    artifact_path.display()
                ),
            };
        };
        // Heuristic upstream-finding paths: look in sibling output dirs
        // for canonical finding-table filenames.
        let outputs_dir = artifact_path.parent();
        let Some(outputs_dir) = outputs_dir else {
            return ValidatorOutcome::Errored {
                reason: "artifact_path has no parent outputs dir".into(),
            };
        };
        let candidates = ["de_results.tsv", "peak_calls.tsv", "variant_calls.tsv"];
        let findings_csv = std::fs::read_dir(outputs_dir).ok().and_then(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .find_map(|sib| {
                    candidates.iter().find_map(|name| {
                        let p = sib.join(name);
                        if p.exists() {
                            Some(p)
                        } else {
                            None
                        }
                    })
                })
        });
        let Some(findings_csv) = findings_csv else {
            // No upstream findings table in a recognized shape. Genomics/transcriptomics
            // analyses emit de_results/peak_calls/variant_calls; metabolomics &
            // generic-omics emit findings under task-specific names (and use
            // task-local finding_ids that don't cross-reference an upstream PK at
            // all). The finding_id↔upstream-PK obligation is N/A there, so SKIP
            // (Passed) rather than Errored — an Errored here was counted as a
            // blocking failure and spuriously stranded the terminal on every
            // non-genomics modality. Asserted concordances remain backed by the
            // manifest-based evidence validators.
            return ValidatorOutcome::Passed;
        };
        // Same honest-parse-failure probe the shared `runner_dispatch` applies:
        // this runner reads the claims table through `load_rows` too.
        if let Some(parse_failure) = claims_table_parse_outcome(&csv, true) {
            return parse_failure;
        }
        match run_claim_row_has_finding_id(&csv, &findings_csv) {
            Ok(()) => ValidatorOutcome::Passed,
            Err((_row_index, cause)) => failure_message(cause.into()),
        }
    }
}

/// `ValidatorRunner` wrapping `run_claim_support_satisfied` for the
/// `claim_support_satisfied` obligation. Reads the corroboration policy from
/// the package (not the evidence manifest), so the manifest is not required.
pub struct ClaimSupportSatisfiedRunner;
impl ValidatorRunner for ClaimSupportSatisfiedRunner {
    fn obligation_id(&self) -> &'static str {
        "claim_support_satisfied"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, false, run_claim_support_satisfied)
    }
}

/// `ValidatorRunner` wrapping `run_doc_page_matches_tool` for the
/// `doc_page_matches_tool` obligation. Requires the evidence manifest to
/// resolve each tool-doc snapshot.
pub struct DocPageMatchesToolRunner;
impl ValidatorRunner for DocPageMatchesToolRunner {
    fn obligation_id(&self) -> &'static str {
        "doc_page_matches_tool"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, true, false, run_doc_page_matches_tool)
    }
}

/// `ValidatorRunner` wrapping `run_method_quote_mentions_candidate`.
pub struct MethodQuoteMentionsCandidateRunner;
impl ValidatorRunner for MethodQuoteMentionsCandidateRunner {
    fn obligation_id(&self) -> &'static str {
        "method_quote_mentions_candidate"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(
            artifact_path,
            false,
            false,
            run_method_quote_mentions_candidate,
        )
    }
}

/// `ValidatorRunner` wrapping `run_concordance_flag_in_closed_set` for the `concordance_flag_in_closed_set` obligation.
pub struct ConcordanceFlagInClosedSetRunner;
impl ValidatorRunner for ConcordanceFlagInClosedSetRunner {
    fn obligation_id(&self) -> &'static str {
        "concordance_flag_in_closed_set"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(
            artifact_path,
            false,
            true,
            run_concordance_flag_in_closed_set,
        )
    }
}

/// `ValidatorRunner` wrapping `run_direction_supported_by_quote` for the
/// `direction_supported_by_quote` obligation.
pub struct DirectionSupportedByQuoteRunner;
impl ValidatorRunner for DirectionSupportedByQuoteRunner {
    fn obligation_id(&self) -> &'static str {
        "direction_supported_by_quote"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        runner_dispatch(artifact_path, false, true, run_direction_supported_by_quote)
    }
}

/// Trait-wrapped runners for the literature obligations. Used by
/// `ValidatorRunner` for the `gene_symbol_ensembl_consistent` obligation
/// (Workstream B). Unlike the other literature runners, the underlying check
/// needs the PACKAGE ROOT (not just this task's artifact dir) because it reads
/// an INDEPENDENT truth table from a different task's output
/// (`pathway_enrichment/intermediates/ranked_genes.tsv`) and compares it to the
/// contextualize step's `claims_evidence_matrix.csv`. The artifact path the
/// harness passes is `<root>/runtime/outputs/<task_id>`, so we locate the
/// package root by walking ancestors until we find the `runtime/outputs` dir
/// (robust to depth changes), then delegate to the pure checker.
pub struct GeneSymbolEnsemblConsistentRunner;
impl ValidatorRunner for GeneSymbolEnsemblConsistentRunner {
    fn obligation_id(&self) -> &'static str {
        "gene_symbol_ensembl_consistent"
    }
    fn run(&self, artifact_path: &Path) -> ValidatorOutcome {
        match artifact_path
            .ancestors()
            .find(|p| p.join("runtime").join("outputs").is_dir())
        {
            Some(root) => gene_symbol_ensembl_consistent(root),
            None => ValidatorOutcome::Errored {
                reason: format!(
                    "cannot locate package root (no runtime/outputs ancestor) from {}",
                    artifact_path.display()
                ),
            },
        }
    }
}

/// `crate::validators::default_runners` so the harness post-task hook
/// routes literature obligation ids to the right runner.
pub fn literature_runners() -> Vec<Box<dyn ValidatorRunner>> {
    vec![
        Box::new(PmidResolvesRunner) as Box<dyn ValidatorRunner>,
        Box::new(SourceResolvesRunner),
        Box::new(EvidenceQuoteSubstringMatchRunner),
        Box::new(RedistributableOrMarkedRunner),
        Box::new(ClaimRowHasFindingIdRunner),
        Box::new(ConcordanceFlagInClosedSetRunner),
        Box::new(DirectionSupportedByQuoteRunner),
        Box::new(ClaimSupportSatisfiedRunner),
        Box::new(DocPageMatchesToolRunner),
        Box::new(MethodQuoteMentionsCandidateRunner),
        Box::new(GeneSymbolEnsemblConsistentRunner),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    // Exercised only from the tests below (the resolver that consumes it now
    // lives in core), so it is imported here rather than module-wide.
    use ecaa_workflow_core::entity_columns::looks_like_ensembl_accession;
    use tempfile::TempDir;

    fn write(p: &Path, s: &str) {
        fs::write(p, s).unwrap();
    }

    /// Drive the real `deserialize_with` hook (not just its accept-set helper)
    /// the way serde calls it: from a string cell.
    fn de_bool(cell: &str) -> Result<bool, String> {
        #[derive(Deserialize)]
        struct BoolCell {
            #[serde(deserialize_with = "de_bool_lenient")]
            v: bool,
        }
        serde_json::from_value::<BoolCell>(serde_json::json!({ "v": cell }))
            .map(|c| c.v)
            .map_err(|e| e.to_string())
    }

    /// A cell that says "no value here" is ABSENT, not malformed. The
    /// contextualize step wrote `redistributable=NA` on 4029 `not_assessed`
    /// rows; rejecting `NA` failed the whole CSV parse and stranded six REQUIRED
    /// obligations on a table that had nothing wrong with it. A genuinely
    /// malformed value must still be an error — this is a sentinel set, not a
    /// blanket "anything unparseable is false".
    #[test]
    fn de_bool_lenient_accepts_na_family() {
        for s in [
            "NA", "na", "N/A", "n/a", "NaN", "nan", "NULL", "null", "None", "none", "-", ".", "?",
            " NA ",
        ] {
            assert_eq!(
                de_bool(s),
                Ok(false),
                "{s:?} is an absent-value sentinel and must read as absent (false)"
            );
        }
        // Real values are unchanged.
        for s in ["true", "True", "TRUE", "1"] {
            assert_eq!(de_bool(s), Ok(true), "{s:?} must still read as true");
        }
        for s in ["false", "False", "FALSE", "0", ""] {
            assert_eq!(de_bool(s), Ok(false), "{s:?} must still read as false");
        }
        // A malformed value is still an error — absence is a closed set.
        for s in ["maybe", "yes", "2", "tru"] {
            let err = de_bool(s).expect_err("a malformed bool cell must still error");
            assert!(
                err.contains("invalid bool literal"),
                "{s:?} must fail as a malformed bool, got: {err}"
            );
        }
    }

    /// Same absent-value contract on the numeric column: an `NA` offset is a
    /// cell with no value, not a broken table.
    #[test]
    fn de_u64_lenient_accepts_na_family() {
        for s in ["NA", "n/a", "NaN", "null", "None", "-", ".", "?", ""] {
            assert_eq!(
                parse_u64_lenient(s),
                Some(0),
                "{s:?} is an absent-value sentinel and must read as 0"
            );
        }
        assert_eq!(
            parse_u64_lenient("42"),
            Some(42),
            "real values are unchanged"
        );
        assert_eq!(
            parse_u64_lenient("twelve"),
            None,
            "a malformed offset must still be an error"
        );
    }

    #[test]
    fn unresolved_word_markers_are_recognized_as_unresolved_symbols() {
        // NA-family sentinels (existing coverage).
        for s in ["NA", "n/a", "NaN", "null", "None", "-", ".", "?", "", "  "] {
            assert!(
                is_unresolved_gene_symbol(s),
                "{s:?} must read as unresolved"
            );
        }
        // Word-form "no symbol resolved" placeholders — the class that regressed
        // himes (`UNRESOLVED` → false cross-gene wrong-binding). Case-insensitive.
        for s in [
            "UNRESOLVED",
            "unresolved",
            "Unmapped",
            "UNKNOWN",
            "unassigned",
        ] {
            assert!(
                is_unresolved_gene_symbol(s),
                "{s:?} must read as unresolved"
            );
        }
        // Real gene symbols are NOT treated as unresolved.
        for s in ["CRISPLD2", "TP53", "MYH16", "DUSP1"] {
            assert!(!is_unresolved_gene_symbol(s), "{s:?} is a real symbol");
        }
    }

    #[test]
    fn redistributable_accepts_v2_source_kinds_but_keeps_legal_gate() {
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json"); // unused by this runner
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let run = |source_kind: &str, redist: &str| {
            let csv = dir.path().join("m.csv");
            write(
                &csv,
                &format!("{hdr}\nACAN,gene,28123456,foo,0,{source_kind},sha256:abc,2026-05-14T00:00:00Z,{redist},true\n"),
            );
            run_redistributable_or_marked(&csv, &manifest)
        };
        // v2 paper-class OA records (incl. OpenAlex/Crossref-surfaced) marked
        // redistributable are accepted — previously fell through to a spurious FAIL.
        assert!(run("openalex", "true").is_ok());
        assert!(run("crossref", "true").is_ok());
        assert!(run("pmc_oa_full_text", "true").is_ok());
        assert!(run("abstract_only", "true").is_ok());
        // Tool-documentation pages: either marking is legal.
        assert!(run("doc_page", "false").is_ok());
        assert!(run("doc_page", "true").is_ok());
        // LEGAL GATE preserved: external local PDFs must NOT claim redistribution.
        assert!(run("external_pdf_local_only", "true").is_err());
        assert!(run("external_pdf_local_only", "false").is_ok());
        // The legal gate rejects any unmarked class whose license is not
        // determined by the class name.
        assert!(run("openalex", "false").is_err());
        assert!(run("crossref", "false").is_err());
        // An explicit PMC OA class is redistributable by class. PubMed
        // abstracts still require an explicit row or manifest marker because
        // NLM delivery does not determine their copyright.
        assert!(run("pmc_oa_full_text", "false").is_ok());
        assert!(run("pubmed_abstract", "false").is_err());
    }

    #[test]
    fn redistributable_accepts_pubmed_efetch_batch() {
        // A `pubmed_efetch_xml_batch` row carrying the helper's explicit
        // redistribution marker is consistent.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json"); // unused by this runner
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let csv = dir.path().join("m.csv");
        write(&csv, &format!("{hdr}\nMaxQuant,method,19029910,foo,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,true,true\n"));
        assert!(run_redistributable_or_marked(&csv, &manifest).is_ok());
        // An unmarked PubMed source fails closed. PubMed does not own
        // publisher-supplied abstract copyright.
        write(&csv, &format!("{hdr}\nMaxQuant,method,19029910,foo,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,false,true\n"));
        assert!(run_redistributable_or_marked(&csv, &manifest).is_err());
    }

    #[test]
    fn claim_row_finding_id_reads_tab_separated_findings_and_normalizes_prefix() {
        // The contextualize atom keys claim rows by `finding_id` (e.g.
        // `DE_FBgn0000043`) / `entity_id` against the upstream DE findings file,
        // which is TAB-separated `de_results.tsv` with a `gene_id` PK. Reading
        // the TSV with the comma default collapsed every row into one field, so
        // the bare gene id never matched and every row orphaned. The reader must
        // sniff the .tsv delimiter, and the finding_id must resolve via the
        // `DE_`-prefix-stripped form or the row's entity_id.
        let dir = TempDir::new().unwrap();
        let findings = dir.path().join("de_results.tsv");
        write(
            &findings,
            "gene_id\tbaseMean\tlog2FoldChange\tpadj\nFBgn0000043\t1.0\t2.0\t0.01\n",
        );
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(
            &csv,
            "finding_id,entity_id,pmid,evidence_quote,source_kind,concordance_flag,redistributable,verified\n\
             DE_FBgn0000043,FBgn0000043,,,,no_prior_finding,,true\n",
        );
        assert!(
            run_claim_row_has_finding_id(&csv, &findings).is_ok(),
            "DE_-prefixed finding_id must resolve against the TSV gene_id PK"
        );
    }

    #[test]
    fn validators_accept_claude_contextualize_schema() {
        // The exact contextualize_findings_with_literature schema claude emits
        // (captured from a live pasilla run): claims CSV uses `concordance` (not
        // concordance_flag), `prior_pmids` PIPE-delimited (not a JSON Vec),
        // `evidence_quote_excerpt` (not evidence_quote), with 852 no_prior_finding
        // rows + 1 `unverifiable` row citing 6 PMIDs; and a SUMMARY manifest with
        // NO `entries`/`sources` field (contextualize reuses upstream claims, no
        // new fetch). Every obligation must resolve it: pipe-split prior_pmids,
        // column aliases, the entries-default manifest, and skipping the
        // non-asserting unverifiable row (which makes no concordance claim).
        let dir = TempDir::new().unwrap();
        let findings = dir.path().join("de_results.tsv");
        write(&findings, "gene_id\tbaseMean\tlog2FoldChange\tpadj\nFBgn0039155\t730\t-4.6\t1e-159\nFBgn0024288\t50\t0.3\t0.9\n");
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(
            &csv,
            "finding_id,baseMean,log2FoldChange,padj,concordance,prior_pmids,prior_axes,evidence_quote_excerpt\n\
             FBgn0039155,730,-4.6,1e-159,no_prior_finding,,,\n\
             FBgn0024288,50,0.3,0.9,unverifiable,20921232|22279750|22623672,splicing,Alternative splicing is generally controlled by proteins\n",
        );
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        // Summary manifest: NO entries/sources field.
        write(
            &manifest,
            r#"{"task_id":"contextualize_findings_with_literature","n_findings":2,"concordance_summary":{"no_prior_finding":1,"unverifiable":1}}"#,
        );
        for (name, ok) in [
            ("pmid_resolves", run_pmid_resolves(&csv, &manifest).is_ok()),
            (
                "evidence_quote_substring_match",
                run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            ),
            (
                "redistributable_or_marked",
                run_redistributable_or_marked(&csv, &manifest).is_ok(),
            ),
            (
                "concordance_flag_in_closed_set",
                run_concordance_flag_in_closed_set(&csv, &manifest).is_ok(),
            ),
            (
                "claim_row_has_finding_id",
                run_claim_row_has_finding_id(&csv, &findings).is_ok(),
            ),
        ] {
            assert!(ok, "{name} must pass on claude's contextualize schema (pipe prior_pmids, evidence_quote_excerpt, unverifiable row, entries-less manifest)");
        }
    }

    #[test]
    fn redistributable_skips_source_less_no_prior_finding_rows() {
        // A `no_prior_finding` concordance row carries no source by definition
        // (no PMID matched for the entity) → empty source_kind / redistributable.
        // The legal gate must skip it, not fail it.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(
            &csv,
            "finding_id,entity_id,pmid,evidence_quote,source_ref_kind,source_kind,concordance_flag,redistributable,verified\n\
             DE_X,X,,,,,no_prior_finding,,true\n",
        );
        assert!(
            run_redistributable_or_marked(&csv, &manifest).is_ok(),
            "source-less no_prior_finding row must be skipped by the legal gate"
        );
    }

    #[test]
    fn evidence_quote_substring_match_resolves_batched_efetch_manifest() {
        // Batched PubMed efetch: one XML snapshot covers many PMIDs, exposed via
        // `pmids_in_batch` (no singular pmid/source_ref field). A claim row citing
        // a PMID in the batch must resolve to that snapshot — previously reported
        // PmidNotFound because the locator index only read pmid/source_ref.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nMaxQuant,method,19029910,enables high peptide identification,0,pubmed_efetch_xml_batch,sha256:abc,2026-06-08T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":2,"entries":[{"pmids_in_batch":["19029910","30656827"],"source_kind":"pubmed_efetch_xml_batch","path":"snap.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-06-08T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"abstract_fair_use"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("snap.xml"),
            "MaxQuant enables high peptide identification rates, individualized ppb mass accuracies",
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "a PMID present only via pmids_in_batch must resolve to its batch snapshot"
        );
    }

    #[test]
    fn resolve_evidence_file_follows_cross_task_sibling_path() {
        // contextualize_findings_with_literature dedups by reusing snapshots,
        // recording manifest paths like
        // `../review_prior_work/evidence/snapshots/<hash>`. After the H10 re-jail
        // the `../`-traversing form is REJECTED by the entry-safety gate, but the
        // snapshot is still reachable in-jail via the basename fallback over the
        // current task's evidence subtree (snapshots/<hash>), which stays under
        // runtime/outputs.
        let root = TempDir::new().unwrap();
        let outputs = root.path().join("runtime/outputs");
        let ctx_ev = outputs.join("contextualize_findings_with_literature/evidence");
        let ctx_snap = ctx_ev.join("snapshots");
        fs::create_dir_all(&ctx_snap).unwrap();
        fs::write(ctx_snap.join("abc123"), "abstract text").unwrap();
        // Entry path carries the cross-task `../` prefix; the resolver rejects
        // the traversal but recovers the snapshot by basename inside the jail.
        let resolved = resolve_evidence_file(
            root.path(),
            &ctx_ev,
            "../review_prior_work/evidence/snapshots/abc123",
        );
        assert!(
            resolved.exists() && resolved.starts_with(&outputs),
            "cross-task snapshot must resolve in-jail via basename fallback; got {}",
            resolved.display()
        );
    }

    #[test]
    fn resolve_evidence_file_handles_package_root_relative_and_nested_paths() {
        // codex writes a PACKAGE-ROOT-relative source_text_path
        // ("runtime/outputs/review_prior_work/evidence/sources/PMID_X.txt") and
        // nests the file under evidence/sources/. After the H10 re-jail the
        // ancestor-walk is gone; the file must still resolve via the basename
        // fallback (sources/<base>), in-jail.
        let root = TempDir::new().unwrap();
        let ev = root
            .path()
            .join("runtime/outputs/review_prior_work/evidence");
        fs::create_dir_all(ev.join("sources")).unwrap();
        fs::write(ev.join("sources/PMID_20921232.txt"), "abstract text").unwrap();
        // Package-root-relative path (resolves via the sources/ basename fallback).
        let r1 = resolve_evidence_file(
            root.path(),
            &ev,
            "runtime/outputs/review_prior_work/evidence/sources/PMID_20921232.txt",
        );
        assert!(
            r1.exists(),
            "package-root-relative path must resolve; got {}",
            r1.display()
        );
        // A bare basename / odd prefix resolves via the sources/ basename fallback.
        let r2 = resolve_evidence_file(root.path(), &ev, "weird/prefix/PMID_20921232.txt");
        assert!(
            r2.exists(),
            "basename fallback must find the nested file; got {}",
            r2.display()
        );
    }

    #[test]
    fn resolve_evidence_file_jails_to_package_root() {
        // H10: the resolver must not let a traversal entry escape the package's
        // runtime/outputs subtree, while a legitimate in-jail nested source
        // still resolves.
        use std::fs;
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let ev = root.join("runtime/outputs/t1/evidence");
        fs::create_dir_all(ev.join("sources")).unwrap();
        fs::write(ev.join("sources/PMID_1.txt"), b"x").unwrap();

        // (a) Traversal to a real file outside the package must NOT resolve to
        // an out-of-jail path. `etc/passwd` has no `..`, but its basename
        // (`passwd`) is not under the evidence subtree, so nothing resolves and
        // the resolver returns the (non-existent) direct join — never an
        // out-of-jail real file.
        let escaped = resolve_evidence_file(root, &ev, "etc/passwd");
        assert!(
            !escaped.exists() || escaped.starts_with(root.join("runtime/outputs")),
            "traversal entry must not reach files outside the package root; got {}",
            escaped.display()
        );

        // (a') An explicit `..`-bearing absolute-ish escape attempt is rejected
        // by the entry-safety gate and the jail prefix-check; even if such a
        // file exists on the host it must not be returned.
        let outside = root.join("secret.txt");
        fs::write(&outside, b"secret").unwrap();
        let escaped2 = resolve_evidence_file(root, &ev, "../../../secret.txt");
        assert!(
            !escaped2.exists() || escaped2.starts_with(root.join("runtime/outputs")),
            "`..`-traversal must be rejected; got {}",
            escaped2.display()
        );

        // (b) Legitimate nested source still resolves (basename fallback,
        // in-jail).
        let ok = resolve_evidence_file(root, &ev, "evidence/sources/PMID_1.txt");
        assert!(
            ok.exists() && ok.starts_with(root.join("runtime/outputs")),
            "in-jail evidence must still resolve; got {}",
            ok.display()
        );
    }

    #[test]
    fn redistributable_match_is_anchored_not_substring() {
        // M8: the legal gate must anchor on the source_kind class PREFIX, not
        // match the token anywhere in the string. Only an explicit PMC OA class
        // is inherently redistributable.
        assert!(source_kind_is_inherently_redistributable(
            "pmc_oa_full_text"
        ));
        assert!(!source_kind_is_inherently_redistributable(
            "pmc_front_or_abstract_xml_only"
        ));
        assert!(!source_kind_is_inherently_redistributable(
            "pubmed_abstract_with_pmc_front_xml_checked"
        ));
        assert!(!source_kind_is_inherently_redistributable("pubmed_efetch"));
        // Substring false-positives must NOT pass (the bug: unanchored
        // contains("pmc")/contains("pubmed")).
        assert!(!source_kind_is_inherently_redistributable(
            "camphor_db_export"
        ));
        assert!(!source_kind_is_inherently_redistributable(
            "campusing_corpus"
        ));
        assert!(!source_kind_is_inherently_redistributable(
            "external_pdf_local_only"
        ));
        assert!(!source_kind_is_inherently_redistributable("openalex"));
    }

    #[test]
    fn validators_accept_codex_sources_manifest_schema() {
        // Codex hand-rolls a well-formed but non-canonical evidence manifest:
        // top-level `sources` (not `entries`), per-source `source_text_path`
        // (not `path`) and `sha256` (not `sha256_binary`), omitting secondary
        // metadata. The data is real (pmid + snapshot + quote); the validators
        // must resolve it via field aliases + defaults.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nMaxQuant,method,19029910,enables high peptide identification,0,pubmed_abstract,sha256:abc,2026-06-09T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":"2","row_count":1,"sources":[{"pmid":"19029910","source_kind":"pubmed_abstract","source_text_path":"pubmed_19029910.txt","sha256":"sha256:00","redistributable":true,"title":"MaxQuant","journal":"NBT"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("pubmed_19029910.txt"),
            "MaxQuant enables high peptide identification rates and quantification",
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "codex `sources` manifest must resolve via aliases (sources/source_text_path)"
        );
        assert!(
            run_pmid_resolves(&csv, &manifest).is_ok(),
            "pmid_resolves must find the entry in a `sources`-keyed manifest"
        );
    }

    #[test]
    fn validators_require_redistribution_basis_for_codex_pasilla_schema() {
        // The exact schema codex (gpt-5.5) emits for review_prior_work, captured
        // from a live pasilla run: claims CSV uses `source_type` (not source_kind),
        // `quote_start` (not evidence_quote_offset), `source_sha256` (not
        // source_hash), and OMITS source_kind / redistributable columns; the
        // manifest entries use `source_type` (not source_kind) and `pmids` (plural).
        // Locator and quote obligations still resolve the schema aliases, but
        // the legal gate fails closed because neither the row nor manifest
        // records a redistribution basis.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(
            &csv,
            "pmid,source_type,evidence_quote,quote_start,quote_end,verified,source_sha256,source_path\n\
             20921232,pubmed_abstract_with_pmc_front_xml_checked,\"combined RNAi and mRNA-seq to identify exons\",10,54,True,sha256:abc,evidence/source_text_pmid_20921232.txt\n",
        );
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"claim_boundary":"verbatim quotes","generated_at":"2026-06-09T00:00:00Z","task_id":"review_prior_work","entries":[{"path":"evidence/pubmed_20921232.xml","pmids":["20921232"],"source_type":"pubmed_efetch_xml","sha256":"sha256:xml"},{"path":"evidence/source_text_pmid_20921232.txt","pmid":"20921232","pmcid":"PMC3032923","source_type":"pubmed_abstract_with_pmc_front_xml_checked","sha256":"sha256:abc"}]}"#,
        );
        write(
            &manifest
                .parent()
                .unwrap()
                .join("source_text_pmid_20921232.txt"),
            "We combined RNAi and mRNA-seq to identify exons regulated by Pasilla.",
        );
        assert!(
            run_pmid_resolves(&csv, &manifest).is_ok(),
            "pmid_resolves must accept codex's source_type/pmids manifest schema"
        );
        assert!(
            run_evidence_quote_substring_match(&csv, &manifest).is_ok(),
            "evidence_quote_substring_match must resolve codex's quote against the source"
        );
        assert!(
            run_redistributable_or_marked(&csv, &manifest).is_err(),
            "PubMed delivery must not substitute for an explicit redistribution basis"
        );
    }

    #[test]
    fn redistributable_accepts_marked_literature_source_generically() {
        // Generalized legal gate: any non-external-PDF source marked
        // redistributable passes (covers executor-specific source_kind
        // spellings like codex's), while external local PDFs stay strict.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let hdr = "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";
        let run = |sk: &str, redist: &str| {
            let csv = dir.path().join("m.csv");
            write(&csv, &format!("{hdr}\nX,gene,28123456,q,0,{sk},sha256:abc,2026-06-09T00:00:00Z,{redist},true\n"));
            run_redistributable_or_marked(&csv, &manifest)
        };
        // codex / unknown literature source_kind, marked redistributable → ok.
        assert!(run("pubmed", "true").is_ok());
        assert!(run("ncbi_efetch", "true").is_ok());
        // Only an explicit PMC OA class passes without a separate marker.
        assert!(run("pmc_xml_fulltext", "false").is_err());
        assert!(run("pmc_oa_full_text", "false").is_ok());
        assert!(run("pubmed_abstract", "false").is_err());
        // Legal gate preserved: external local PDF must not claim redistribution,
        // and an unmarked source with no class-determined license still fails.
        assert!(run("external_pdf_local_only", "true").is_err());
        assert!(run("external_pdf_local_only", "false").is_ok());
        assert!(run("some_random_blog", "false").is_err());
    }

    #[test]
    fn normalize_collapses_whitespace_and_lowercases() {
        assert_eq!(
            collapse_whitespace_lowercase_v1("  Hello   World\n\t"),
            "hello world"
        );
    }

    #[test]
    fn quote_match_does_not_discard_unclosed_comparison_text() {
        assert!(
            !quote_matches_snapshot("A real prefix was retained.", "A real prefix < fabricated"),
            "an unmatched comparison token must remain part of the asserted quote"
        );
        assert!(
            !quote_matches_snapshot("Any source text", ""),
            "a verified quote cannot be empty"
        );
    }

    #[test]
    fn pmid_resolves_passes_on_well_formed_rows() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(&manifest.parent().unwrap().join("28123456.xml"), "");
        assert!(run_pmid_resolves(&csv, &manifest).is_ok());
    }

    #[test]
    fn pmid_resolves_rejects_malformed_pmid() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,123,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_pmid_resolves(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::PmidMalformed,
                ..
            }
        ));
    }

    #[test]
    fn evidence_quote_substring_match_finds_present_quote() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,reduction in disc tissue,5,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("28123456.xml"),
            "ACAN reduction in disc tissue was observed",
        );
        assert!(run_evidence_quote_substring_match(&csv, &manifest).is_ok());
    }

    #[test]
    fn evidence_quote_substring_match_extracts_structured_abstract_text() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nCASC7,gene,31412983,\"The mechanism remains unknown. Airway smooth muscle cells were used at 1 µM.\",0,pubmed_abstract_xml,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":2,"entries":[{"source_ref":"31412983","source_kind":"pubmed_abstract_xml","path":"pmid_31412983.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"abstract_fair_use"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("pmid_31412983.xml"),
            r#"<PubmedArticle><Abstract><AbstractText Label="BACKGROUND">The mechanism remains unknown.</AbstractText><AbstractText Label="METHODS">Airway smooth muscle cells were used at 1 &#xb5;M.</AbstractText></Abstract></PubmedArticle>"#,
        );
        assert!(run_evidence_quote_substring_match(&csv, &manifest).is_ok());
    }

    #[test]
    fn evidence_quote_substring_match_includes_structured_abstract_label() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nCRISPLD2,gene,42519304,\"BACKGROUND: Secondary spinal cord injury alters airway function.\",0,pubmed_abstract_xml,sha256:abc,2026-07-29T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":2,"entries":[{"source_ref":"42519304","source_kind":"pubmed_abstract_xml","path":"pmid_42519304.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-07-29T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"abstract_fair_use"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("pmid_42519304.xml"),
            r#"<PubmedArticle><Abstract><AbstractText Label="BACKGROUND" NlmCategory="BACKGROUND">Secondary spinal cord injury alters airway function.</AbstractText></Abstract></PubmedArticle>"#,
        );
        assert!(run_evidence_quote_substring_match(&csv, &manifest).is_ok());
    }

    #[test]
    fn evidence_quote_substring_match_includes_pmc_section_title() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nDESeq2,method,40098239,\"MOTIVATION: Biomarker discovery is important and offers insight into potential underlying mechanisms of disease.\",0,pmc_oa_xml,sha256:abc,2026-07-29T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":2,"entries":[{"source_ref":"40098239","source_kind":"pmc_oa_xml","path":"pmc_40098239.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-07-29T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"pmc_oa_cc"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("pmc_40098239.xml"),
            r#"<article><abstract><title>Abstract</title><sec><title>Motivation</title><p>Biomarker discovery is important and offers insight into potential underlying mechanisms of disease.</p></sec></abstract></article>"#,
        );
        assert!(run_evidence_quote_substring_match(&csv, &manifest).is_ok());
    }

    #[test]
    fn evidence_quote_substring_match_rejects_absent_quote() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,this quote is not there,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(
            &manifest,
            r#"{"schema_version":1,"entries":[{"pmid":"28123456","source_kind":"pmc_oa_full_text","path":"28123456.xml","sha256_binary":"00","sha256_extracted_text":"00","extracted_text_normalization":"collapse_whitespace_lowercase_v1","bytes":0,"retrieval_ts":"2026-05-14T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}]}"#,
        );
        write(
            &manifest.parent().unwrap().join("28123456.xml"),
            "some other text",
        );
        let err = run_evidence_quote_substring_match(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::QuoteNotInSource,
                ..
            }
        ));
    }

    // ---- direction_supported_by_quote (FAITHFUL TWINS) ----
    // The concordance-matrix builder assigns same/opposite_direction from THIS
    // dataset's log2FC sign even when the cited quote names no direction. The
    // atom claim_boundary mandates `unverifiable` for a directionless quote;
    // this validator enforces it. The header below is the contextualize
    // claims_evidence_matrix shape (concordance_flag + evidence_quote).
    const DIR_HDR: &str = "finding_id,entity,entity_kind,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified";

    fn write_dir_row(dir: &TempDir, flag: &str, quote: &str) -> std::path::PathBuf {
        let csv = dir.path().join("claims_evidence_matrix.csv");
        // Quote is CSV-quoted so embedded commas/spaces survive the parse.
        write(
            &csv,
            &format!(
                "{DIR_HDR}\nIRS2_finding,IRS2,gene,12345678,{flag},\"{quote}\",0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n"
            ),
        );
        csv
    }

    #[test]
    fn direction_supported_when_quote_states_direction() {
        // (a) FAITHFUL TWIN — genuinely-correct claim still PASSES: a
        // same_direction row whose quote names a direction ("dexamethasone
        // increased X") is supported and must pass.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json"); // unused by this runner
        let csv = write_dir_row(
            &dir,
            "same_direction",
            "dexamethasone increased IRS2 expression in airway smooth muscle",
        );
        assert!(
            run_direction_supported_by_quote(&csv, &manifest).is_ok(),
            "a directional quote must support a same_direction flag"
        );
        // opposite_direction, decrease cue.
        let csv = write_dir_row(
            &dir,
            "opposite_direction",
            "the treatment reduced MFGE8 levels relative to control",
        );
        assert!(run_direction_supported_by_quote(&csv, &manifest).is_ok());
    }

    #[test]
    fn direction_not_supported_when_quote_is_directionless() {
        // (b) FAITHFUL TWIN — the IRS2/MFGE8 case: a same_direction flag whose
        // quote is mere panel membership ("the panel included IRS2, APPL2,
        // RAMP1, MFGE8") states NO direction, so the row must FAIL with
        // DirectionNotSupportedByQuote (a genuine error of this class is caught).
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let csv = write_dir_row(
            &dir,
            "same_direction",
            "the panel included IRS2, APPL2, RAMP1, MFGE8",
        );
        let err = run_direction_supported_by_quote(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::DirectionNotSupportedByQuote,
                ..
            }
        ));
    }

    #[test]
    fn direction_check_skips_unverifiable_row() {
        // (c) FAITHFUL TWIN — a non-asserting row is NOT subject to the check:
        // an `unverifiable` row with the SAME directionless quote makes no
        // directional claim (exactly the verdict the boundary mandates) and
        // must PASS. This proves the validator gates the direction assertion,
        // not the quote text.
        let dir = TempDir::new().unwrap();
        let manifest = dir.path().join("evidence/manifest.json");
        let csv = write_dir_row(
            &dir,
            "unverifiable",
            "the panel included IRS2, APPL2, RAMP1, MFGE8",
        );
        assert!(
            run_direction_supported_by_quote(&csv, &manifest).is_ok(),
            "an unverifiable row asserts no direction and is not subject to the check"
        );
        // no_prior_finding likewise asserts nothing — passes regardless of quote.
        let csv = write_dir_row(&dir, "no_prior_finding", "");
        assert!(run_direction_supported_by_quote(&csv, &manifest).is_ok());
    }

    #[test]
    fn redistributable_inconsistent_rejected() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,28123456,q,0,external_pdf_local_only,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_redistributable_or_marked(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::RedistributableTagInconsistent,
                ..
            }
        ));
    }

    #[test]
    fn source_resolves_accepts_doi_and_rejects_missing_url_snapshot() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        // snapshot present for the DOI row
        std::fs::write(evdir.join("doi_ok.json"), b"hello").unwrap();
        let manifest = serde_json::json!({
            "schema_version": 2,
            "entries": [
                {"source_ref_kind":"doi","source_ref":"10.1/ok","source_class":"conference_proceedings",
                 "source_kind":"abstract_only","path":"doi_ok.json","sha256_binary":"a".repeat(64),
                 "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
                 "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}
            ]
        });
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        std::fs::write(&csv, "entity,entity_kind,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nflair,gene,doi,10.1/ok,conference_proceedings,recommendation_or_benchmark,hello,0,abstract_only,deadbeef,2026-05-31T00:00:00Z,true,true\n").unwrap();
        assert!(run_source_resolves(&csv, &evdir.join("manifest.json")).is_ok());

        // A URL row with no snapshot on disk → SourceUnresolvable.
        let csv2 = dir.path().join("ml2.csv");
        std::fs::write(&csv2, "entity,entity_kind,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nstar,gene,url,https://x/y,tool_documentation,capability_or_version,hello,0,doc_page,deadbeef,2026-05-31T00:00:00Z,false,true\n").unwrap();
        let err = run_source_resolves(&csv2, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::SourceUnresolvable,
                ..
            }
        ));
    }

    #[test]
    fn source_resolves_pmid_branch_byte_identical_to_legacy() {
        // A legacy PMID row (no source_ref_kind) must behave exactly like
        // run_pmid_resolves: malformed PMID → PmidMalformed.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("prior_claims_matrix.csv");
        write(&csv, "entity,entity_kind,pmid,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\nACAN,gene,123,foo,0,pmc_oa_full_text,sha256:abc,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_source_resolves(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::PmidMalformed,
                ..
            }
        ));
    }

    #[test]
    fn source_resolves_skips_curated_baseline_rows() {
        // A curated_baseline row (offline/thin-literature fallback) carries no
        // locator: source_ref_kind / source_ref empty, verified=false. It must
        // be SKIPPED — not resolved, not failed — so the survey task completes
        // rather than blocking when literature retrieval was unavailable.
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            r#"{"schema_version":2,"entries":[]}"#,
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        // Real method_landscape.csv column shape (axis/candidate_method/...),
        // a single curated_baseline row with no locator.
        std::fs::write(
            &csv,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,star,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n",
        )
        .unwrap();
        assert!(
            run_source_resolves(&csv, &evdir.join("manifest.json")).is_ok(),
            "curated_baseline rows must pass source_resolves (skipped, not failed)"
        );

        // A curated_baseline row mixed with a real locator row that resolves
        // also passes; the real-locator check is not weakened.
        std::fs::write(evdir.join("doi_ok.json"), b"hello").unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&serde_json::json!({
                "schema_version": 2,
                "entries": [
                    {"source_ref_kind":"doi","source_ref":"10.1/ok","source_class":"conference_proceedings",
                     "source_kind":"openalex","path":"doi_ok.json","sha256_binary":"a".repeat(64),
                     "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
                     "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q001","redistributable":true,"license":"CC-BY-4.0"}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let csv2 = dir.path().join("ml_mixed.csv");
        std::fs::write(
            &csv2,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,star,doi,10.1/ok,conference_proceedings,recommendation_or_benchmark,hello,0,openalex,sha256:abc,2026-05-31T00:00:00Z,true,true\n\
             alignment,hisat2,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n",
        )
        .unwrap();
        assert!(
            run_source_resolves(&csv2, &evdir.join("manifest.json")).is_ok(),
            "mixed curated_baseline + resolvable-locator rows must pass"
        );

        // A curated_baseline row alongside an UNresolvable real-locator row
        // still fails on the real row — the skip is scoped to curated_baseline.
        let csv3 = dir.path().join("ml_bad.csv");
        std::fs::write(
            &csv3,
            "axis,candidate_method,source_ref_kind,source_ref,source_class,evidence_role,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\n\
             alignment,hisat2,,,curated_baseline,,,0,,,2026-05-31T00:00:00Z,true,false\n\
             alignment,salmon,url,https://x/missing,tool_documentation,capability_or_version,hello,0,doc_page,sha256:abc,2026-05-31T00:00:00Z,false,true\n",
        )
        .unwrap();
        let err = run_source_resolves(&csv3, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::SourceUnresolvable,
                ..
            }
        ));
    }

    // ====================================================================
    // claim_support_satisfied
    // ====================================================================

    #[test]
    fn claim_support_rejects_default_without_paper_class() {
        // hisat2 has only tool-doc evidence → not literature_eligible, so it
        // can NOT qualify as a default/recommended candidate. With no
        // qualifying default present, a tool-doc-only candidate carrying the
        // default marker must fail with InsufficientCorroboration.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,tier,source_class,source_ref,verified\n\
             alignment,hisat2,defaultRecommended,tool_documentation,https://rtd/x,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_accepts_default_with_two_paper_sources() {
        // star has two distinct paper-class verified sources → qualifies as a
        // default candidate with ≥1 paper-class row AND ≥2 distinct sources.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("ml.csv");
        write(
            &csv,
            "axis,candidate_method,tier,source_class,source_ref,verified\n\
             alignment,star,defaultRecommended,primary_literature,30000000,true\n\
             alignment,star,defaultRecommended,conference_proceedings,10.1/x,true\n",
        );
        assert!(run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok());
    }

    #[test]
    fn claim_support_rejects_eligible_default_with_one_source() {
        // star is literature_eligible (one paper-class verified row) and thus
        // a default candidate, but it has only ONE distinct verified source —
        // below minimumIndependentSources (2) → InsufficientCorroboration.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,star,primary_literature,30000000,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_ignores_non_default_tool_doc_candidates() {
        // A candidate with only tool-doc evidence is not literature_eligible
        // and is not marked default → it is not constrained and must pass.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,hisat2,tool_documentation,https://rtd/x,true\n",
        );
        assert!(run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok());
    }

    #[test]
    fn claim_support_reads_minimum_from_policy() {
        // A package-relative source-discovery-policy.json raising the minimum
        // to 3 makes a two-source default fail.
        let dir = TempDir::new().unwrap();
        // artifact dir layout: <root>/runtime/outputs/<task>/method_landscape.csv
        let artifact = dir.path().join("runtime/outputs/survey_method_landscape");
        std::fs::create_dir_all(&artifact).unwrap();
        let policies = dir.path().join("policies");
        std::fs::create_dir_all(&policies).unwrap();
        write(
            &policies.join("source-discovery-policy.json"),
            r#"{"claimSupportRules":{"minimumIndependentSources":3}}"#,
        );
        let csv = artifact.join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             alignment,star,primary_literature,30000000,true\n\
             alignment,star,conference_proceedings,10.1/x,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_deranks_weak_alternative_when_axis_has_valid_default() {
        // An axis with a corroborated default (gatk_hard_filter: 2 distinct
        // paper sources) plus a thin alternative (bcftools_filter: 1 source)
        // must PASS — the weak candidate is de-ranked, not a failure, because
        // the axis is still recommendable. (Regression for the nekrutenko eval
        // 0.0 where one peripheral single-citation method blocked the whole
        // survey and stranded variant_calling.)
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000001,true\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000002,true\n\
             variant_filtering,bcftools_filter,primary_literature,30000003,true\n",
        );
        assert!(
            run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok(),
            "axis with a corroborated default must pass; the single-source \
             alternative is de-ranked, not a hard failure"
        );
    }

    #[test]
    fn claim_support_axis_corroboration_counts_distinct_sources_across_candidates() {
        // Two candidates on one axis, each with a SINGLE (distinct) source. No
        // single candidate reaches minimumIndependentSources (2), but the axis
        // as a whole cites 2 distinct verified paper-class sources → the axis's
        // method-choice IS corroborated. Corroboration is an axis-level property
        // that may be distributed across candidates, so this PASSES. (Regression
        // for the pasilla normalisation axis: thin PubMed retrieval returned one
        // abstract per method — edger_tmm + sctransform — and the per-candidate
        // bar flakily blocked an axis that is in fact doubly-grounded.)
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             variant_filtering,gatk_hard_filter,primary_literature,30000001,true\n\
             variant_filtering,bcftools_filter,primary_literature,30000003,true\n",
        );
        assert!(
            run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok(),
            "axis citing 2 distinct papers across its candidates is corroborated"
        );
    }

    #[test]
    fn claim_support_fails_axis_grounded_by_single_paper() {
        // An eligible axis whose candidates, taken together, cite only ONE
        // distinct verified paper-class source (the same PMID under two methods)
        // is genuinely under-corroborated → InsufficientCorroboration. This is
        // the floor the axis-level rule preserves: distributed corroboration
        // still requires ≥minimumIndependentSources DISTINCT papers.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             normalisation,deseq2_mor,primary_literature,30000777,true\n\
             normalisation,edger_tmm,primary_literature,30000777,true\n",
        );
        let err = run_claim_support_satisfied(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InsufficientCorroboration,
                ..
            }
        ));
    }

    #[test]
    fn claim_support_thin_retrieval_axis_with_curated_fallback_passes() {
        // Mirrors the live pasilla normalisation axis post-fix: two methods each
        // grounded by a single distinct paper, plus source-less curated_baseline
        // fallback rows for methods PubMed could not ground. The two papers
        // corroborate the axis; the curated_baseline rows are skipped (not
        // eligible, no obligation). Must PASS.
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,verified\n\
             normalisation,edger_tmm,primary_literature,19910308,true\n\
             normalisation,sctransform,primary_literature,31870423,true\n\
             normalisation,deseq2_vst,curated_baseline,,false\n\
             normalisation,scran,curated_baseline,,false\n",
        );
        assert!(
            run_claim_support_satisfied(&csv, &dir.path().join("ignored")).is_ok(),
            "axis grounded by 2 distinct papers passes despite thin per-method retrieval"
        );
    }

    // ====================================================================
    // method_quote_mentions_candidate
    // ====================================================================

    #[test]
    fn method_quote_accepts_canonical_compound_alias() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,evidence_quote\n\
             normalisation,deseq2_vst,primary_literature,DESeq2 estimates sample-specific size factors.\n",
        );
        assert!(run_method_quote_mentions_candidate(&csv, &dir.path().join("ignored")).is_ok());
    }

    #[test]
    fn method_quote_rejects_generic_query_hit_quote() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,evidence_quote\n\
             normalisation,deseq2_vst,primary_literature,RNA sequencing is widely used in transcriptomics.\n",
        );
        let err =
            run_method_quote_mentions_candidate(&csv, &dir.path().join("ignored")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::CandidateNotInEvidenceQuote,
                ..
            }
        ));
    }

    #[test]
    fn method_quote_skips_curated_baseline_rows() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,evidence_quote\n\
             pathway_enrichment,fgsea,curated_baseline,\n",
        );
        assert!(run_method_quote_mentions_candidate(&csv, &dir.path().join("ignored")).is_ok());
    }

    // ====================================================================
    // doc_page_matches_tool
    // ====================================================================

    fn doc_manifest(entries: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "schema_version": 2, "entries": entries })
    }

    #[test]
    fn doc_page_matches_when_snapshot_mentions_tool() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("flair_doc.html"), "FLAIR v2 supports isoforms").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/flair","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"flair_doc.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/flair,2,true\n",
        );
        assert!(run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).is_ok());
    }

    #[test]
    fn doc_page_rejects_snapshot_missing_tool_name() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("doc.html"), "this page is about something else").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/x","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"doc.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/x,2,true\n",
        );
        let err = run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::DocPageToolMismatch,
                ..
            }
        ));
    }

    #[test]
    fn doc_page_rejects_missing_version_context() {
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(evdir.join("flair.html"), "flair supports isoforms").unwrap();
        let manifest = doc_manifest(serde_json::json!([{
            "source_ref_kind":"url","source_ref":"https://rtd/flair","source_class":"tool_documentation",
            "source_kind":"doc_page","path":"flair.html","sha256_binary":"a".repeat(64),
            "sha256_extracted_text":"b".repeat(64),"extracted_text_normalization":"collapse_whitespace_lowercase_v1",
            "bytes":5,"retrieval_ts":"2026-05-31T00:00:00Z","retrieval_query_id":"q","redistributable":true,"license":"CC-BY-4.0"
        }]));
        std::fs::write(
            evdir.join("manifest.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        // version_context column present but empty → VersionContextMissing.
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,flair,tool_documentation,https://rtd/flair,,true\n",
        );
        let err = run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::VersionContextMissing,
                ..
            }
        ));
    }

    #[test]
    fn doc_page_ignores_non_tool_doc_rows() {
        // A primary_literature row is not subject to the tool-doc relevance /
        // version-context guards.
        let dir = TempDir::new().unwrap();
        let evdir = dir.path().join("evidence");
        std::fs::create_dir_all(&evdir).unwrap();
        std::fs::write(
            evdir.join("manifest.json"),
            r#"{"schema_version":2,"entries":[]}"#,
        )
        .unwrap();
        let csv = dir.path().join("method_landscape.csv");
        write(
            &csv,
            "axis,candidate_method,source_class,source_ref,version_context,verified\n\
             alignment,star,primary_literature,30000000,,true\n",
        );
        assert!(run_doc_page_matches_tool(&csv, &evdir.join("manifest.json")).is_ok());
    }

    #[test]
    fn concordance_flag_outside_closed_set_rejected() {
        let dir = TempDir::new().unwrap();
        let csv = dir.path().join("claims_evidence_matrix.csv");
        write(&csv, "finding_id,entity,entity_kind,prior_pmids,concordance_flag,evidence_quote,evidence_quote_offset,source_kind,source_hash,retrieval_ts,redistributable,verified\ngene_1,ACAN,gene,,hallucinated_flag,,0,none,none,2026-05-14T00:00:00Z,true,true\n");
        let manifest = dir.path().join("evidence/manifest.json");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        write(&manifest, r#"{"schema_version":1,"entries":[]}"#);
        let err = run_concordance_flag_in_closed_set(&csv, &manifest).unwrap_err();
        assert!(matches!(
            err.1,
            ValidationFailureCause::LiteratureClaim {
                kind: LiteratureClaimFailureKind::InvalidConcordanceFlag,
                ..
            }
        ));
    }

    // ---- gene_symbol_ensembl_consistent (Workstream B) ----

    /// Write the claims matrix plus ONE candidate annotation table at
    /// `rel` (relative to `runtime/outputs`). The column-role cases below differ
    /// only in that table's header + values, so they all reuse this.
    fn scaffold_gene_pkg_with_truth(
        root: &Path,
        matrix_csv: &str,
        rel: &str,
        truth: &str,
    ) -> std::path::PathBuf {
        let outputs = root.join("runtime/outputs");
        let ctx = outputs.join("contextualize_findings_with_literature");
        fs::create_dir_all(&ctx).unwrap();
        write(&ctx.join("claims_evidence_matrix.csv"), matrix_csv);
        let truth_path = outputs.join(rel);
        fs::create_dir_all(truth_path.parent().unwrap()).unwrap();
        write(&truth_path, truth);
        truth_path
    }

    fn scaffold_gene_pkg(root: &Path, matrix_csv: &str, with_truth: bool) {
        if with_truth {
            // Independent annotation: CRISPLD2 -> ENSG00000103196 (the real gene).
            scaffold_gene_pkg_with_truth(
                root,
                matrix_csv,
                "pathway_enrichment/intermediates/ranked_genes.tsv",
                "symbol\tgene_id\tstat\nCRISPLD2\tENSG00000103196\t16.7\n",
            );
        } else {
            let ctx = root.join("runtime/outputs/contextualize_findings_with_literature");
            fs::create_dir_all(&ctx).unwrap();
            write(&ctx.join("claims_evidence_matrix.csv"), matrix_csv);
        }
    }

    /// The real shape the DESeq2-plus-annotation step writes: `gene` holds the
    /// ENSG accession, `symbol` the HGNC label, and `gene` comes FIRST in file
    /// column order.
    const DESEQ_PLUS_SYMBOL_TSV: &str = "gene\tbaseMean\tlog2FoldChange\tlfcSE\tstat\tpvalue\tpadj\tsymbol\n\
         ENSG00000103196\t997.4447\t4.574967\t0.184241\t24.83137\t4.11066699528116e-136\t7.05595989740011e-132\tCRISPLD2\n\
         ENSG00000152583\t495.0957\t3.291099\t0.133053\t24.735296\t4.46338432298434e-135\t3.83069959520131e-131\tSPARCL1\n";

    #[test]
    fn gene_symbol_ensembl_consistent_catches_wrong_gene_citation() {
        // The Jun-18 hallucination: CRISPLD2 bound to ENSG00000197142 (ACSL5).
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
            true,
        );
        match gene_symbol_ensembl_consistent(dir.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(message.contains("CRISPLD2"), "msg: {message}");
                assert!(message.contains("ENSG00000197142"), "msg: {message}");
            }
            other => panic!("must Fail on the ACSL5 mislabel, got {other:?}"),
        }
    }

    #[test]
    fn gene_symbol_ensembl_consistent_passes_on_correct_binding() {
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
            true,
        );
        assert!(matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Passed
        ));
    }

    #[test]
    fn gene_symbol_ensembl_consistent_soft_errors_without_truth_source() {
        // No independent annotation -> Errored (non-blocking), never a false Pass/Fail.
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
            false,
        );
        assert!(matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Errored { .. }
        ));
    }

    #[test]
    fn gene_symbol_ensembl_consistent_finds_truth_under_drifted_filename() {
        // Robustness: the truth table may not be named ranked_genes.tsv. A
        // drifted basename in any output dir that carries symbol+Ensembl must
        // still be discovered by the scan fallback.
        let dir = TempDir::new().unwrap();
        let ctx = dir
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        fs::create_dir_all(&ctx).unwrap();
        write(
            &ctx.join("claims_evidence_matrix.csv"),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
        );
        // Truth lives in a differently-named table (annotation.tsv), not the
        // hardcoded ranked_genes.tsv.
        let ann = dir.path().join("runtime/outputs/normalisation");
        fs::create_dir_all(&ann).unwrap();
        write(
            &ann.join("annotation.tsv"),
            "gene_symbol\tensembl_id\nCRISPLD2\tENSG00000103196\n",
        );
        assert!(
            matches!(
                gene_symbol_ensembl_consistent(dir.path()),
                ValidatorOutcome::Failed { .. }
            ),
            "must discover the drifted-name truth table and catch the mislabel"
        );
    }

    // ---- column ROLES: content decides, header order does not ----------------

    #[test]
    fn roles_bind_by_content_when_gene_holds_accessions() {
        // The defect: `gene` is a legal name in BOTH roles and comes first in
        // file column order, so a name+order match bound it as the SYMBOL and
        // then found no Ensembl column at all — the table was reported missing
        // while sitting in the package. Content must win: `gene` (ENSG values)
        // is the accession, `symbol` the label.
        let dir = TempDir::new().unwrap();
        let path = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
            "contextualize_findings_with_literature/intermediates/de_results_with_symbols.tsv",
            DESEQ_PLUS_SYMBOL_TSV,
        );
        // Keyed by SYMBOL, valued by ACCESSION — the inverted binding would map
        // "ENSG00000103196" -> "CRISPLD2" instead.
        let map = load_symbol_ensembl_map(&path).expect("both roles are present in this table");
        assert_eq!(
            map.get("CRISPLD2").map(String::as_str),
            Some("ENSG00000103196")
        );
        assert_eq!(
            map.get("SPARCL1").map(String::as_str),
            Some("ENSG00000152583")
        );
        // …and the whole obligation now reaches a verdict instead of Errored.
        assert!(
            matches!(
                gene_symbol_ensembl_consistent(dir.path()),
                ValidatorOutcome::Passed
            ),
            "a package that ships the annotation table must reach a verdict"
        );
    }

    #[test]
    fn roles_bind_conventionally_for_symbol_then_gene_id() {
        // The conventional header order must be unaffected by the content sniff.
        let dir = TempDir::new().unwrap();
        let path = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
            "pathway_enrichment/intermediates/ranked_genes.tsv",
            "symbol\tgene_id\nCRISPLD2\tENSG00000103196\n",
        );
        let map = load_symbol_ensembl_map(&path).expect("symbol+gene_id is the conventional shape");
        assert_eq!(
            map.get("CRISPLD2").map(String::as_str),
            Some("ENSG00000103196")
        );
    }

    #[test]
    fn gene_column_holding_labels_still_binds_as_the_symbol() {
        // The mirror case: `gene` holds real symbols and the identifier column
        // is a non-Ensembl accession (Entrez), so NO column is accession-shaped
        // and resolution degrades to the name candidates. The dual-role `gene`
        // must lose the accession role to the unambiguous `gene_id` and take the
        // label role — its content does not look like an accession.
        let dir = TempDir::new().unwrap();
        let path = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\n83716,CRISPLD2\n",
            "normalisation/annotation.tsv",
            "gene\tgene_id\nCRISPLD2\t83716\nSPARCL1\t8404\n",
        );
        let map = load_symbol_ensembl_map(&path).expect("name-based fallback must still resolve");
        assert_eq!(map.get("CRISPLD2").map(String::as_str), Some("83716"));
        assert_eq!(map.get("SPARCL1").map(String::as_str), Some("8404"));
    }

    #[test]
    fn single_role_table_is_honestly_not_an_annotation_source() {
        // Roles are DISJOINT: one column can never satisfy both. A label-only
        // ranking (`symbol stat` — the shape the pathway step actually writes)
        // and an accession-only table (`gene_id variance`) each carry ONE role,
        // so neither is an annotation source and the obligation soft-skips
        // rather than fabricating a truth source from a single column.
        let dir = TempDir::new().unwrap();
        let ranked = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
            "pathway_enrichment/intermediates/ranked_genes.tsv",
            "symbol\tstat\nCRISPLD2\t16.7\n",
        );
        assert!(
            load_symbol_ensembl_map(&ranked).is_none(),
            "label-only table"
        );
        let hvg = dir
            .path()
            .join("runtime/outputs/normalisation/hvg_list.tsv");
        fs::create_dir_all(hvg.parent().unwrap()).unwrap();
        write(&hvg, "gene_id\tvariance\nENSG00000129824\t5.74\n");
        assert!(
            load_symbol_ensembl_map(&hvg).is_none(),
            "accession-only table"
        );
        match gene_symbol_ensembl_consistent(dir.path()) {
            ValidatorOutcome::Errored { reason } => {
                assert!(reason.contains("no independent"), "reason: {reason}");
            }
            other => panic!("no two-role table ⇒ honest soft-skip, got {other:?}"),
        }
    }

    #[test]
    fn claims_matrix_is_never_its_own_annotation_source() {
        // The canonical claims matrix carries `finding_id` (accession-shaped)
        // and `entity` (a label), so a content-driven scan would happily resolve
        // both roles ON THE ARTIFACT UNDER TEST and Pass vacuously. It must stay
        // excluded from discovery.
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg(
            dir.path(),
            "finding_id,entity,entity_kind\nENSG00000197142,CRISPLD2,gene\n",
            false,
        );
        assert!(
            matches!(
                gene_symbol_ensembl_consistent(dir.path()),
                ValidatorOutcome::Errored { .. }
            ),
            "the matrix under test must never serve as its own truth source"
        );
    }

    #[test]
    fn effect_map_reads_the_accession_from_a_bare_gene_column() {
        // The same inverted candidate list silently emptied the DR-10 effect
        // map on a `gene`-keyed DE table, so every paralog disagreement lost its
        // direction signal.
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("de_results_with_symbols.tsv");
        write(&path, DESEQ_PLUS_SYMBOL_TSV);
        let effects = load_ensembl_effect_map(&path);
        assert_eq!(effects.get("ENSG00000103196").copied(), Some(4.574967));
        assert_eq!(effects.get("ENSG00000152583").copied(), Some(3.291099));
    }

    #[test]
    fn accession_shape_is_anchored_and_species_agnostic() {
        assert!(looks_like_ensembl_accession("ENSG00000103196"));
        assert!(looks_like_ensembl_accession("ENSG00000103196.13"));
        assert!(looks_like_ensembl_accession("ENSMUSG00000017167"));
        assert!(looks_like_ensembl_accession(" ENSG00000103196 "));
        // A composite key is a label-bearing identifier, not a bare accession
        // column, and a symbol is neither.
        assert!(!looks_like_ensembl_accession("DE_CRISPLD2_ENSG00000103196"));
        assert!(!looks_like_ensembl_accession("CRISPLD2"));
        assert!(!looks_like_ensembl_accession("ENST00000367714"));
    }

    #[test]
    fn discovery_picks_the_two_role_table_over_the_named_ranking() {
        // Both files exist, and this is the shape the live run produced:
        // `ranked_genes.tsv` was preferred by NAME (first in the hardcoded list)
        // but carries only the label role, and the fall-through then failed to
        // read the table that DOES carry both — so the obligation Errored with
        // the annotation table sitting in the package. Discovery must key on the
        // roles, not the basename.
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
            "pathway_enrichment/intermediates/ranked_genes.tsv",
            "symbol\tstat\nCRISPLD2\t16.7\n",
        );
        let real = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
            "contextualize_findings_with_literature/intermediates/de_results_with_symbols.tsv",
            DESEQ_PLUS_SYMBOL_TSV,
        );
        assert_eq!(
            find_symbol_ensembl_table(&dir.path().join("runtime/outputs")),
            Some(real),
        );
    }

    #[test]
    fn a_dedicated_annotation_subdir_is_discovered() {
        // The producer writes its first-class map under `annotation/` rather
        // than `intermediates/` (which the deposit exporter prunes). Discovery
        // enumerates one subdir level instead of naming one, so the promoted
        // table is found without the Rust side knowing the convention.
        let dir = TempDir::new().unwrap();
        let real = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
            "contextualize_findings_with_literature/annotation/symbol_map.tsv",
            "symbol\tensembl_gene_id\nCRISPLD2\tENSG00000103196\n",
        );
        assert_eq!(
            find_symbol_ensembl_table(&dir.path().join("runtime/outputs")),
            Some(real),
        );
        assert!(
            matches!(
                gene_symbol_ensembl_consistent(dir.path()),
                ValidatorOutcome::Failed { .. }
            ),
            "the promoted annotation table must adjudicate the mislabel"
        );
    }

    #[test]
    fn a_table_of_non_gene_entities_resolves_the_same_way() {
        // Modality-agnostic: nothing outside the accession sniff is
        // gene-specific, so a peak table (`region_id` + `label`) resolves
        // through the same name-based degrade path.
        let dir = TempDir::new().unwrap();
        let path = scaffold_gene_pkg_with_truth(
            dir.path(),
            "finding_id,entity\nPEAK_1,chr1:1000-2000\n",
            "peak_calling/annotation.tsv",
            "region_id\tlabel\nPEAK_1\tchr1:1000-2000\n",
        );
        let map = load_symbol_ensembl_map(&path).expect("region_id+label carries both roles");
        assert_eq!(
            map.get("chr1:1000-2000").map(String::as_str),
            Some("PEAK_1")
        );
    }

    #[test]
    fn gene_symbol_ensembl_runner_routes_via_default_dispatch() {
        // The obligation must reach GeneSymbolEnsemblConsistentRunner (not fall
        // through to Unimplemented), and the runner must derive the package root
        // from the task artifact dir so the cross-task truth table is found.
        let dir = TempDir::new().unwrap();
        scaffold_gene_pkg(
            dir.path(),
            "finding_id,gene_symbol\nENSG00000197142,CRISPLD2\n",
            true,
        );
        let artifact = dir
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        let runners = crate::validators::default_runners();
        let rows = crate::validators::run_validators(
            &["gene_symbol_ensembl_consistent".into()],
            &runners,
            &artifact,
        );
        assert_eq!(rows.len(), 1);
        assert!(
            !matches!(rows[0].outcome, ValidatorOutcome::Unimplemented { .. }),
            "obligation must be dispatched, not Unimplemented"
        );
        assert!(
            matches!(rows[0].outcome, ValidatorOutcome::Failed { .. }),
            "must catch the ACSL5 mislabel through the dispatch path, got {:?}",
            rows[0].outcome
        );
    }

    // ---- DR-10: paralog-aware classification --------------------------------

    /// Scaffold a package with a custom independent truth table
    /// (`ranked_genes.tsv`: symbol/gene_id/stat) and a custom claims matrix
    /// (`finding_id,gene_symbol`).
    fn scaffold_paralog_pkg(
        root: &Path,
        truth_rows: &[(&str, &str, f64)],
        claims_rows: &[(&str, &str)],
    ) {
        let ctx = root.join("runtime/outputs/contextualize_findings_with_literature");
        fs::create_dir_all(&ctx).unwrap();
        let mut claims = String::from("finding_id,gene_symbol\n");
        for (fid, sym) in claims_rows {
            claims.push_str(&format!("{fid},{sym}\n"));
        }
        write(&ctx.join("claims_evidence_matrix.csv"), &claims);

        let pw = root.join("runtime/outputs/pathway_enrichment/intermediates");
        fs::create_dir_all(&pw).unwrap();
        let mut truth = String::from("symbol\tgene_id\tstat\n");
        for (sym, ens, stat) in truth_rows {
            truth.push_str(&format!("{sym}\t{ens}\t{stat}\n"));
        }
        write(&pw.join("ranked_genes.tsv"), &truth);
    }

    fn read_gene_symbol_verdict(root: &Path) -> Option<serde_json::Value> {
        let p = root
            .join("runtime/outputs")
            .join(GENE_SYMBOL_VALIDATE_TASK)
            .join("result.json");
        std::fs::read_to_string(p)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
    }

    #[test]
    fn paralog_benign_same_family_concordant_downgrades_to_warn() {
        // LRRC37A truly maps to ENSG…681; the claim binds it to its 17q21
        // segmental-duplication paralog LRRC37A2 (ENSG…083), which is DE in the
        // SAME direction. Benign ambiguity → WARN, not a required failure.
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[
                ("LRRC37A", "ENSG00000176681", 5.0),
                ("LRRC37A2", "ENSG00000238083", 4.7),
            ],
            &[("DE_LRRC37A2_ENSG00000238083", "LRRC37A")],
        );
        assert!(
            matches!(
                gene_symbol_ensembl_consistent(dir.path()),
                ValidatorOutcome::Passed
            ),
            "a concordant same-family paralog must not be a required failure"
        );
        let v = read_gene_symbol_verdict(dir.path()).expect("verdict recorded");
        assert_eq!(v["validation_passed"], serde_json::json!(true));
        assert_eq!(v["required_failures"].as_array().unwrap().len(), 0);
        assert_eq!(
            v["warnings"].as_array().unwrap().len(),
            1,
            "the benign paralog must be recorded as a warning"
        );
    }

    #[test]
    fn paralog_same_family_discordant_direction_stays_required() {
        // Same LRRC37A↔LRRC37A2 family, but the paralog is DE in the OPPOSITE
        // direction — swapping it changes the biology → required failure.
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[
                ("LRRC37A", "ENSG00000176681", 5.0),
                ("LRRC37A2", "ENSG00000238083", -4.7),
            ],
            &[("DE_LRRC37A2_ENSG00000238083", "LRRC37A")],
        );
        assert!(matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Failed { .. }
        ));
        let v = read_gene_symbol_verdict(dir.path()).unwrap();
        assert_eq!(v["validation_passed"], serde_json::json!(false));
        assert_eq!(v["required_failures"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn smn1_smn2_opposite_clinical_meaning_stays_required() {
        // SMN1 and SMN2 are same-family, concordant direction — but they carry
        // OPPOSITE clinical meaning, so the confusion must stay required-fail.
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[
                ("SMN1", "ENSG00000172062", 3.0),
                ("SMN2", "ENSG00000205571", 3.1),
            ],
            &[("DE_SMN2_ENSG00000205571", "SMN1")],
        );
        match gene_symbol_ensembl_consistent(dir.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(
                    message.contains("opposite clinical meaning"),
                    "msg: {message}"
                );
            }
            other => panic!("SMN1↔SMN2 must stay required-fail, got {other:?}"),
        }
        assert_eq!(
            read_gene_symbol_verdict(dir.path()).unwrap()["validation_passed"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn pseudogene_target_stays_required() {
        // GOLGA6L3P is a processed pseudogene (flagged in the curated table);
        // an expression claim mis-bound onto it is not adjudicable → required.
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[
                ("GOLGA6L3P", "ENSG00000259425", 2.0),
                ("GOLGA6L1", "ENSG00000174450", 2.1),
            ],
            &[("DE_GOLGA6L1_ENSG00000174450", "GOLGA6L3P")],
        );
        match gene_symbol_ensembl_consistent(dir.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(message.contains("pseudogene"), "msg: {message}");
            }
            other => panic!("a pseudogene target must stay required-fail, got {other:?}"),
        }
    }

    #[test]
    fn cross_gene_unrelated_locus_stays_required() {
        // CRISPLD2 (chr16) bound to ACSL5's Ensembl (chr10): unrelated loci,
        // not a paralog family → required (the historical false-citation class).
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[("CRISPLD2", "ENSG00000103196", 16.7)],
            &[("ENSG00000197142", "CRISPLD2")],
        );
        match gene_symbol_ensembl_consistent(dir.path()) {
            ValidatorOutcome::Failed { message } => {
                assert!(message.contains("CRISPLD2"), "msg: {message}");
                assert!(message.contains("ENSG00000197142"), "msg: {message}");
            }
            other => panic!("cross-gene wrong-binding must stay required-fail, got {other:?}"),
        }
        assert_eq!(
            read_gene_symbol_verdict(dir.path()).unwrap()["validation_passed"],
            serde_json::json!(false)
        );
    }

    #[test]
    fn benign_paralog_corpus_downgrades_to_warn() {
        // Representative benign deposit cases across distinct segmental-
        // duplication families: each binds its symbol to a concordant
        // same-family paralog → WARN (not required).
        let cases: &[(&str, &str, f64, &str, &str, f64)] = &[
            (
                "LRRC37A",
                "ENSG00000176681",
                4.0,
                "LRRC37A2",
                "ENSG00000238083",
                3.6,
            ),
            (
                "ARL17A",
                "ENSG00000185829",
                2.5,
                "ARL17B",
                "ENSG00000228696",
                2.2,
            ),
            (
                "RABL2B",
                "ENSG00000079805",
                1.8,
                "RABL2A",
                "ENSG00000079974",
                1.7,
            ),
            (
                "GOLGA8A",
                "ENSG00000104332",
                3.1,
                "GOLGA8M",
                "ENSG00000188626",
                2.9,
            ),
            (
                "SLX1B",
                "ENSG00000181625",
                2.0,
                "SLX1A",
                "ENSG00000180992",
                1.9,
            ),
            (
                "GPR89B",
                "ENSG00000117262",
                2.4,
                "GPR89A",
                "ENSG00000188092",
                2.3,
            ),
        ];
        for (sym, sym_ens, sym_eff, para, para_ens, para_eff) in cases {
            let dir = TempDir::new().unwrap();
            scaffold_paralog_pkg(
                dir.path(),
                &[(sym, sym_ens, *sym_eff), (para, para_ens, *para_eff)],
                &[(&format!("DE_{para}_{para_ens}"), sym)],
            );
            assert!(
                matches!(
                    gene_symbol_ensembl_consistent(dir.path()),
                    ValidatorOutcome::Passed
                ),
                "benign paralog {sym}→{para} must downgrade to a warning"
            );
            let v = read_gene_symbol_verdict(dir.path()).unwrap();
            assert_eq!(
                v["warnings"].as_array().unwrap().len(),
                1,
                "{sym}→{para} must record exactly one paralog warning"
            );
            assert_eq!(v["required_failures"].as_array().unwrap().len(), 0);
        }
    }

    #[test]
    fn clean_pass_removes_stale_domain_verdict() {
        // A required failure records a verdict; when the disagreement is later
        // fixed (claim == truth), the stale verdict must be dropped so a fixed
        // re-run does not keep reporting a domain failure.
        let dir = TempDir::new().unwrap();
        scaffold_paralog_pkg(
            dir.path(),
            &[("CRISPLD2", "ENSG00000103196", 16.7)],
            &[("ENSG00000197142", "CRISPLD2")],
        );
        assert!(matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Failed { .. }
        ));
        assert!(read_gene_symbol_verdict(dir.path()).is_some());

        // Fix the binding and re-run.
        let ctx = dir
            .path()
            .join("runtime/outputs/contextualize_findings_with_literature");
        write(
            &ctx.join("claims_evidence_matrix.csv"),
            "finding_id,gene_symbol\nENSG00000103196,CRISPLD2\n",
        );
        assert!(matches!(
            gene_symbol_ensembl_consistent(dir.path()),
            ValidatorOutcome::Passed
        ));
        assert!(
            read_gene_symbol_verdict(dir.path()).is_none(),
            "a clean pass must remove the stale domain verdict"
        );
    }

    #[test]
    fn embedded_paralog_table_parses() {
        let t = paralog_table();
        assert!(!t.families.is_empty());
        // The opposite-clinical-meaning SMN family must be present and flagged.
        let smn = t
            .families
            .iter()
            .find(|f| f.members.iter().any(|m| m.symbol == "SMN1"))
            .expect("SMN family present");
        assert!(smn.opposite_clinical_meaning);
    }
}
