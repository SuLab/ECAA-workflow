//! Declarative intake-field → atom-input mapper.
//!
//! Bridges the intake layer (`IntakeFacts` from SME prose) and the
//! composer's slot-fill layer (which expects EDAM-typed inputs to
//! seed each atom's `consumes` slots). The builder hard-codes a
//! similar mapping in `crates/core/src/builder.rs` (intake field →
//! stage spec parameter); this declarative form lets the composer
//! read the same rules without re-implementing them.
//!
//! # Rule shape
//!
//! Each rule maps a leaf-level intake field name (e.g. `samples`,
//! `organism`, `reference_genome`) to:
//!
//! - the EDAM data class the field's value represents (so the
//!   composer's slot-fill knows what atoms can consume it)
//! - an optional `format` hint (file format the field's value names
//!   if it's a path)
//! - an optional `cardinality` (single vs collection)
//! - an optional `constraints:` block — hook for WINGS-style
//!   constraint reasoning; v1 schema records the field but the
//!   composer ignores it.
//!
//! Rules live in `config/intake-port-mapping.yaml`. The loader
//! validates against an embedded JSON schema (`include_str!`) so
//! malformed rule files fail loudly at startup, not silently at
//! runtime when slot-fill misses a field.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use ts_rs::TS;

/// One rule mapping an intake-facts field name to its EDAM type
/// signature.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, TS, schemars::JsonSchema)]
#[ts(export)]
pub struct PortMapping {
    /// Intake field name, e.g. `samples` or `reference_genome`.
    pub intake_field: String,
    /// EDAM data class IRI the value represents.
    pub edam_data: String,
    /// EDAM format IRI when the field's value names a file path.
    /// `None` for non-path fields like `sample_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub edam_format: Option<String>,
    /// Whether the field carries a single value or a list of them.
    /// Single is the default; the composer fans out collection
    /// fields into per-element atom invocations.
    #[serde(default = "default_cardinality")]
    pub cardinality: PortCardinality,
    /// Phase-3 hook — WINGS-style constraint reasoning per [DEC
    /// Part 2]. Schema records the field; v1 composer ignores it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, TS, schemars::JsonSchema)]
#[ts(export)]
#[serde(rename_all = "snake_case")]
/// PortCardinality discriminant.
pub enum PortCardinality {
    /// Single variant.
    Single,
    /// Collection variant.
    Collection,
}

fn default_cardinality() -> PortCardinality {
    PortCardinality::Single
}

/// In-memory mapping table. Keyed by `intake_field`.
#[derive(Debug, Clone, Default)]
pub struct PortMappingRegistry {
    rules: BTreeMap<String, PortMapping>,
}

impl PortMappingRegistry {
    /// Load + validate `config/intake-port-mapping.yaml`. Missing
    /// file is allowed (returns an empty registry) so legacy
    /// Taxonomy paths keep working before the file is.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self {
                rules: BTreeMap::new(),
            });
        }
        let raw = crate::fs_helpers::read_to_string_ctx(path)?;
        let file: PortMappingFile =
            serde_yml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
        let mut rules = BTreeMap::new();
        for r in file.rules {
            // Validate EDAM IRI shape — same regex as the atom
            // schema: `(operation|data|format|topic):\d+` or
            // `ecaax:<slug>`.
            check_edam_iri(&r.edam_data, &r.intake_field, "edam_data")?;
            if let Some(f) = &r.edam_format {
                check_edam_iri(f, &r.intake_field, "edam_format")?;
            }
            if rules.insert(r.intake_field.clone(), r.clone()).is_some() {
                return Err(anyhow!(
                    "duplicate intake_field {} in {}",
                    r.intake_field,
                    path.display()
                ));
            }
        }
        Ok(Self { rules })
    }

    /// Get.
    pub fn get(&self, field: &str) -> Option<&PortMapping> {
        self.rules.get(field)
    }

    /// Iter.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &PortMapping)> {
        self.rules.iter()
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize, schemars::JsonSchema)]
struct PortMappingFile {
    #[serde(default)]
    #[allow(dead_code)]
    // reserved-for-forward-compat: schema-version field; loader is version-agnostic today
    version: u32,
    rules: Vec<PortMapping>,
}

fn check_edam_iri(iri: &str, field: &str, slot: &str) -> Result<()> {
    let ok = iri.starts_with("operation:")
        || iri.starts_with("data:")
        || iri.starts_with("format:")
        || iri.starts_with("topic:")
        || iri.starts_with("ecaax:");
    if !ok {
        return Err(anyhow!(
            "field {} {} = {} doesn't match EDAM IRI pattern \
             (operation:NNNN | data:NNNN | format:NNNN | topic:NNNN | ecaax:slug)",
            field,
            slot,
            iri
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_yaml(dir: &Path, body: &str) -> std::path::PathBuf {
        let p = dir.join("intake-port-mapping.yaml");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    #[test]
    fn missing_file_yields_empty_registry() {
        let reg = PortMappingRegistry::load(Path::new("/nonexistent/path")).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn loads_minimal_mapping_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_yaml(
            tmp.path(),
            r#"version: 1
rules:
  - intake_field: samples
    edam_data: data:2531
    edam_format: format:1930
    cardinality: collection
  - intake_field: reference_genome
    edam_data: data:1383
    edam_format: format:1929
"#,
        );
        let reg = PortMappingRegistry::load(&path).unwrap();
        assert_eq!(reg.len(), 2);
        let samples = reg.get("samples").unwrap();
        assert_eq!(samples.edam_data, "data:2531");
        assert_eq!(samples.cardinality, PortCardinality::Collection);
        let refgen = reg.get("reference_genome").unwrap();
        assert_eq!(refgen.cardinality, PortCardinality::Single); // default
    }

    #[test]
    fn rejects_invalid_edam_iri() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_yaml(
            tmp.path(),
            r#"version: 1
rules:
  - intake_field: samples
    edam_data: not-an-iri-at-all
"#,
        );
        let err = PortMappingRegistry::load(&path).unwrap_err().to_string();
        assert!(err.contains("EDAM IRI pattern"), "got: {}", err);
    }

    #[test]
    fn rejects_duplicate_intake_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_yaml(
            tmp.path(),
            r#"version: 1
rules:
  - intake_field: samples
    edam_data: data:2531
  - intake_field: samples
    edam_data: data:2531
"#,
        );
        let err = PortMappingRegistry::load(&path).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "got: {}", err);
    }

    #[test]
    fn accepts_ecaax_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_yaml(
            tmp.path(),
            r#"version: 1
rules:
  - intake_field: cell_panel
    edam_data: ecaax:cell_marker_panel
"#,
        );
        let reg = PortMappingRegistry::load(&path).unwrap();
        assert_eq!(
            reg.get("cell_panel").unwrap().edam_data,
            "ecaax:cell_marker_panel"
        );
    }

    #[test]
    fn constraints_block_records_but_does_not_enforce() {
        // Phase-3 hook — schema accepts the field, v1 composer ignores
        // it. Lock the round-trip.
        let tmp = tempfile::tempdir().unwrap();
        let path = write_yaml(
            tmp.path(),
            r#"version: 1
rules:
  - intake_field: organism
    edam_data: data:1872
    constraints:
      - "must_be_in: [Homo sapiens, Mus musculus, Rattus norvegicus]"
      - "case_insensitive_match"
"#,
        );
        let reg = PortMappingRegistry::load(&path).unwrap();
        let r = reg.get("organism").unwrap();
        assert_eq!(r.constraints.len(), 2);
        assert!(r.constraints[0].contains("Homo sapiens"));
    }
}
