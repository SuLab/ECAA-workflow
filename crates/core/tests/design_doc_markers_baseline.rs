//! Asserts the design-doc `[partial]` / `[planned]` marker set has not grown
//! beyond the §0.2 carry-forward list. New markers must be added to the
//! allowlist OR ratified via dated ADR.
//!
//! Closure plan Step C.17 — Every
//! prose-level `[partial]` / `[planned]` marker in `docs/dag_design_v3.md`
//! and `docs/dag_design_v4.md` outside §0.1 / §0.4 table rows must either
//! map to a §0.2 deferral (allowlist entry below) or be flipped to
//! promoted to `[done]` with a code anchor.
//!
//! The check is by-substring against `ALLOWED_MARKERS`. Lines beginning
//! with `- **[` (the legend bullets at the top of each doc) are filtered.

use std::collections::BTreeSet;
use std::path::Path;

const ALLOWED_MARKERS: &[&str] = &[
    // §0.2 carry-forwards (one entry per ratified deferral)
    "External backend emitters",
    "External registry importers",
    "LLM-side embedding-based duplicate detection",
    "Upstream-ontology automated PR generation",
    "Repair-strategy chaining",
    "Full structured-form replacement",
    "Const generics for variable-dimension",
    "Phantom-typed serialization escape hatches",
    "LLM-repair retry loop on schema-invalid intent",
];

fn extract_marker_contexts(doc_path: &Path) -> BTreeSet<String> {
    let text = std::fs::read_to_string(doc_path).unwrap_or_else(|e| {
        panic!("failed to read {}: {}", doc_path.display(), e);
    });
    let mut out = BTreeSet::new();
    for line in text.lines() {
        if (line.contains("[partial]") || line.contains("[planned]"))
            && !line.trim().starts_with("- **[")
        {
            out.insert(line.trim().to_string());
        }
    }
    out
}

fn marker_matches_allowlist(line: &str) -> bool {
    ALLOWED_MARKERS.iter().any(|m| line.contains(m))
}

#[test]
#[ignore = "docs/dag_design_v3.md not in OSS repo"]
fn v3_design_doc_markers_in_allowlist() {
    let markers = extract_marker_contexts(Path::new("../../docs/dag_design_v3.md"));
    let violations: Vec<_> = markers
        .iter()
        .filter(|l| !marker_matches_allowlist(l))
        .collect();
    assert!(
        violations.is_empty(),
        "v3 design doc has [partial]/[planned] markers outside the §0.2 allowlist:\n{:#?}\nIf a new deferral is genuine, add it to ALLOWED_MARKERS *and* author a dated ADR under docs/adr/.",
        violations
    );
}

#[test]
#[ignore = "docs/dag_design_v4.md not in OSS repo"]
fn v4_design_doc_markers_in_allowlist() {
    let markers = extract_marker_contexts(Path::new("../../docs/dag_design_v4.md"));
    let violations: Vec<_> = markers
        .iter()
        .filter(|l| !marker_matches_allowlist(l))
        .collect();
    assert!(
        violations.is_empty(),
        "v4 design doc has [partial]/[planned] markers outside the §0.2 allowlist:\n{:#?}",
        violations
    );
}
