//! Generic YAML-dir loader: read dir -> filter `_*` schema sidecars ->
//! per-file YAML deserialize -> schema-validate -> filename-stem-vs-id
//! check -> duplicate-id check -> return `BTreeMap<String, T>`.
//!
//! Subsumes the 5 ~50-line scaffolds in atom_registry,
//! archetype_registry, modality_registry, project_class_registry,
//! gene_panel_registry. Individual registries can migrate to call
//! this loader instead of hand-rolling their own walk.

use anyhow::{Context, Result};
use jsonschema::JSONSchema;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Load every YAML file in `dir` as a `T`, validate each against `schema`,
/// and return a `BTreeMap` keyed by the id `id_extractor` pulls out.
///
/// Filtering rules (matching the five existing registries):
/// - Only files with a `.yaml` extension are considered.
/// - Files whose stem starts with `_` are skipped (schema sidecars +
///   docs).
/// - Hidden files (whose stem starts with `.`) are skipped too — same
///   guard as in `gene_panel_registry`.
///
/// Sanity checks:
/// - Every entry's filename stem must match its parsed id. Mismatches
///   bail with a clear "filename stem 'foo' must match id 'bar'" error.
/// - Duplicate ids across files in the directory bail.
///
/// `noun` is the human-readable subject ("atom", "archetype", "modality")
/// woven into every error context.
///
/// Missing directory is *not* an error — returns an empty map. This
/// matches the existing registries' behavior so an environment without
/// optional config doesn't fail-closed.
pub fn load_yaml_dir<T: DeserializeOwned>(
    dir: &Path,
    schema: &JSONSchema,
    noun: &str,
    id_extractor: impl Fn(&T) -> String,
) -> Result<BTreeMap<String, T>> {
    let mut out: BTreeMap<String, T> = BTreeMap::new();

    if !dir.exists() {
        return Ok(out);
    }

    let mut entries: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {} dir {}", noun, dir.display()))?
        .filter_map(|r| r.ok())
        .filter(|e| {
            let p = e.path();
            let is_yaml = p.extension().and_then(|s| s.to_str()) == Some("yaml");
            let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            let is_sidecar = stem.starts_with('_');
            let is_hidden = stem.starts_with('.');
            is_yaml && !is_sidecar && !is_hidden
        })
        .collect();
    // Sort by path for deterministic load order (which feeds deterministic
    // BTreeMap iteration downstream, which feeds byte-reproducible emit).
    entries.sort_by_key(|e| e.path());

    for entry in entries {
        let path = entry.path();
        let raw = crate::fs_helpers::read_to_string_ctx(&path)?;
        let yaml_value: serde_yml::Value = serde_yml::from_str(&raw)
            .with_context(|| format!("parsing {} YAML at {}", noun, path.display()))?;
        let json_value: serde_json::Value = serde_json::to_value(&yaml_value)
            .with_context(|| format!("yaml->json for {} at {}", noun, path.display()))?;

        crate::schema_helpers::validate_value(
            schema,
            &json_value,
            &format!("{}: {}", noun, path.display()),
        )?;

        let parsed: T = serde_json::from_value(json_value)
            .with_context(|| format!("deserializing {} from {}", noun, path.display()))?;

        let id = id_extractor(&parsed);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        if stem != id {
            anyhow::bail!(
                "{} filename stem '{}' must match id '{}' in {}",
                noun,
                stem,
                id,
                path.display()
            );
        }
        if out.insert(id.clone(), parsed).is_some() {
            anyhow::bail!("duplicate {} id '{}' in {}", noun, id, path.display());
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq, Clone)]
    struct Thing {
        id: String,
        kind: String,
    }

    fn thing_id(t: &Thing) -> String {
        t.id.clone()
    }

    const SCHEMA: &str = r#"{
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["id", "kind"],
        "properties": {
            "id":   { "type": "string", "minLength": 1 },
            "kind": { "type": "string" }
        },
        "additionalProperties": false
    }"#;

    fn compiled_schema() -> &'static JSONSchema {
        crate::schema_helpers::compile_schema_cached("thing", SCHEMA).unwrap()
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn missing_dir_yields_empty_map() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope");
        let schema = compiled_schema();
        let out: BTreeMap<String, Thing> =
            load_yaml_dir(&missing, schema, "thing", thing_id).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn empty_dir_yields_empty_map() {
        let tmp = tempfile::tempdir().unwrap();
        let schema = compiled_schema();
        let out: BTreeMap<String, Thing> =
            load_yaml_dir(tmp.path(), schema, "thing", thing_id).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn loads_multiple_valid_yamls() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\nkind: x\n");
        write(tmp.path(), "beta.yaml", "id: beta\nkind: y\n");
        let schema = compiled_schema();
        let out: BTreeMap<String, Thing> =
            load_yaml_dir(tmp.path(), schema, "thing", thing_id).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out.get("alpha"),
            Some(&Thing {
                id: "alpha".to_string(),
                kind: "x".to_string()
            })
        );
        assert_eq!(
            out.get("beta"),
            Some(&Thing {
                id: "beta".to_string(),
                kind: "y".to_string()
            })
        );
    }

    #[test]
    fn skips_underscore_sidecars() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\nkind: x\n");
        write(tmp.path(), "_thing.schema.yaml", "ignore me");
        write(tmp.path(), "_README.yaml", "ignore me too");
        let schema = compiled_schema();
        let out: BTreeMap<String, Thing> =
            load_yaml_dir(tmp.path(), schema, "thing", thing_id).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out.contains_key("alpha"));
    }

    #[test]
    fn skips_non_yaml_files() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\nkind: x\n");
        write(tmp.path(), "ignored.json", "{}");
        write(tmp.path(), "ignored.txt", "hello");
        let schema = compiled_schema();
        let out: BTreeMap<String, Thing> =
            load_yaml_dir(tmp.path(), schema, "thing", thing_id).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn rejects_filename_stem_id_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: wrong\nkind: x\n");
        let schema = compiled_schema();
        let err = load_yaml_dir::<Thing>(tmp.path(), schema, "thing", thing_id).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("filename stem"), "got: {msg}");
        assert!(msg.contains("alpha"), "got: {msg}");
        assert!(msg.contains("wrong"), "got: {msg}");
    }

    #[test]
    fn rejects_schema_violation() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\n"); // missing `kind`
        let schema = compiled_schema();
        let err = load_yaml_dir::<Thing>(tmp.path(), schema, "thing", thing_id).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("schema validation failed"), "got: {msg}");
        assert!(msg.contains("alpha.yaml"), "got: {msg}");
    }

    #[test]
    fn rejects_bad_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\n  : bad: indent\n");
        let schema = compiled_schema();
        let err = load_yaml_dir::<Thing>(tmp.path(), schema, "thing", thing_id).unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("parsing thing YAML"), "got: {msg}");
    }

    #[test]
    fn deterministic_load_order() {
        // Run the load twice and assert the BTreeMap iteration order is
        // identical — implicit because we sort by path and BTreeMap is
        // sorted, but lock the invariant in case someone replaces the
        // collection type.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "alpha.yaml", "id: alpha\nkind: x\n");
        write(tmp.path(), "beta.yaml", "id: beta\nkind: y\n");
        write(tmp.path(), "gamma.yaml", "id: gamma\nkind: z\n");
        let schema = compiled_schema();
        let a: Vec<String> = load_yaml_dir::<Thing>(tmp.path(), schema, "thing", thing_id)
            .unwrap()
            .into_keys()
            .collect();
        let b: Vec<String> = load_yaml_dir::<Thing>(tmp.path(), schema, "thing", thing_id)
            .unwrap()
            .into_keys()
            .collect();
        assert_eq!(a, b);
        assert_eq!(a, vec!["alpha", "beta", "gamma"]);
    }
}
