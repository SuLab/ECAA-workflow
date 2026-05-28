//! JSON Schema compile + validate helpers. Consolidates the 12-site
//! pattern across `atom_registry`, `archetype_registry`,
//! `modality_registry`, `project_class_registry`, `gene_panel_registry`,
//! `policy_schema`, and `slurm/sizing`.
//!
//! Phase-6b migration complete. All workspace JSONSchema::compile sites
//! route through `compile_schema_cached` / `compile_schema_from_path_cached`
//! for process-wide caching.

use anyhow::{Context, Result};
use jsonschema::JSONSchema;
use serde_json::Value;

/// Parse `json_str` as a `serde_json::Value` and compile it as a JSON
/// Schema. The `label` is woven into both stage error contexts so a
/// failure trail names the schema, not just the file path.
///
/// Used at registry-load time for embedded `_<noun>.schema.json` blobs
/// that ride inside the binary via `include_str!`.
pub fn compile_schema(json_str: &str, label: &str) -> Result<JSONSchema> {
    let schema_value: Value =
        serde_json::from_str(json_str).with_context(|| format!("parsing {} schema JSON", label))?;
    JSONSchema::compile(&schema_value)
        .map_err(|e| anyhow::anyhow!("compiling {} schema: {}", label, e))
}

/// Validate `v` against `schema`. On failure, emits one error line per
/// jsonschema violation prefixed with the instance pointer so callers can
/// surface the bad path to the user.
///
/// `context_label` is the human-readable subject ("atom: /path/to/x.yaml",
/// "modality manifest: bulk_rnaseq.yaml") that prefixes the bail message.
pub fn validate_value(schema: &JSONSchema, v: &Value, context_label: &str) -> Result<()> {
    if let Err(errors) = schema.validate(v) {
        let msgs: Vec<String> = errors
            .map(|e| format!("  - {}: {}", e.instance_path, e))
            .collect();
        anyhow::bail!(
            "{} schema validation failed:\n{}",
            context_label,
            msgs.join("\n")
        );
    }
    Ok(())
}

/// Cached schema compile for `include_str!`-loaded embedded schemas.
/// The schema JSON is process-lifetime (`&'static str`) so the cache
/// key is the pointer identity of the schema source (collapsed to
/// `usize` so the map is `Send + Sync`). First call compiles;
/// subsequent calls return the same compiled schema.
pub fn compile_schema_cached(
    label: &'static str,
    json_str: &'static str,
) -> Result<&'static jsonschema::JSONSchema> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static CACHE: OnceLock<std::sync::Mutex<HashMap<usize, &'static jsonschema::JSONSchema>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let key = json_str.as_ptr() as usize;
    if let Ok(guard) = cache.lock() {
        if let Some(s) = guard.get(&key) {
            return Ok(*s);
        }
    }
    let compiled = compile_schema(json_str, label)?;
    // `Box::leak` gives us 'static; schemas are process-lifetime.
    let leaked: &'static jsonschema::JSONSchema = Box::leak(Box::new(compiled));
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, leaked);
    }
    Ok(leaked)
}

/// Cached schema compile keyed on absolute filesystem path + mtime.
/// When the on-disk schema changes (config edit), the next call
/// recompiles. Used for sidecar `.schema.json` files outside the
/// binary.
pub fn compile_schema_from_path_cached(
    path: &std::path::Path,
) -> Result<std::sync::Arc<jsonschema::JSONSchema>> {
    use std::collections::HashMap;
    use std::sync::{Arc, OnceLock};
    // The cache key is `PathBuf`, value is `(mtime, compiled-schema)` —
    // the verbosity is inherent to the invariant and a type alias here
    // would obscure rather than help.
    #[allow(clippy::type_complexity)]
    static CACHE: OnceLock<
        std::sync::Mutex<
            HashMap<std::path::PathBuf, (std::time::SystemTime, Arc<jsonschema::JSONSchema>)>,
        >,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let canon = path
        .canonicalize()
        .with_context(|| format!("canonicalizing schema path {}", path.display()))?;
    let mtime = std::fs::metadata(&canon)
        .and_then(|m| m.modified())
        .with_context(|| format!("stat schema path {}", canon.display()))?;
    if let Ok(guard) = cache.lock() {
        if let Some((cached_mtime, cached_schema)) = guard.get(&canon) {
            if *cached_mtime == mtime {
                return Ok(cached_schema.clone());
            }
        }
    }
    let raw = std::fs::read_to_string(&canon)
        .with_context(|| format!("reading schema {}", canon.display()))?;
    let value: serde_json::Value = serde_json::from_str(&raw)
        .with_context(|| format!("parsing schema {}", canon.display()))?;
    let compiled = jsonschema::JSONSchema::compile(&value)
        .map_err(|e| anyhow::anyhow!("compiling schema {}: {}", canon.display(), e))?;
    let arc = Arc::new(compiled);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(canon, (mtime, arc.clone()));
    }
    Ok(arc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const TEST_SCHEMA: &str = r#"{
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "required": ["id", "kind"],
        "properties": {
            "id": { "type": "string", "minLength": 1 },
            "kind": { "type": "string", "enum": ["a", "b", "c"] },
            "count": { "type": "integer", "minimum": 0 }
        },
        "additionalProperties": false
    }"#;

    #[test]
    fn compile_schema_succeeds_for_valid_json() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        // Smoke-check by validating a trivially-valid value.
        validate_value(&schema, &json!({"id": "x", "kind": "a"}), "smoke").unwrap();
    }

    #[test]
    fn compile_schema_surfaces_label_on_parse_failure() {
        let err = compile_schema("not valid json {{", "my-label").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("my-label"), "got: {msg}");
        assert!(msg.contains("parsing"), "got: {msg}");
    }

    #[test]
    fn compile_schema_surfaces_label_on_compile_failure() {
        // Use a JSON value that's parseable but rejected by the JSON Schema
        // compiler (`type` claiming to be a number is invalid).
        let bad = r#"{"type": 5}"#;
        let err = compile_schema(bad, "broken").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("broken"), "got: {msg}");
        assert!(msg.contains("compiling"), "got: {msg}");
    }

    #[test]
    fn validate_value_accepts_compliant_value() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        let v = json!({"id": "abc", "kind": "b", "count": 5});
        validate_value(&schema, &v, "ctx").unwrap();
    }

    #[test]
    fn validate_value_rejects_missing_required() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        let v = json!({"id": "abc"}); // missing kind
        let err = validate_value(&schema, &v, "my-ctx").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("my-ctx"), "got: {msg}");
        assert!(msg.contains("schema validation failed"), "got: {msg}");
    }

    #[test]
    fn validate_value_rejects_enum_violation() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        let v = json!({"id": "abc", "kind": "zzz"});
        let err = validate_value(&schema, &v, "enum-ctx").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("enum-ctx"), "got: {msg}");
    }

    #[test]
    fn validate_value_rejects_additional_properties() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        let v = json!({"id": "abc", "kind": "a", "rogue": 1});
        let err = validate_value(&schema, &v, "extra-ctx").unwrap_err();
        let msg = format!("{:#}", err);
        assert!(msg.contains("extra-ctx"), "got: {msg}");
    }

    #[test]
    fn validate_value_surfaces_instance_pointer_in_error() {
        let schema = compile_schema(TEST_SCHEMA, "test").unwrap();
        let v = json!({"id": "abc", "kind": "a", "count": -1});
        let err = validate_value(&schema, &v, "neg-ctx").unwrap_err();
        let msg = format!("{:#}", err);
        // The /count pointer should appear in the violation list.
        assert!(msg.contains("/count"), "got: {msg}");
    }
}
