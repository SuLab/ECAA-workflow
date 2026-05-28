//! Path-hint extraction for SME intake prose.
//!
//! When the SME types "the CSV is at testdata/scenarios/.../monthly_admissions.csv"
//! the compiler used to ignore the path: it lived only in the prose, never
//! made it into `runtime/inputs.json`, and the harness-dispatched agent
//! fell back to fabricating synthetic data. This module scans every chunk
//! of intake prose for filesystem-shaped tokens, validates them against
//! the same allowlist the `register_input_path` endpoint enforces, and
//! returns a set of structured hints the conversation service stashes on
//! `Session::pending_input_hints`.
//!
//! The hints are surfaced to the LLM via `get_session_state` and to the
//! UI via `SessionStateSnapshot.pending_input_hints`; the LLM is prompted
//! (see `prompt_role.txt`) to offer registration to the SME via plain
//! language rather than silently committing. When
//! `ECAA_AUTO_REGISTER_PROSE_PATHS=1` the conversation service registers
//! every validated hint automatically — useful for non-interactive
//! fixture runs where there's no SME loop to confirm.
//!
//! Security posture:
//!
//! - **Validate before exposing.** A path is surfaced only when (a) it
//!   resolves to an existing file or directory, (b) it canonicalizes
//!   under one of the `ECAA_INPUT_ROOTS` allowlisted roots, and (c) the
//!   file extension is in the recognized-data-format set. A SME
//!   pasting `/etc/passwd` produces no hint.
//! - **No prose-driven write paths.** Hints are read-only metadata; the
//!   conversation service uses the existing `register_input_path`
//!   surface to commit them, which runs its own jail.
//! - **Relative paths resolve against the allowlist.** A bare `data/foo.csv`
//!   resolves against each allowlist root in turn. If none match, the
//!   hint is dropped. Relative paths that escape via `..` are dropped
//!   at canonicalize time.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use ts_rs::TS;

/// Recognised data-format file extensions. Used to gate path
/// extraction so a stray `report.html` mention doesn't surface as an
/// input hint. Lowercase; matched against the lowercased extension
/// of the candidate file.
const DATA_EXTENSIONS: &[&str] = &[
    // Tabular text
    "csv",
    "tsv",
    "txt",
    "tab",
    // Compressed tabular / generic
    "gz",
    "bz2",
    "zip",
    "xz",
    // Sequencing / bioinformatics binary
    "bam",
    "sam",
    "cram",
    "vcf",
    "bcf",
    "bed",
    "bedpe",
    "narrowpeak",
    "broadpeak",
    "gff",
    "gtf",
    "fasta",
    "fa",
    "fastq",
    "fq",
    // Matrices / structured
    "mtx",
    "h5",
    "h5ad",
    "h5mu",
    "loom",
    "zarr",
    "rds",
    "rdata",
    "parquet",
    "feather",
    "arrow",
    "npy",
    "npz",
    // Structured / config / generic data
    "json",
    "jsonl",
    "ndjson",
    "yaml",
    "yml",
    "toml",
    // Clinical / tabular special
    "sas7bdat",
    "xpt",
    "xlsx",
    "xls",
];

/// One extractor-surfaced candidate. Carries enough information for
/// the conversation service to call `register_input_path` and for the
/// UI to render a "Register?" affordance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct InputPathHint {
    /// Verbatim token taken from prose. Surfaced so the SME sees
    /// the same string they typed.
    pub raw_mention: String,
    /// Absolute canonicalized path of the directory the SME would
    /// register. When the prose pointed at a file we surface its
    /// parent directory here, since the `register_input_path` endpoint
    /// only accepts directories.
    pub canonical_root: String,
    /// File extension (lowercase, sans dot) that triggered acceptance.
    pub matched_extension: String,
    /// True when the prose pointed at a file (we resolved its parent);
    /// false when the prose pointed at the directory directly. Surfaced
    /// so the LLM can phrase the suggestion ("I see you mentioned the
    /// file X — would you like to register its directory Y?").
    pub file_mention: bool,
    /// When `Some`, the relative filename inside `canonical_root` the
    /// SME named. Lets downstream tooling pin the specific file the
    /// SME meant rather than re-walking the whole directory.
    pub file_relpath: Option<String>,
}

/// Extract every distinct filesystem-shaped token from `prose` that
/// (a) parses as a path, (b) canonicalizes to an existing file or
/// directory, (c) lives under one of `allowlisted_roots`, and (d) has
/// a recognized data extension.
///
/// Returns deduplicated hints, preserving the order of first
/// occurrence in the prose. Empty when no token survives validation.
pub fn extract_path_hints(prose: &str, allowlisted_roots: &[PathBuf]) -> Vec<InputPathHint> {
    if allowlisted_roots.is_empty() {
        return Vec::new();
    }
    let mut out: Vec<InputPathHint> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for token in tokenize_path_candidates(prose) {
        // Reject obvious non-paths early so we don't burn canonicalize
        // calls on every gene symbol or accession.
        if !looks_like_path(&token) {
            continue;
        }
        let Some(hint) = validate_candidate(&token, allowlisted_roots) else {
            continue;
        };
        // Dedup on canonical_root + file_relpath so two mentions of
        // the same file don't double-list. SME prose often repeats
        // the path across paragraphs.
        let key = format!(
            "{}|{}",
            hint.canonical_root,
            hint.file_relpath.as_deref().unwrap_or("")
        );
        if seen.insert(key) {
            out.push(hint);
        }
    }
    out
}

/// Walk `prose` and yield whitespace- and punctuation-trimmed tokens
/// that could be a path. We split on whitespace because real
/// filesystem paths don't contain unescaped spaces in any context the
/// SME would type without quoting, and quoted spaces are stripped by
/// the sanitizer upstream.
fn tokenize_path_candidates(prose: &str) -> Vec<String> {
    prose
        .split(|c: char| c.is_whitespace())
        .filter_map(|raw| {
            // Strip common surrounding punctuation. SMEs often write
            // "the data at `foo/bar.csv`," or "see foo/bar.csv." or
            // "(foo/bar.csv)". Don't strip slashes, hyphens, dots,
            // underscores, or non-leading parens (some real paths
            // contain `(`).
            let trimmed = raw.trim_matches(|c: char| {
                matches!(c, ',' | ';' | ':' | '"' | '\'' | '`' | '<' | '>' | ' ')
            });
            // Strip a single trailing dot (sentence terminator) but
            // not a leading one (./relative).
            let trimmed = trimmed.strip_suffix('.').unwrap_or(trimmed);
            // Strip wrapping parens only when both ends carry them.
            let trimmed = if trimmed.starts_with('(') && trimmed.ends_with(')') {
                &trimmed[1..trimmed.len() - 1]
            } else {
                trimmed
            };
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

/// Heuristic gate. Reject tokens that obviously can't be paths
/// before calling canonicalize (which performs syscalls).
///
/// Accept:
/// - Tokens containing `/` (likely relative/absolute path).
/// - Tokens with a recognized data extension that are also
///   `./foo.csv` shaped or contain a directory component.
///
/// Reject:
/// - URLs (http(s)://, ftp://, file://, s3://, gs://, ...).
/// - Bare words (no `/` and no recognized extension).
/// - GitHub-shorthand strings like `user/repo` that have no leading
///   `./` and no recognized extension.
/// - Tokens longer than 1024 chars (path budget on most filesystems).
fn looks_like_path(token: &str) -> bool {
    if token.len() > 1024 || token.is_empty() {
        return false;
    }
    // Reject URL schemes outright. SMEs often mention accessions or
    // database URLs that aren't on the filesystem.
    let lower = token.to_ascii_lowercase();
    for scheme in [
        "http://", "https://", "ftp://", "ftps://", "file://", "s3://", "gs://", "azure://",
        "git@", "mailto:",
    ] {
        if lower.starts_with(scheme) {
            return false;
        }
    }
    // Must have a `/` OR a leading `./`/`../` OR a recognized
    // extension. Bare `foo.csv` qualifies via the extension test.
    let has_slash = token.contains('/');
    let has_relative_prefix = token.starts_with("./") || token.starts_with("../");
    let has_known_extension = lowercased_extension(token)
        .map(|ext| DATA_EXTENSIONS.contains(&ext.as_str()))
        .unwrap_or(false);
    if !(has_slash || has_relative_prefix || has_known_extension) {
        return false;
    }
    // Reject placeholder paths like `/path/to/your/data.csv` or
    // `<path>` that the LLM (or SME) might use as an example.
    if lower.contains("path/to/") || lower.contains("path/to") {
        return false;
    }
    if token.contains('<') || token.contains('>') {
        return false;
    }
    true
}

fn lowercased_extension(token: &str) -> Option<String> {
    // For composite extensions like `.vcf.gz` / `.fastq.gz` we still
    // return the trailing extension only — the recognised set includes
    // both `vcf` and `gz`, so either path will trigger acceptance.
    // Caller doesn't need to know about composite handling.
    Path::new(token)
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
}

/// Convert a candidate token into a validated `InputPathHint`, or
/// `None` if it doesn't pass all gates.
fn validate_candidate(token: &str, allowlisted_roots: &[PathBuf]) -> Option<InputPathHint> {
    let raw_mention = token.to_string();
    let candidate = PathBuf::from(token);

    // Step 1: try the token as-is. Handles absolute paths and paths
    // relative to the server's CWD (typically the repo root in dev).
    let direct_canonical = candidate.canonicalize().ok();

    // Step 2: if direct didn't resolve, try joining against each
    // allowlist root in turn. Handles SME-typed `testdata/foo.csv`
    // when the allowlist contains `/home/<user>/ecaa-workflow`.
    let resolved = direct_canonical.or_else(|| {
        for root in allowlisted_roots {
            let candidate = root.join(&candidate);
            if let Ok(c) = candidate.canonicalize() {
                return Some(c);
            }
        }
        None
    })?;

    // Step 3: must live under the allowlist. Defense in depth — even
    // if we joined relative paths against an allowlist root,
    // a symlink chain or `..` segment could push the canonicalized
    // result outside it.
    let inside_allowlist = allowlisted_roots.iter().any(|root| {
        let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
        resolved.starts_with(&root_canon)
    });
    if !inside_allowlist {
        return None;
    }

    // Step 4: file vs dir. The `register_input_path` endpoint only
    // accepts directories, so when the SME named a file we use its
    // parent as the registerable root. The file itself is kept as
    // `file_relpath` so downstream tooling can read it directly.
    let metadata = std::fs::metadata(&resolved).ok()?;
    let (registerable_root, file_relpath, file_mention) = if metadata.is_file() {
        let parent = resolved.parent()?.to_path_buf();
        // Re-canonicalize the parent so the comparison below stays
        // monotone (parent of a canonicalized path is already canonical
        // for non-trailing-slash inputs, but the explicit call avoids
        // any subtle edge case).
        let parent_canon = parent.canonicalize().unwrap_or(parent);
        let inside_after_parent_walk = allowlisted_roots.iter().any(|root| {
            let root_canon = root.canonicalize().unwrap_or_else(|_| root.clone());
            parent_canon.starts_with(&root_canon)
        });
        if !inside_after_parent_walk {
            return None;
        }
        let relpath = resolved
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string());
        (parent_canon, relpath, true)
    } else if metadata.is_dir() {
        (resolved.clone(), None, false)
    } else {
        return None;
    };

    // Step 5: recognised extension. We accept directory mentions
    // unconditionally (the SME pointing at a folder of data files is
    // explicit enough); for file mentions we require the extension to
    // be in the data set so we don't surface every random `.md` or
    // `.html` mention.
    let matched_extension = if file_mention {
        let ext = lowercased_extension(&resolved.to_string_lossy())?;
        if !DATA_EXTENSIONS.contains(&ext.as_str()) {
            return None;
        }
        ext
    } else {
        // Directory mention: extension field is reserved for files; emit
        // a sentinel so the consumer can still serialize cleanly.
        "dir".to_string()
    };

    Some(InputPathHint {
        raw_mention,
        canonical_root: registerable_root.to_string_lossy().to_string(),
        matched_extension,
        file_mention,
        file_relpath,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowlist_with(dir: &Path) -> Vec<PathBuf> {
        vec![dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())]
    }

    #[test]
    fn extracts_absolute_csv_path_under_allowlist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("monthly_admissions.csv");
        std::fs::write(&file, "month,value\n2017-01,100\n").unwrap();
        let prose = format!(
            "I have a CSV at {} with monthly admissions data.",
            file.display()
        );
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1, "should extract one path");
        let h = &hints[0];
        assert!(h.file_mention);
        assert_eq!(h.matched_extension, "csv");
        assert_eq!(h.file_relpath.as_deref(), Some("monthly_admissions.csv"));
        // canonical_root is the parent dir of the file.
        assert!(
            PathBuf::from(&h.canonical_root)
                .canonicalize()
                .unwrap()
                .starts_with(tmp.path().canonicalize().unwrap()),
            "canonical_root must live under the allowlist"
        );
    }

    #[test]
    fn extracts_relative_path_when_under_allowlist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let sub = tmp.path().join("data");
        std::fs::create_dir_all(&sub).unwrap();
        let file = sub.join("counts.tsv");
        std::fs::write(&file, "gene\tcount\n").unwrap();
        // SME types `data/counts.tsv` (relative). The allowlist root
        // is the tmp dir.
        let prose = "load data/counts.tsv".to_string();
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].matched_extension, "tsv");
        assert!(hints[0].file_mention);
    }

    #[test]
    fn rejects_path_outside_allowlist() {
        let tmp = tempfile::TempDir::new().unwrap();
        let other = tempfile::TempDir::new().unwrap();
        let file = other.path().join("secret.csv");
        std::fs::write(&file, "").unwrap();
        let prose = format!("see {}", file.display());
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert!(hints.is_empty(), "must reject paths outside allowlist");
    }

    #[test]
    fn rejects_url_mentions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prose = "fetch https://example.com/data.csv";
        let hints = extract_path_hints(prose, &allowlist_with(tmp.path()));
        assert!(hints.is_empty(), "URL must not be treated as a path");
    }

    #[test]
    fn rejects_placeholder_paths() {
        let tmp = tempfile::TempDir::new().unwrap();
        let prose = "the file is at /path/to/your/data.csv".to_string();
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert!(
            hints.is_empty(),
            "placeholder /path/to/ paths must be dropped"
        );
    }

    #[test]
    fn rejects_unknown_file_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("readme.md");
        std::fs::write(&file, "").unwrap();
        let prose = format!("see {}", file.display());
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert!(
            hints.is_empty(),
            "non-data file extensions must not surface"
        );
    }

    #[test]
    fn accepts_directory_mention() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("counts");
        std::fs::create_dir_all(&dir).unwrap();
        let prose = format!("data lives in {}", dir.display());
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1);
        assert!(!hints[0].file_mention);
        assert_eq!(hints[0].matched_extension, "dir");
        assert!(hints[0].file_relpath.is_none());
    }

    #[test]
    fn dedupes_repeated_mentions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("admissions.csv");
        std::fs::write(&file, "").unwrap();
        let prose = format!(
            "I have {} with admissions. Note that {} is also the only file.",
            file.display(),
            file.display()
        );
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1, "duplicate mentions must dedupe");
    }

    #[test]
    fn strips_trailing_punctuation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("data.csv");
        std::fs::write(&file, "").unwrap();
        // SME ends the sentence; comma in the next clause.
        let prose = format!(
            "see {}, which has columns month and admissions. Also see {}.",
            file.display(),
            file.display()
        );
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1);
    }

    #[test]
    fn empty_allowlist_yields_no_hints() {
        let prose = "see /tmp/foo.csv";
        let hints = extract_path_hints(prose, &[]);
        assert!(
            hints.is_empty(),
            "no allowlist means no extraction (would otherwise surface arbitrary paths)"
        );
    }

    #[test]
    fn rejects_path_traversal_via_relative_segments() {
        let tmp = tempfile::TempDir::new().unwrap();
        // Try to escape via `../../etc/passwd`. Canonicalize collapses
        // the segments; the result `/etc/passwd` is outside tmp.
        let prose = "../../etc/passwd".to_string();
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert!(hints.is_empty(), "`..`-based escape must not surface");
    }

    #[test]
    fn handles_gzipped_data_extensions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let file = tmp.path().join("variants.vcf.gz");
        std::fs::write(&file, "").unwrap();
        let prose = format!("VCF is at {}", file.display());
        let hints = extract_path_hints(&prose, &allowlist_with(tmp.path()));
        assert_eq!(hints.len(), 1, "vcf.gz must be accepted");
        // Trailing extension is `gz` (composite handling is intentional).
        assert_eq!(hints[0].matched_extension, "gz");
    }
}
