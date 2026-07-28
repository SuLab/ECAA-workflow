//! Content-based delimiter detection for tabular artifacts.
//!
//! A table's filename extension is a naming convention the writer may
//! violate: R's `write.csv(x, "foo.tsv")` produces a comma-delimited file
//! under a `.tsv` name, and the agent-authored analysis scripts do exactly
//! that. Every reader that trusted the extension mis-parsed such a file as
//! ONE column whose name was the entire header line, with two distinct
//! consequences depending on the reader:
//!
//! - the re-execution comparator ([`crate::reexecution`]) compared that
//!   synthetic column as a string and failed the artifact spuriously;
//! - the report-data assembler ([`crate::report_contract::assemble`]) found
//!   no recognizable entity column and silently skipped the table as "not a
//!   result table", quietly omitting real results from the report.
//!
//! Both now sniff from content and fall back to the extension only when the
//! content is ambiguous. Each caller supplies its own extension fallback,
//! because the two historical defaults differ and changing either would move
//! behavior for extensionless paths.

use std::io::Read;
use std::path::Path;

/// Delimiters considered when sniffing. Tab and comma only — the artifact
/// contract admits `.tsv` and `.csv`, and admitting semicolon or pipe would
/// mis-sniff those characters where they appear inside free-text columns.
pub(crate) const CANDIDATE_DELIMITERS: [u8; 2] = [b'\t', b','];

/// Leading records inspected when sniffing. Enough to tell a real field
/// separator from a character that merely appears in the header, without
/// parsing a multi-megabyte table once per candidate.
pub(crate) const SNIFF_RECORDS: usize = 5;

/// Bytes read from the head of a file when sniffing a delimiter from a path.
/// Comfortably covers [`SNIFF_RECORDS`] rows of a wide table.
const SNIFF_BYTES: usize = 64 * 1024;

/// Field count shared by the first [`SNIFF_RECORDS`] records when parsed with
/// `delimiter`, or `None` when those records are ragged under it (or the
/// reader errors). A returned count of exactly 1 means `delimiter` never
/// occurs in the inspected content — every line came back whole as a single
/// field.
pub(crate) fn consistent_field_count(bytes: &[u8], delimiter: u8) -> Option<usize> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(false)
        .flexible(true)
        .from_reader(bytes);
    let mut count: Option<usize> = None;
    for rec in rdr.records().take(SNIFF_RECORDS) {
        let len = rec.ok()?.len();
        match count {
            None => count = Some(len),
            Some(n) if n == len => {}
            // Ragged under this delimiter: it is not the field separator.
            Some(_) => return None,
        }
    }
    count
}

/// Sniff the field delimiter of `bytes`, using `fallback` when the content is
/// ambiguous.
///
/// A candidate is *viable* when the leading records all parse to the SAME
/// field count and that count exceeds 1. `fallback` wins whenever it is
/// viable — that keeps a genuine TSV on tab even when some column also
/// contains commas — and the other candidate is used only when `fallback` is
/// not viable. When neither is viable (a genuinely single-column table, or an
/// empty file) `fallback` is returned, preserving each caller's historical
/// behavior.
pub(crate) fn sniff_delimiter(bytes: &[u8], fallback: u8) -> u8 {
    let viable = |d: u8| consistent_field_count(bytes, d).is_some_and(|n| n > 1);
    if viable(fallback) {
        return fallback;
    }
    CANDIDATE_DELIMITERS
        .into_iter()
        .find(|&d| d != fallback && viable(d))
        .unwrap_or(fallback)
}

/// [`sniff_delimiter`] over the head of a file, for callers that stream the
/// table rather than holding it in memory.
///
/// Reads at most [`SNIFF_BYTES`] and trims back to the last newline so a
/// buffer boundary cannot manufacture a ragged final record and defeat the
/// sniff. An unreadable path yields `fallback` — the caller's own open will
/// report the real error.
pub(crate) fn sniff_delimiter_from_path(path: &Path, fallback: u8) -> u8 {
    let mut buf = vec![0u8; SNIFF_BYTES];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut buf))
        .unwrap_or(0);
    buf.truncate(read);
    // Trim a partial trailing line, but only when a newline was seen at all:
    // a single header line wider than the buffer still sniffs correctly from
    // the fragment, whereas truncating it to nothing would not.
    if let Some(last_nl) = buf.iter().rposition(|&b| b == b'\n') {
        buf.truncate(last_nl + 1);
    }
    sniff_delimiter(&buf, fallback)
}

/// True when `field` contains a candidate delimiter OTHER than the one it was
/// parsed with — the signature of a mis-parse rather than a real
/// single-column cell.
pub(crate) fn contains_foreign_delimiter(field: &str, delimiter: u8) -> bool {
    CANDIDATE_DELIMITERS
        .iter()
        .any(|&d| d != delimiter && field.as_bytes().contains(&d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_content_under_tab_fallback_sniffs_comma() {
        assert_eq!(
            sniff_delimiter(b"gene_id,mean_log10\nENSG1,2.87\n", b'\t'),
            b',',
            "comma-delimited content must win over a tab fallback"
        );
    }

    #[test]
    fn genuine_content_keeps_its_fallback() {
        assert_eq!(sniff_delimiter(b"gene\tlfc\nG1\t2.0\n", b'\t'), b'\t');
        assert_eq!(sniff_delimiter(b"gene,lfc\nG1,2.0\n", b','), b',');
    }

    #[test]
    fn ambiguous_content_breaks_toward_the_fallback() {
        // Both delimiters yield a consistent 2-field parse, so the caller's
        // fallback decides — a genuine TSV with a comma inside column one is
        // not flipped onto comma.
        let body = b"name,tag\tvalue\nA,x\t1.0\nB,y\t2.0\n";
        assert_eq!(sniff_delimiter(body, b'\t'), b'\t');
        assert_eq!(sniff_delimiter(body, b','), b',');
    }

    #[test]
    fn single_column_and_empty_content_keep_the_fallback() {
        assert_eq!(sniff_delimiter(b"gene\nG1\nG2\n", b'\t'), b'\t');
        assert_eq!(sniff_delimiter(b"gene\nG1\nG2\n", b','), b',');
        assert_eq!(sniff_delimiter(b"", b'\t'), b'\t');
        assert_eq!(sniff_delimiter(b"", b','), b',');
    }

    #[test]
    fn ragged_under_a_candidate_rejects_it() {
        // Comma appears, but raggedly — it is not the separator.
        assert_eq!(consistent_field_count(b"a,b\nc\n", b','), None);
        assert_eq!(consistent_field_count(b"a\tb\nc\td\n", b'\t'), Some(2));
    }

    #[test]
    fn path_sniff_survives_a_buffer_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wide.tsv");
        // Comma-delimited body large enough that the head read lands
        // mid-line; trimming to the last newline must keep the sniff honest.
        let mut body = String::from("gene_id,value\n");
        for i in 0..20_000 {
            body.push_str(&format!("ENSG{i:011},{i}.0\n"));
        }
        std::fs::write(&path, &body).unwrap();
        assert_eq!(sniff_delimiter_from_path(&path, b'\t'), b',');
    }

    #[test]
    fn path_sniff_on_missing_file_yields_the_fallback() {
        assert_eq!(
            sniff_delimiter_from_path(Path::new("/nonexistent/x.tsv"), b'\t'),
            b'\t'
        );
    }

    #[test]
    fn foreign_delimiter_detection_is_delimiter_relative() {
        assert!(contains_foreign_delimiter("gene_id,mean", b'\t'));
        assert!(!contains_foreign_delimiter("gene_id", b'\t'));
        assert!(contains_foreign_delimiter("gene_id\tmean", b','));
    }
}
