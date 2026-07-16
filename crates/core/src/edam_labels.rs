//! Curated EDAM CURIE → human label lookup for the archetype catalog.
//!
//! The composer routes archetypes by EDAM `data:`/`format:` CURIEs (see
//! `config/archetypes/*.yaml` `goal_data` / `goal_format`), but the emitter
//! carries only the bare IRI — there is no EDAM resolver in the tree. This map
//! covers EXACTLY the distinct `goal_data` / `goal_format` CURIEs used across
//! the shipped archetypes, mapping each to its canonical EDAM preferred label
//! (from the EDAM ontology via the EBI OLS), so `ecaa-workflow list archetypes
//! --json` can render a human-readable catalog (Application-Note Table S3).
//!
//! Scope is intentionally narrow: it is a display aid for the catalog, not a
//! general EDAM binding. Unknown or empty IRIs return `None` (callers render
//! "N/A"). When a new archetype introduces a new `goal_data`/`goal_format`
//! CURIE, add its label here — the `covers_all_archetype_goal_iris` test in
//! `crates/core/tests` pins the closure against the config tree.

/// Return the canonical EDAM preferred label for a `data:`/`format:` CURIE, or
/// `None` for an unknown/empty IRI. Covers exactly the CURIEs used as archetype
/// `goal_data` / `goal_format` across `config/archetypes/*.yaml`.
pub fn edam_label(iri: &str) -> Option<&'static str> {
    match iri {
        // data: — archetype goal_data
        "data:0006" => Some("Data"),
        "data:0863" => Some("Sequence alignment"),
        "data:0951" => Some("Statistical estimate score"),
        "data:1255" => Some("Sequence features"),
        "data:2048" => Some("Report"),
        "data:2976" => Some("Protein sequence"),
        "data:3498" => Some("Sequence variations"),
        "data:3917" => Some("Count matrix"),
        // format: — archetype goal_format
        "format:2331" => Some("HTML"),
        "format:3003" => Some("BED"),
        "format:3016" => Some("VCF"),
        "format:3475" => Some("TSV"),
        "format:3590" => Some("HDF5"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::edam_label;

    #[test]
    fn known_data_and_format_iris_resolve() {
        assert_eq!(edam_label("data:0951"), Some("Statistical estimate score"));
        assert_eq!(edam_label("data:3917"), Some("Count matrix"));
        assert_eq!(edam_label("format:3475"), Some("TSV"));
        assert_eq!(edam_label("format:3016"), Some("VCF"));
    }

    #[test]
    fn empty_and_unknown_iris_return_none() {
        assert_eq!(edam_label(""), None);
        assert_eq!(edam_label("data:9999"), None);
        assert_eq!(edam_label("format:9999"), None);
    }
}
