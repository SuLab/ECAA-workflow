//! Re-export of the deterministic `ro-crate-preview.html` renderer from
//! `ecaa_workflow_core::preview`.
//!
//! The implementation lives in `crates/core/src/preview.rs` so it can be
//! called from `finalize_evidence_registration_with_verifier` in
//! `crates/core/src/ro_crate.rs` (the last step before the BagIt reseal).
//! This module re-exports the public API so conversation-layer callers and
//! tests can reference it via the conversation crate's emit path.

pub use ecaa_workflow_core::preview::{render_ro_crate_preview, write_ro_crate_preview};

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke-test the re-export: the function is callable and produces valid HTML.
    #[test]
    fn reexport_render_is_callable() {
        let meta = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "name": "Reexport Test",
                    "description": "Checks the re-export compiles and works.",
                    "conformsTo": [{"@id": "https://w3id.org/ro/crate/1.1"}],
                    "hasPart": [{"@id": "README.md"}]
                },
                {
                    "@id": "README.md",
                    "@type": "File",
                    "name": "README",
                    "encodingFormat": "text/markdown"
                }
            ]
        });
        let html = render_ro_crate_preview(&meta);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<script type=\"application/ld+json\">"));
        assert!(html.contains("Reexport Test"));
        assert!(!html.to_lowercase().contains("<script>"), "no executable JS");
    }

    /// Ensure the re-exported write function works end-to-end.
    #[test]
    fn reexport_write_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let meta = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "name": "Write Test"
                }
            ]
        });
        write_ro_crate_preview(dir.path(), &meta).unwrap();
        let path = dir.path().join("ro-crate-preview.html");
        assert!(path.exists(), "ro-crate-preview.html must be created");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Write Test"));
    }
}
