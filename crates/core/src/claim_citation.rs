//! Parsing of the free-text evidence string an execution agent writes next to
//! each claim.
//!
//! The evidence field is prose-adjacent, not a path: an agent routinely names
//! several artifacts in one string, qualifies one of them with a parenthetical,
//! and points into a structured file with a `::` field path. A consumer that
//! treats the whole string as a single filename resolves nothing and convicts
//! correct claims of citing an artifact that "does not exist" — the compound
//! citation was never split. [`parse_evidence_citations`] is the shared splitter
//! that makes each reference individually resolvable.
//!
//! The grammar keys only off punctuation, path syntax, and the
//! `EVIDENCE_ARTIFACT_EXTENSIONS` allowlist. No analysis vocabulary
//! participates in any decision, so the parser behaves identically for every
//! workflow modality, including modalities that do not exist yet: an unfamiliar
//! output format is one new entry in the extension allowlist, never a new match
//! arm.

use std::collections::BTreeSet;
use std::sync::LazyLock;

/// File extensions that mark an evidence token as naming an artifact.
///
/// Data, not logic. The parser never inspects an artifact's *name*, only the
/// extension it carries, which is what keeps recognition modality-agnostic.
/// Matching is ASCII-case-insensitive.
const EVIDENCE_ARTIFACT_EXTENSIONS: &[&str] = &[
    "tsv", "csv", "txt", "json", "jsonl", "md", "png", "pdf", "svg", "parquet", "h5", "h5ad",
    "bed", "vcf", "gff", "gtf", "fa", "fasta", "fastq", "bam", "cram", "mtx", "rds", "npz",
];

/// Bracket characters that open a commentary group. A group is scanned as one
/// unit so a separator inside it never splits the citation that owns it.
const GROUP_OPENERS: [char; 3] = ['(', '[', '{'];

/// Bracket characters that close a commentary group, paired positionally-
/// agnostically with [`GROUP_OPENERS`]: agents mix `(`/`]` often enough that
/// requiring the matching partner loses more citations than it protects.
const GROUP_CLOSERS: [char; 3] = [')', ']', '}'];

/// Characters stripped from the *front* of a candidate artifact token.
///
/// `.` and `/` are deliberately absent: `./out/x.tsv` and `/abs/x.tsv` are
/// paths, and trimming their first character would rewrite the reference.
const LEADING_NOISE: &[char] = &[
    '`', '"', '\'', '*', '<', '“', '”', '‘', '’', '«', '»', ',', ';', ':', '!', '?', '(', '[', '{',
    '=', '|', '#',
];

/// Characters stripped from the *end* of a candidate artifact token, so a
/// citation that closes a sentence yields the bare artifact name.
const TRAILING_NOISE: &[char] = &[
    '`', '"', '\'', '*', '>', '“', '”', '‘', '’', '«', '»', '.', ',', ';', ':', '!', '?', ')', ']',
    '}', '=', '|',
];

/// Characters trimmed from both ends of selector commentary. Brackets are absent
/// on purpose: commentary is preserved as the agent wrote it apart from quoting
/// and sentence punctuation, so a nested qualifier survives intact.
const COMMENTARY_TRIM: &[char] = &[
    '`', '"', '\'', '*', '.', ',', ';', ':', '!', '?', '“', '”', '‘', '’', ' ', '\t', '\n', '\r',
];

/// Bracket nesting depth the scanner descends into before treating the enclosed
/// text as opaque commentary. Bounds recursion on adversarial input; real
/// citations nest one or two levels.
const MAX_GROUP_DEPTH: usize = 8;

/// Separator that spells a conjunction between two citations (` and `, ` + `).
/// Anchored at the head of the unscanned tail, and only consulted at bracket
/// depth zero, so `sand.tsv` and `(a and b)` are never split.
static CONJUNCTION_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^\s+(?:and|\+)(?:\s+|$)").expect("static regex"));

/// Any whitespace run, collapsed to one space when normalising commentary so a
/// selector spanning a line break compares equal to its single-line form.
static WHITESPACE_RUN_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\s+").expect("static regex"));

/// One resolved reference inside an agent-written evidence string.
///
/// The invariant that makes this type useful to a resolver: [`Self::artifact`]
/// is always a bare name or relative path with no commentary, no field pointer,
/// and no surrounding punctuation, so it can be handed straight to a
/// filesystem or manifest lookup. Everything the resolver must *not* treat as
/// part of the path is separated out into [`Self::selector`] and
/// [`Self::field`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceCitation {
    /// File name or relative path as written, with surrounding punctuation and
    /// parenthetical commentary stripped. Never empty.
    pub artifact: String,
    /// A parenthetical or trailing qualifier naming a column, row, or subset,
    /// e.g. `score column`, `row 12`. Commentary only; not a path, and never
    /// `Some("")`.
    pub selector: Option<String>,
    /// A field path following `::`, e.g. `n_rows`, `counts.n_retained`. Dots
    /// inside the field are part of the field, not a file extension. Never
    /// `Some("")`.
    pub field: Option<String>,
}

impl EvidenceCitation {
    /// A citation naming a bare artifact — no selector, no field.
    ///
    /// Convenience constructor for call sites (and test fixtures) that already
    /// hold a clean artifact name; it performs no parsing and no validation, so
    /// `name` is stored verbatim.
    pub fn artifact_only(name: &str) -> Self {
        Self {
            artifact: name.to_string(),
            selector: None,
            field: None,
        }
    }
}

/// Split an agent-written evidence string into its constituent citations,
/// preserving source order and deduplicating exact repeats.
///
/// Guarantees:
/// - Every returned [`EvidenceCitation::artifact`] was present verbatim in
///   `raw` (modulo stripped surrounding punctuation) — no artifact is ever
///   synthesised, so prose that names no file returns an empty vector.
/// - Separators recognised at bracket depth zero: `;`, `,`, ` and `, ` + `,
///   newline, and whitespace runs between two artifact tokens. A separator
///   inside a parenthetical does not split.
/// - A token qualifies as an artifact when its last path component carries an
///   extension in the `EVIDENCE_ARTIFACT_EXTENSIONS` allowlist, or —
///   extension-less — when it contains a `/`.
/// - Output order is first-occurrence order; a later citation identical in all
///   three fields is dropped.
pub fn parse_evidence_citations(raw: &str) -> Vec<EvidenceCitation> {
    let mut collected = Vec::new();
    scan_text(raw, 0, &mut collected);
    dedup_preserving_order(collected)
}

/// Split `text` on depth-zero separators and scan each segment in turn.
fn scan_text(text: &str, depth: usize, out: &mut Vec<EvidenceCitation>) {
    for segment in split_top_level(text) {
        scan_segment(&segment, depth, out);
    }
}

/// Walk one separator-free segment, opening a new citation at every artifact
/// token and attaching intervening non-artifact text to the citation in scope.
///
/// Text preceding the segment's first artifact is discarded: commentary with no
/// citation to qualify is not evidence.
fn scan_segment(segment: &str, depth: usize, out: &mut Vec<EvidenceCitation>) {
    let mut current: Option<usize> = None;
    let mut fragments: Vec<String> = Vec::new();

    for token in tokenize(segment) {
        match token {
            Token::Word(word) => match classify_word(word) {
                Some((artifact, field)) => {
                    flush_selector(&mut current, &mut fragments, out);
                    out.push(EvidenceCitation {
                        artifact,
                        selector: None,
                        field,
                    });
                    current = Some(out.len() - 1);
                }
                None => push_fragment(current, word, &mut fragments),
            },
            Token::Group(inner) => {
                // A bracketed group holding its own artifact tokens is a
                // container, not commentary: descend into it. One holding no
                // artifact is a qualifier for the citation in scope.
                let mut nested = Vec::new();
                if depth < MAX_GROUP_DEPTH {
                    scan_text(inner, depth + 1, &mut nested);
                }
                if nested.is_empty() {
                    push_fragment(current, inner, &mut fragments);
                } else {
                    flush_selector(&mut current, &mut fragments, out);
                    out.append(&mut nested);
                    current = Some(out.len() - 1);
                }
            }
        }
    }

    flush_selector(&mut current, &mut fragments, out);
}

/// Record `text` as commentary for the citation in scope, dropping it when no
/// citation is in scope or when it normalises to nothing.
fn push_fragment(current: Option<usize>, text: &str, fragments: &mut Vec<String>) {
    if current.is_none() {
        return;
    }
    let fragment = normalize_commentary(text);
    if !fragment.is_empty() {
        fragments.push(fragment);
    }
}

/// Attach the accumulated commentary to the citation in scope and clear it.
///
/// Fragments join with a single space in source order; an existing selector is
/// extended rather than overwritten, so no commentary is silently lost.
fn flush_selector(
    current: &mut Option<usize>,
    fragments: &mut Vec<String>,
    out: &mut Vec<EvidenceCitation>,
) {
    let Some(index) = current.take() else {
        fragments.clear();
        return;
    };
    let joined = fragments.join(" ").trim().to_string();
    fragments.clear();
    if joined.is_empty() {
        return;
    }
    let citation = &mut out[index];
    citation.selector = match citation.selector.take() {
        Some(existing) => Some(format!("{existing} {joined}")),
        None => Some(joined),
    };
}

/// Split on separators that appear at bracket depth zero, keeping bracketed
/// spans (and every separator inside them) intact.
fn split_top_level(text: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut depth: usize = 0;
    let mut idx = 0usize;

    while idx < text.len() {
        let tail = &text[idx..];
        let Some(ch) = tail.chars().next() else { break };
        let ch_len = ch.len_utf8();

        if GROUP_OPENERS.contains(&ch) {
            depth += 1;
        } else if GROUP_CLOSERS.contains(&ch) {
            depth = depth.saturating_sub(1);
        } else if depth == 0 {
            if matches!(ch, ';' | ',' | '\n' | '\r') {
                segments.push(std::mem::take(&mut current));
                idx += ch_len;
                continue;
            }
            if ch.is_whitespace() {
                if let Some(conjunction) = CONJUNCTION_RE.find(tail) {
                    segments.push(std::mem::take(&mut current));
                    idx += conjunction.end();
                    continue;
                }
            }
        }

        current.push(ch);
        idx += ch_len;
    }

    segments.push(current);
    segments
}

/// A whitespace-delimited word, or the inner text of a bracketed group.
enum Token<'a> {
    Word(&'a str),
    Group(&'a str),
}

/// Tokenise one segment: whitespace at depth zero ends a word, a bracket at
/// depth zero starts a group that runs to its balancing close (or to the end of
/// the segment when the agent left it unclosed).
fn tokenize(segment: &str) -> Vec<Token<'_>> {
    let mut tokens = Vec::new();
    let mut word_start: Option<usize> = None;
    let mut idx = 0usize;

    while idx < segment.len() {
        let Some(ch) = segment[idx..].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();

        if GROUP_OPENERS.contains(&ch) {
            if let Some(start) = word_start.take() {
                tokens.push(Token::Word(&segment[start..idx]));
            }
            let inner_start = idx + ch_len;
            let (inner_end, after) = find_group_end(segment, inner_start);
            tokens.push(Token::Group(&segment[inner_start..inner_end]));
            idx = after;
            continue;
        }

        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                tokens.push(Token::Word(&segment[start..idx]));
            }
            idx += ch_len;
            continue;
        }

        if word_start.is_none() {
            word_start = Some(idx);
        }
        idx += ch_len;
    }

    if let Some(start) = word_start {
        tokens.push(Token::Word(&segment[start..]));
    }
    tokens
}

/// Locate the balancing close bracket for a group whose inner text starts at
/// `inner_start`. Returns `(end of inner text, index just past the close)`;
/// an unclosed group ends at the end of the segment.
fn find_group_end(segment: &str, inner_start: usize) -> (usize, usize) {
    let mut depth = 1usize;
    let mut idx = inner_start;
    while idx < segment.len() {
        let Some(ch) = segment[idx..].chars().next() else {
            break;
        };
        let ch_len = ch.len_utf8();
        if GROUP_OPENERS.contains(&ch) {
            depth += 1;
        } else if GROUP_CLOSERS.contains(&ch) {
            depth -= 1;
            if depth == 0 {
                return (idx, idx + ch_len);
            }
        }
        idx += ch_len;
    }
    (segment.len(), segment.len())
}

/// Decide whether a word names an artifact, returning the cleaned artifact and
/// its `::` field path when it does.
///
/// Returns `None` for ordinary prose, which is what keeps the parser from
/// fabricating a citation out of a sentence that names no file.
fn classify_word(word: &str) -> Option<(String, Option<String>)> {
    let cleaned = trim_noise(word);
    if cleaned.is_empty() {
        return None;
    }

    let (base_raw, field_raw) = match cleaned.split_once("::") {
        Some((base, field)) => (base, Some(field)),
        None => (cleaned, None),
    };

    let base = trim_noise(base_raw);
    if !is_artifact_reference(base) {
        return None;
    }
    let artifact = base.trim_end_matches('/').to_string();
    if artifact.is_empty() {
        return None;
    }

    let field = field_raw
        .map(trim_noise)
        .filter(|field| !field.is_empty())
        .map(str::to_string);

    Some((artifact, field))
}

/// A token is an artifact reference when its last path component carries a
/// recognised extension, or — extension-less — when it is shaped like a path.
fn is_artifact_reference(token: &str) -> bool {
    let path = token.trim_end_matches('/');
    if path.is_empty() {
        return false;
    }
    let last_component = path.rsplit('/').next().unwrap_or(path);
    if has_recognised_extension(last_component) {
        return true;
    }
    path.contains('/')
}

/// Whether any dot-separated suffix of `component` is a recognised extension.
///
/// Every suffix is checked, not just the last, so a doubly-suffixed artifact is
/// still recognised. A leading dot is not an extension: a dotfile name alone
/// does not make a reference.
fn has_recognised_extension(component: &str) -> bool {
    component.split('.').skip(1).any(|suffix| {
        EVIDENCE_ARTIFACT_EXTENSIONS
            .iter()
            .any(|known| suffix.eq_ignore_ascii_case(known))
    })
}

/// Strip surrounding punctuation and quoting from a candidate artifact token.
fn trim_noise(token: &str) -> &str {
    token
        .trim_start_matches(|ch: char| LEADING_NOISE.contains(&ch) || ch.is_whitespace())
        .trim_end_matches(|ch: char| TRAILING_NOISE.contains(&ch) || ch.is_whitespace())
}

/// Normalise a commentary span: collapse whitespace runs to one space and trim
/// quoting plus sentence punctuation from both ends.
fn normalize_commentary(text: &str) -> String {
    let collapsed = WHITESPACE_RUN_RE.replace_all(text.trim(), " ");
    collapsed
        .trim_matches(|ch: char| COMMENTARY_TRIM.contains(&ch))
        .to_string()
}

/// Drop citations identical in all three fields, keeping the first occurrence.
///
/// Deduplication is exact: two references to one artifact under different
/// selectors are two distinct citations and both survive.
fn dedup_preserving_order(citations: Vec<EvidenceCitation>) -> Vec<EvidenceCitation> {
    let mut seen: BTreeSet<(String, Option<String>, Option<String>)> = BTreeSet::new();
    let mut unique = Vec::with_capacity(citations.len());
    for citation in citations {
        let key = (
            citation.artifact.clone(),
            citation.selector.clone(),
            citation.field.clone(),
        );
        if seen.insert(key) {
            unique.push(citation);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(artifact: &str, selector: Option<&str>, field: Option<&str>) -> EvidenceCitation {
        EvidenceCitation {
            artifact: artifact.to_string(),
            selector: selector.map(str::to_string),
            field: field.map(str::to_string),
        }
    }

    #[test]
    fn parenthetical_selector_and_field_pointer_split() {
        // The production failure: one compound string read as one filename.
        let parsed = parse_evidence_citations(
            "de_results.tsv (padj column); de_summary.json::n_significant",
        );
        assert_eq!(
            parsed,
            vec![
                cite("de_results.tsv", Some("padj column"), None),
                cite("de_summary.json", None, Some("n_significant")),
            ],
            "compound citation must split into artifact+selector and artifact+field"
        );
    }

    #[test]
    fn trailing_words_become_the_selector() {
        let parsed = parse_evidence_citations(
            "de_results.tsv row ENSG00000109906; de_summary.json::top_gene_up",
        );
        assert_eq!(
            parsed,
            vec![
                cite("de_results.tsv", Some("row ENSG00000109906"), None),
                cite("de_summary.json", None, Some("top_gene_up")),
            ],
            "an unbracketed trailing qualifier must attach as a selector, not a second artifact"
        );
    }

    #[test]
    fn bare_artifact_has_no_selector_or_field() {
        assert_eq!(
            parse_evidence_citations("de_results.tsv"),
            vec![cite("de_results.tsv", None, None)],
            "a lone artifact must parse to exactly one plain citation"
        );
    }

    #[test]
    fn comma_separated_paths_preserved() {
        assert_eq!(
            parse_evidence_citations("results/tables/x.tsv, results/tables/y.tsv"),
            vec![
                cite("results/tables/x.tsv", None, None),
                cite("results/tables/y.tsv", None, None),
            ],
            "a depth-zero comma splits, and relative paths survive verbatim"
        );
    }

    #[test]
    fn comma_inside_parenthetical_does_not_split() {
        assert_eq!(
            parse_evidence_citations("de_results.tsv (padj, log2FoldChange columns)"),
            vec![cite(
                "de_results.tsv",
                Some("padj, log2FoldChange columns"),
                None
            )],
            "a comma inside a qualifier is commentary, not a separator"
        );
    }

    #[test]
    fn dotted_field_path_kept_whole() {
        assert_eq!(
            parse_evidence_citations("summary.json::counts.n_retained"),
            vec![cite("summary.json", None, Some("counts.n_retained"))],
            "dots after `::` belong to the field path, not to a file extension"
        );
    }

    #[test]
    fn prose_without_an_artifact_yields_nothing() {
        assert!(
            parse_evidence_citations("").is_empty(),
            "empty evidence must not fabricate a citation"
        );
        assert!(
            parse_evidence_citations("see the attached figure").is_empty(),
            "prose naming no file must not fabricate a citation"
        );
    }

    #[test]
    fn exact_repeats_deduplicate() {
        assert_eq!(
            parse_evidence_citations("a.tsv; a.tsv"),
            vec![cite("a.tsv", None, None)],
            "an exact repeat must collapse to one citation"
        );
    }

    #[test]
    fn sentence_punctuation_never_joins_the_artifact() {
        assert_eq!(
            parse_evidence_citations("(see de_results.tsv)."),
            vec![cite("de_results.tsv", None, None)],
            "a bracketed aside holding an artifact is a container, not a selector"
        );
    }

    #[test]
    fn distinct_selectors_on_one_artifact_both_survive() {
        assert_eq!(
            parse_evidence_citations("a.tsv (col x); a.tsv (col y)"),
            vec![
                cite("a.tsv", Some("col x"), None),
                cite("a.tsv", Some("col y"), None),
            ],
            "deduplication is exact over all three fields, not artifact-only"
        );
    }

    #[test]
    fn conjunction_separators_split() {
        assert_eq!(
            parse_evidence_citations("a.tsv (col x) and b.json + c.csv"),
            vec![
                cite("a.tsv", Some("col x"), None),
                cite("b.json", None, None),
                cite("c.csv", None, None),
            ],
            "` and ` / ` + ` are separators and must not leak into a selector"
        );
    }

    #[test]
    fn conjunction_inside_a_word_does_not_split() {
        assert_eq!(
            parse_evidence_citations("sand.tsv android.json"),
            vec![
                cite("sand.tsv", None, None),
                cite("android.json", None, None)
            ],
            "`and` only separates when whitespace-delimited on both sides"
        );
    }

    #[test]
    fn newline_and_whitespace_runs_split() {
        assert_eq!(
            parse_evidence_citations("a.tsv\n\nb.tsv   c.tsv"),
            vec![
                cite("a.tsv", None, None),
                cite("b.tsv", None, None),
                cite("c.tsv", None, None),
            ],
            "newlines and whitespace runs both separate adjacent artifacts"
        );
    }

    #[test]
    fn only_separators_yields_nothing() {
        for raw in [";", ",,,", " ; , and + ; ", "\n\n", "()", "( ; )", "::"] {
            assert!(
                parse_evidence_citations(raw).is_empty(),
                "separator-only input {raw:?} must yield no citations"
            );
        }
    }

    #[test]
    fn empty_field_after_double_colon_is_no_field() {
        assert_eq!(
            parse_evidence_citations("summary.json::"),
            vec![cite("summary.json", None, None)],
            "a dangling `::` must leave the artifact intact with no field"
        );
        assert!(
            parse_evidence_citations("::n_significant").is_empty(),
            "a field pointer with no base file names no artifact"
        );
    }

    #[test]
    fn nested_parens_without_an_artifact_stay_commentary() {
        assert_eq!(
            parse_evidence_citations("a.tsv ((padj) column)"),
            vec![cite("a.tsv", Some("(padj) column"), None)],
            "a qualifier containing no artifact stays commentary at every depth"
        );
    }

    #[test]
    fn nested_brackets_holding_an_artifact_are_descended() {
        assert_eq!(
            parse_evidence_citations("[(results/x.tsv)]"),
            vec![cite("results/x.tsv", None, None)],
            "nested containers must be descended, not read as commentary"
        );
    }

    #[test]
    fn pathological_nesting_is_bounded_and_panic_free() {
        let deep = format!("{}x.tsv{}", "(".repeat(256), ")".repeat(256));
        assert!(
            parse_evidence_citations(&deep).is_empty(),
            "nesting past the depth bound degrades to commentary rather than recursing"
        );
        let unclosed = format!("{}a.tsv", "(".repeat(3));
        assert_eq!(
            parse_evidence_citations(&unclosed),
            vec![cite("a.tsv", None, None)],
            "an unclosed group still yields its artifact"
        );
    }

    #[test]
    fn extension_matching_is_case_insensitive_and_allowlisted() {
        assert_eq!(
            parse_evidence_citations("REPORT.PDF"),
            vec![cite("REPORT.PDF", None, None)],
            "extension recognition ignores ASCII case and preserves the name as written"
        );
        assert!(
            parse_evidence_citations("version.9 build.exe").is_empty(),
            "an extension outside the allowlist is not an artifact"
        );
    }

    #[test]
    fn extension_less_reference_requires_path_syntax() {
        assert_eq!(
            parse_evidence_citations("runtime/outputs/summary"),
            vec![cite("runtime/outputs/summary", None, None)],
            "an extension-less token containing `/` is a path reference"
        );
        assert!(
            parse_evidence_citations("summary").is_empty(),
            "an extension-less token with no `/` is prose"
        );
        assert!(
            parse_evidence_citations("outputs/").is_empty(),
            "a trailing slash alone does not make path syntax"
        );
    }

    #[test]
    fn relative_and_dotfile_prefixes_survive_trimming() {
        assert_eq!(
            parse_evidence_citations("./out/a.tsv, /abs/b.json"),
            vec![
                cite("./out/a.tsv", None, None),
                cite("/abs/b.json", None, None),
            ],
            "a leading `.` or `/` is path syntax, not punctuation to strip"
        );
    }

    #[test]
    fn quoting_and_markdown_emphasis_stripped() {
        assert_eq!(
            parse_evidence_citations("`a.tsv`, **b.json**, \"c.csv\""),
            vec![
                cite("a.tsv", None, None),
                cite("b.json", None, None),
                cite("c.csv", None, None),
            ],
            "agent markup around a citation must not enter the artifact name"
        );
    }

    #[test]
    fn commentary_before_the_first_artifact_is_discarded() {
        assert_eq!(
            parse_evidence_citations("as shown in a.tsv row 3"),
            vec![cite("a.tsv", Some("row 3"), None)],
            "text with no citation in scope is not commentary and is dropped"
        );
    }

    #[test]
    fn field_pointer_survives_surrounding_punctuation() {
        assert_eq!(
            parse_evidence_citations("(summary.json::counts.n_retained)."),
            vec![cite("summary.json", None, Some("counts.n_retained"))],
            "a bracketed field pointer must keep its field and shed its punctuation"
        );
    }

    #[test]
    fn selector_whitespace_is_normalised() {
        assert_eq!(
            parse_evidence_citations("a.tsv (col\n  x)"),
            vec![cite("a.tsv", Some("col x"), None)],
            "a selector spanning a line break normalises to its single-line form"
        );
    }

    #[test]
    fn artifact_only_constructor_is_verbatim() {
        assert_eq!(
            EvidenceCitation::artifact_only("results/x.tsv"),
            cite("results/x.tsv", None, None),
            "the convenience constructor stores the name as given"
        );
    }
}
