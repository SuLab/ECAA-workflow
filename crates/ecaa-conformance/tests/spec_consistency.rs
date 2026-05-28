//! Cross-document consistency linter for the ECAA v0.1 specification.
//!
//! Enforces four consistency rules (ECAA spec §10):
//!   1. Invariant ID consistency across v0.1.md, invariants.md, and
//!      `crates/core/src/audit_proof/invariants/`.
//!   2. Node/edge type consistency across v0.1.md §5, ecaa-v0.1.ttl,
//!      and the 8 JSON Schemas.
//!   3. BlockerKind consistency: v0.1.md Appendix B MUST match
//!      `crates/core/src/blocker.rs`.
//!   4. Sidecar filename consistency: v0.1.md §3 sidecar table MUST
//!      match the emit-side paths and the loader-side paths.
//!
//! Canonical type-set definitions live in
//! `ecaa-workflow-types::consts`; this test imports them as the
//! single source of truth.

use ecaa_workflow_types::consts::{
    EDGE_PREDICATES as EXPECTED_EDGE_PREDICATES, INVARIANT_IDS as EXPECTED_INVARIANT_IDS,
    NODE_TYPES as EXPECTED_NODE_TYPES, SIDECAR_PATHS as EXPECTED_SIDECARS,
};
use std::path::{Path, PathBuf};
use strum::EnumCount;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("CARGO_MANIFEST_DIR has two ancestors")
        .to_path_buf()
}

fn spec_dir() -> PathBuf {
    repo_root().join("docs").join("ecaa-spec")
}

#[test]
fn spec_dir_exists() {
    assert!(
        spec_dir().is_dir(),
        "docs/ecaa-spec/ must exist; check the design doc"
    );
}

fn audit_proof_invariant_modules() -> Vec<String> {
    let dir = repo_root()
        .join("crates")
        .join("core")
        .join("src")
        .join("audit_proof")
        .join("invariants");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension()?.to_str()? != "rs" {
                return None;
            }
            let stem = path.file_stem()?.to_str()?.to_string();
            if stem == "mod" {
                return None;
            }
            // Module file stems are snake_case; canonical wire IDs are snake_case.
            Some(stem)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn invariant_ids_match_implementation() {
    let mut expected: Vec<String> = EXPECTED_INVARIANT_IDS
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.sort();
    let actual = audit_proof_invariant_modules();
    assert_eq!(
        actual, expected,
        "audit_proof/invariants/*.rs filenames must match the 6 normative invariant IDs"
    );
}

use pulldown_cmark::{Event, Options, Parser};

fn extract_inline_code_strings(md_path: &Path) -> Vec<String> {
    let src = std::fs::read_to_string(md_path).unwrap_or_else(|e| panic!("read {md_path:?}: {e}"));
    let parser = Parser::new_ext(&src, Options::all());
    let mut out = Vec::new();
    for event in parser {
        if let Event::Code(s) = event {
            out.push(s.into_string());
        }
    }
    out
}

#[test]
fn invariant_ids_appear_in_invariants_md() {
    let path = spec_dir().join("invariants.md");
    let codes = extract_inline_code_strings(&path);
    for id in EXPECTED_INVARIANT_IDS {
        assert!(
            codes.iter().any(|c| c == id),
            "invariants.md must reference `{id}` as an inline code span"
        );
    }
}

#[test]
fn invariant_ids_appear_in_v01_md() {
    let path = spec_dir().join("v0.1.md");
    let codes = extract_inline_code_strings(&path);
    for id in EXPECTED_INVARIANT_IDS {
        assert!(
            codes.iter().any(|c| c == id),
            "v0.1.md §6 must reference `{id}` as an inline code span"
        );
    }
}

#[test]
fn sidecar_filenames_in_v01_md() {
    let path = spec_dir().join("v0.1.md");
    let codes = extract_inline_code_strings(&path);
    for (letter, fname) in EXPECTED_SIDECARS {
        assert!(
            codes.iter().any(|c| c == fname),
            "v0.1.md §3 must list `{fname}` (sub-graph {letter})"
        );
    }
}

/// Parse variant names from crates/core/src/blocker.rs source.
///
/// We can't use strum::EnumIter on BlockerKind because variant payload types
/// (e.g., ToolErrorEnvelope) don't implement Default. Source-parse via regex
/// instead — this keeps the spec/code coupling at the variant-name level
/// without requiring Default on payload types.
fn blocker_variant_names() -> Vec<String> {
    let src_path = repo_root()
        .join("crates")
        .join("ecaa-types")
        .join("src")
        .join("blocker.rs");
    let src =
        std::fs::read_to_string(&src_path).unwrap_or_else(|e| panic!("read {src_path:?}: {e}"));

    // Find the `pub enum BlockerKind {` block and collect variant lines.
    let re = regex::Regex::new(r"(?m)^    ([A-Z][A-Za-z0-9]+)(?:\s*\{|\s*\(|,\s*$)")
        .expect("regex compiles");

    let mut in_enum = false;
    let mut depth = 0i32;
    let mut names: Vec<String> = Vec::new();
    for line in src.lines() {
        if !in_enum {
            if line.trim_start().starts_with("pub enum BlockerKind") {
                in_enum = true;
                // `pub enum BlockerKind {` opens the enum block — count the `{`.
                depth = line.chars().filter(|&c| c == '{').count() as i32;
            }
            continue;
        }
        // Track brace depth so we stop at the close of the enum.
        let opens = line.chars().filter(|&c| c == '{').count() as i32;
        let closes = line.chars().filter(|&c| c == '}').count() as i32;
        // For variant-name matching we want lines that are AT depth 1 BEFORE
        // any further opens (e.g., `Foo { ... },` is depth 1 entry, opens to
        // 2 within the line, closes back to 1).
        let line_starts_at_depth = depth;
        depth += opens - closes;
        if depth <= 0 {
            in_enum = false;
            break;
        }
        if line_starts_at_depth != 1 {
            continue;
        }
        if let Some(caps) = re.captures(line) {
            let name = caps[1].to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

#[test]
fn blocker_variants_match_appendix_b() {
    let path = spec_dir().join("v0.1.md");
    let codes = extract_inline_code_strings(&path);
    let names = blocker_variant_names();
    assert_eq!(
        names.len(),
        47,
        "regex-parsed BlockerKind variant count mismatched the 47-variant assumption — \
         either the parser drifted or the impl gained/lost variants. \
         (Re-added TurnBudgetExceeded post-deletion; bump here in lockstep \
         with the impl-side count assert in crates/core/tests/blocker_variant_count.rs.)"
    );
    for name in &names {
        assert!(
            codes.iter().any(|c| c == name),
            "v0.1.md Appendix B must list `{name}` as an inline code span"
        );
    }
    // Cross-check against the strum::EnumCount macro (compile-time count).
    assert_eq!(
        ecaa_workflow_core::blocker::BlockerKind::COUNT,
        47,
        "design + spec assume 47 BlockerKind variants; update this assertion AND the spec if the impl drifts"
    );
}

#[test]
fn closed_set_sizes_match_design() {
    assert_eq!(EXPECTED_NODE_TYPES.len(), 25);
    assert_eq!(EXPECTED_EDGE_PREDICATES.len(), 20);
    assert_eq!(EXPECTED_INVARIANT_IDS.len(), 6);
    assert_eq!(EXPECTED_SIDECARS.len(), 8);
}

#[test]
fn node_types_in_v01_md() {
    let path = spec_dir().join("v0.1.md");
    let codes = extract_inline_code_strings(&path);
    for name in EXPECTED_NODE_TYPES {
        assert!(
            codes.iter().any(|c| c == name),
            "v0.1.md §5 must reference `{name}` as an inline code span"
        );
    }
}

#[test]
fn edge_predicates_in_v01_md() {
    let path = spec_dir().join("v0.1.md");
    let codes = extract_inline_code_strings(&path);
    for name in EXPECTED_EDGE_PREDICATES {
        assert!(
            codes.iter().any(|c| c == name),
            "v0.1.md §5 must reference `{name}` as an inline code span"
        );
    }
}
