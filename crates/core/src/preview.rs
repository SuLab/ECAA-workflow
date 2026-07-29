//! Deterministic, offline, zero-JS `ro-crate-preview.html` renderer.
//!
//! Pure function of the `@graph`: no clock, no RNG, no `HashMap`, no host
//! paths. Determinism contract: calling `render_ro_crate_preview` twice with
//! the same `serde_json::Value` MUST produce byte-identical output. This is
//! enforced by:
//! - Using `serde_json::to_string` (deterministic for the same `Value`)
//! - Iterating `hasPart` in array order (insertion order preserved by
//!   `serde_json` JSON arrays)
//! - Iterating `conformsTo` in array order
//! - No `HashMap` anywhere — only `Vec` iteration
//!
//! Spec source: RO-Crate 1.1 §10 "RO-Crate Website":
//!   "It is RECOMMENDED that the RO-Crate is accompanied by a human-readable
//!    HTML document … If an RO-Crate is to be published as a website, the
//!    RECOMMENDED file-name for the HTML file is `ro-crate-preview.html`."
//!   "The HTML page … SHOULD include the JSON-LD in a script element of type
//!    `application/ld+json`."

use serde_json::Value;
use std::path::Path;

/// Escape text for HTML body content (not attributes — no `'` escape needed
/// for body text).
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Render a deterministic, zero-JS `ro-crate-preview.html` from the given
/// `ro-crate-metadata.json` JSON-LD value.
///
/// Guarantees:
/// - Byte-identical for fixed `metadata` value (no clock/RNG/HashMap).
/// - Valid HTML5 (`<!DOCTYPE html>` first line).
/// - `<head>` contains `<script type="application/ld+json">` with the exact
///   re-serialized metadata bytes (RO-Crate MUST requirement).
/// - Zero executable JavaScript: no bare `<script>` tags.
/// - Inline `<style>` only (no external stylesheet references).
/// - HTML-escaped all interpolated text.
/// - Body renders: root `name` + `description`, Files table from root `hasPart`
///   (in array order), and `conformsTo` IRIs.
/// - Does NOT embed host temp-dir paths.
pub fn render_ro_crate_preview(metadata: &Value) -> String {
    let graph = metadata
        .get("@graph")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    // Locate the root Dataset entity (always "@id": "./").
    let root = graph
        .iter()
        .find(|e| e.get("@id").and_then(Value::as_str) == Some("./"));

    let name = root
        .and_then(|r| r.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("RO-Crate");

    let desc = root
        .and_then(|r| r.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("");

    // Build the Files table rows from root `hasPart`, in array order
    // (deterministic — serde_json preserves JSON array order).
    let mut rows = String::new();
    if let Some(parts) = root
        .and_then(|r| r.get("hasPart"))
        .and_then(Value::as_array)
    {
        for part in parts {
            let pid = part.get("@id").and_then(Value::as_str).unwrap_or("");
            // Look up the full entity for this part to get name + format.
            let ent = graph
                .iter()
                .find(|e| e.get("@id").and_then(Value::as_str) == Some(pid));
            let nm = ent
                .and_then(|e| e.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            let fmt = ent
                .and_then(|e| e.get("encodingFormat"))
                .and_then(Value::as_str)
                .unwrap_or("");
            rows.push_str(&format!(
                "<tr><td><a href=\"{pid}\">{pid_esc}</a></td><td>{nm_esc}</td><td>{fmt_esc}</td></tr>\n",
                pid = esc(pid),
                pid_esc = esc(pid),
                nm_esc = esc(nm),
                fmt_esc = esc(fmt),
            ));
        }
    }

    // Build the conformsTo list from root `conformsTo`, in array order.
    let mut conf = String::new();
    if let Some(conforms) = root
        .and_then(|r| r.get("conformsTo"))
        .and_then(Value::as_array)
    {
        for v in conforms {
            // conformsTo entries are `{"@id": "https://…"}` objects.
            if let Some(iri) = v.get("@id").and_then(Value::as_str) {
                conf.push_str(&format!(
                    "<li><a href=\"{iri}\"><code>{iri_esc}</code></a></li>\n",
                    iri = esc(iri),
                    iri_esc = esc(iri),
                ));
            }
        }
    }

    // Head JSON-LD: re-serialize the SAME ordered Value (stable bytes for
    // fixed input). serde_json serializes deterministically for `Value`.
    let jsonld = serde_json::to_string(metadata).unwrap_or_default();

    format!(
        "<!DOCTYPE html>\n\
<html lang=\"en\">\n\
<head>\n\
<meta charset=\"utf-8\">\n\
<title>{title} — RO-Crate preview</title>\n\
<script type=\"application/ld+json\">{jsonld}</script>\n\
<style>\n\
body {{ font-family: sans-serif; max-width: 60rem; margin: 2rem auto; padding: 0 1rem; }}\n\
table {{ border-collapse: collapse; width: 100%; }}\n\
td, th {{ border: 1px solid #ccc; padding: .3rem .5rem; text-align: left; }}\n\
code {{ font-size: .85em; }}\n\
h1 {{ margin-bottom: .2rem; }}\n\
a {{ color: #1a6fce; }}\n\
</style>\n\
</head>\n\
<body>\n\
<h1>{title}</h1>\n\
<p>{desc}</p>\n\
<section>\n\
<h2>Files</h2>\n\
<table>\n\
<tr><th>Path</th><th>Name</th><th>Format</th></tr>\n\
{rows}\
</table>\n\
</section>\n\
<section>\n\
<h2>Conforms to</h2>\n\
<ul>\n\
{conf}\
</ul>\n\
</section>\n\
</body>\n\
</html>\n",
        title = esc(name),
        desc = esc(desc),
        jsonld = jsonld,
        rows = rows,
        conf = conf,
    )
}

/// Write `ro-crate-preview.html` at `package_root` via
/// [`ecaa_workflow_core::fs_helpers::atomic_write_bytes_sync`].
///
/// This is the file-writing companion to the pure [`render_ro_crate_preview`].
/// Callers should pass the FINAL metadata value (after all `@graph` mutations
/// have been applied) so the embedded JSON-LD reflects the canonical
/// `ro-crate-metadata.json`.
pub fn write_ro_crate_preview(package_root: &Path, metadata: &Value) -> std::io::Result<()> {
    let html = render_ro_crate_preview(metadata);
    crate::fs_helpers::atomic_write_bytes_sync(
        &package_root.join("ro-crate-preview.html"),
        html.as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_metadata() -> serde_json::Value {
        serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "name": "Airway DEX RNA-seq",
                    "description": "What was asked …",
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
        })
    }

    /// TDD RED→GREEN: primary determinism + spec compliance test.
    #[test]
    fn preview_is_deterministic_and_embeds_jsonld() {
        let meta = sample_metadata();
        let a = render_ro_crate_preview(&meta);
        let b = render_ro_crate_preview(&meta);
        assert_eq!(a, b, "byte-deterministic for fixed metadata");
        assert!(a.starts_with("<!DOCTYPE html>"), "must start with DOCTYPE");
        assert!(
            a.contains("<script type=\"application/ld+json\">"),
            "head JSON-LD embed (spec MUST)"
        );
        assert!(a.contains("Airway DEX RNA-seq"), "root name in body");
        assert!(a.contains("README.md"), "hasPart @id in file table");
        assert!(
            a.contains("https://w3id.org/ro/crate/1.1"),
            "conformsTo IRI present"
        );
        // No executable JS — no bare `<script>` (without type attribute)
        assert!(
            !a.to_lowercase().contains("<script>"),
            "no executable JS bare <script>"
        );
        // No host temp dir path
        assert!(
            !a.contains(std::env::temp_dir().to_string_lossy().as_ref()),
            "no host temp paths"
        );
    }

    #[test]
    fn preview_html_escapes_special_chars() {
        let meta = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": [
                {
                    "@id": "./",
                    "@type": "Dataset",
                    "name": "Test <b>bold</b> & \"quoted\"",
                    "description": "A <script>alert(1)</script> test",
                    "hasPart": []
                }
            ]
        });
        let html = render_ro_crate_preview(&meta);
        // The JSON-LD script block (in <head>) may contain the raw value — that
        // is correct and expected. Only the rendered body (h1/p) must escape.
        let body_start = html.find("<body>").unwrap_or(0);
        let body = &html[body_start..];
        // The h1 title must be HTML-escaped.
        assert!(
            !body.contains("<b>bold</b>"),
            "raw HTML <b> tags must not appear in body"
        );
        assert!(
            body.contains("&lt;b&gt;"),
            "< must be escaped as &lt; in body"
        );
        // A <script>alert injection must not appear in the body.
        assert!(
            !body.contains("<script>alert"),
            "script injection must not appear in body"
        );
    }

    #[test]
    fn preview_has_no_executable_js() {
        let meta = sample_metadata();
        let html = render_ro_crate_preview(&meta);
        // Only the typed ld+json script tag is allowed.
        // Count occurrences of `<script` (case-insensitive).
        let lower = html.to_lowercase();
        let script_count = lower.matches("<script").count();
        // There should be exactly ONE <script tag (the ld+json one).
        assert_eq!(
            script_count, 1,
            "exactly one <script> tag (the ld+json one), got {script_count}"
        );
        // And it must be typed.
        assert!(
            lower.contains("<script type=\"application/ld+json\">"),
            "the one script tag must be typed application/ld+json"
        );
    }

    #[test]
    fn preview_empty_graph_does_not_panic() {
        let meta = serde_json::json!({
            "@context": "https://w3id.org/ro/crate/1.1/context",
            "@graph": []
        });
        let html = render_ro_crate_preview(&meta);
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("RO-Crate")); // fallback name
    }

    #[test]
    fn write_ro_crate_preview_writes_file() {
        let dir = tempfile::tempdir().unwrap();
        let meta = sample_metadata();
        write_ro_crate_preview(dir.path(), &meta).unwrap();
        let path = dir.path().join("ro-crate-preview.html");
        assert!(path.exists(), "ro-crate-preview.html must be written");
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("<!DOCTYPE html>"));
        assert!(content.contains("Airway DEX RNA-seq"));
    }

    #[test]
    fn write_ro_crate_preview_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let meta = sample_metadata();
        write_ro_crate_preview(dir.path(), &meta).unwrap();
        let first = std::fs::read(dir.path().join("ro-crate-preview.html")).unwrap();
        write_ro_crate_preview(dir.path(), &meta).unwrap();
        let second = std::fs::read(dir.path().join("ro-crate-preview.html")).unwrap();
        assert_eq!(first, second, "write is idempotent for same metadata");
    }
}
