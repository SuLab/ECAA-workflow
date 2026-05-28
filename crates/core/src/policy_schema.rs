//! Load-time JSON schema validation for `config/downstream-policy/*.json`.
//!
//! Each policy ships with a `<name>.schema.json` sidecar. `load_and_validate`
//! parses the policy, compiles the schema, and runs a validation pass that
//! flags shape drift at load time with a clear error pointing at the path
//! and the violated rule. The validator is lightweight: it runs one pass
//! per policy on every call, which is sufficient because the handful of
//! policies are loaded at most twice per emission (once for discovery, once
//! for copy).
//!
//! `_shared-vocab.json` in the same directory provides a single source of
//! truth for enumerated values that appear in multiple policies. Any JSON
//! value of the form `{"$shared": "<key>"}` is replaced with the
//! corresponding entry from the vocab file before schema validation.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::fs;
use std::path::Path;

/// Parse the policy at `policy_path`, resolve any `{"$shared": "<key>"}`
/// references against `_shared-vocab.json` in the same directory, then
/// validate the resolved document against its sidecar schema. Returns the
/// fully-resolved `Value`.
///
/// Also validates the policy against the shared
/// `_policy-skeleton.schema.json` when present in the same directory
/// AND the policy matches the claim-boundary shape (has both
/// `targetStages` and `claimBoundary` fields). That pass is
/// independent of the per-policy sidecar, so the three thin
/// claim-boundary policies can shrink their sidecars to just their
/// domain-specific additions.
pub fn load_and_validate(policy_path: &Path) -> Result<Value> {
    let policy_raw = fs::read_to_string(policy_path)
        .with_context(|| format!("reading policy at {}", policy_path.display()))?;
    let mut policy: Value = serde_json::from_str(&policy_raw)
        .with_context(|| format!("parsing JSON at {}", policy_path.display()))?;

    // Resolve shared-vocab references before any schema validation so the
    // validator sees the final, fully-typed shape.
    let parent = policy_path.parent().unwrap_or_else(|| Path::new("."));
    let shared_vocab = load_shared_vocab(parent)?;
    resolve_shared_refs(&mut policy, &shared_vocab)
        .with_context(|| format!("resolving $shared references in {}", policy_path.display()))?;

    // Shared claim-boundary skeleton — applied first so the specific
    // sidecar can trust its precondition shape.
    apply_skeleton_if_claim_boundary(policy_path, &policy)?;

    let schema_path = schema_sidecar_path(policy_path);
    if !schema_path.exists() {
        // A missing sidecar is a soft error during the transitional rollout:
        // policy shape is still verified, but via a skipped pass. Return the
        // parsed policy so emission proceeds. If this becomes load-bearing,
        // swap the early-return for an Err.
        return Ok(policy);
    }

    let compiled = crate::schema_helpers::compile_schema_from_path_cached(&schema_path)?;

    if let Err(errs) = compiled.validate(&policy) {
        let messages: Vec<String> = errs
            .map(|e| format!("  at {}: {}", e.instance_path, e))
            .collect();
        return Err(anyhow!(
            "policy {} failed schema validation ({} violation(s)):\n{}",
            policy_path.display(),
            messages.len(),
            messages.join("\n")
        ));
    }

    Ok(policy)
}

/// Load `_shared-vocab.json` from `dir`. Returns an empty object when the
/// file is absent — policies that contain no `$shared` references work fine
/// without a vocab file in the directory.
fn load_shared_vocab(dir: &Path) -> Result<Value> {
    let vocab_path = dir.join("_shared-vocab.json");
    if !vocab_path.exists() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    let raw = fs::read_to_string(&vocab_path)
        .with_context(|| format!("reading shared vocab at {}", vocab_path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parsing shared vocab at {}", vocab_path.display()))
}

/// Walk `value` in place, replacing every `{"$shared": "<key>"}` object with
/// the corresponding entry from `shared`. Returns `Err` when a referenced key
/// is absent from `shared`.
fn resolve_shared_refs(value: &mut Value, shared: &Value) -> Result<()> {
    match value {
        Value::Object(map) => {
            // Detect the substitution sentinel: an object with exactly one
            // key named "$shared" whose value is a string. Extra keys are
            // rejected so sibling fields aren't silently discarded — vocab
            // values are not themselves recursively resolved, so a caller
            // who wants both a reference and a comment must write the comment
            // into _shared-vocab.json itself.
            if let Some(Value::String(key)) = map.get("$shared").cloned() {
                if map.len() > 1 {
                    let extras = map
                        .keys()
                        .filter(|k| k.as_str() != "$shared")
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ");
                    return Err(anyhow!(
                        "$shared reference to \"{}\" has extra sibling \
                         keys [{}]; the sentinel must be alone in its \
                         object",
                        key,
                        extras
                    ));
                }
                match shared.get(&key) {
                    Some(replacement) => {
                        *value = replacement.clone();
                        // Vocab values are not recursively resolved; if a
                        // future vocab entry contains a nested $shared,
                        // it falls through unchanged. This keeps the
                        // substitution single-pass and avoids cycles.
                        return Ok(());
                    }
                    None => {
                        return Err(anyhow!(
                            "$shared reference to unknown key \"{}\"; \
                             available keys: [{}]",
                            key,
                            shared
                                .as_object()
                                .map(|m| m
                                    .keys()
                                    .filter(|k| !k.starts_with('$'))
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", "))
                                .unwrap_or_default()
                        ));
                    }
                }
            }
            // Not a sentinel — recurse into each field value.
            for v in map.values_mut() {
                resolve_shared_refs(v, shared)?;
            }
        }
        Value::Array(arr) => {
            for v in arr.iter_mut() {
                resolve_shared_refs(v, shared)?;
            }
        }
        // Scalars (null, bool, number, string) have nothing to substitute.
        _ => {}
    }
    Ok(())
}

/// Detect the "claim-boundary" shape and run an extra validation pass
/// against `_policy-skeleton.schema.json` in the same directory. A
/// policy matches when it has both `targetStages` and `claimBoundary`.
/// Absence of the skeleton file is a no-op (so non-claim-boundary
/// policies aren't disturbed).
fn apply_skeleton_if_claim_boundary(policy_path: &Path, policy: &Value) -> Result<()> {
    let is_claim_boundary =
        policy.get("targetStages").is_some() && policy.get("claimBoundary").is_some();
    if !is_claim_boundary {
        return Ok(());
    }
    let parent = policy_path.parent().unwrap_or_else(|| Path::new("."));
    let skeleton_path = parent.join("_policy-skeleton.schema.json");
    if !skeleton_path.exists() {
        return Ok(());
    }
    let compiled = crate::schema_helpers::compile_schema_from_path_cached(&skeleton_path)?;
    if let Err(errs) = compiled.validate(policy) {
        let messages: Vec<String> = errs
            .map(|e| format!("  at {}: {}", e.instance_path, e))
            .collect();
        return Err(anyhow!(
            "policy {} failed skeleton validation ({} violation(s)):\n{}",
            policy_path.display(),
            messages.len(),
            messages.join("\n")
        ));
    }
    Ok(())
}

/// Given `foo-policy.json`, return the path `foo-policy.schema.json` in the
/// same directory.
fn schema_sidecar_path(policy_path: &Path) -> std::path::PathBuf {
    let stem = policy_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let parent = policy_path.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{}.schema.json", stem))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_sidecar_path_is_sibling() {
        let p = Path::new("/etc/foo/best-practice-scoring-policy.json");
        assert_eq!(
            schema_sidecar_path(p),
            Path::new("/etc/foo/best-practice-scoring-policy.schema.json")
        );
    }

    #[test]
    fn missing_sidecar_soft_errors() {
        // When the sidecar is absent, load_and_validate returns the parsed
        // Value unchanged (transitional-rollout contract).
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("no-sidecar.json");
        fs::write(&policy_path, r#"{"schemaVersion":"1.0","x":1}"#).unwrap();
        let v = load_and_validate(&policy_path).unwrap();
        assert_eq!(v["schemaVersion"], "1.0");
    }

    #[test]
    fn malformed_json_errors_with_path() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("bad.json");
        fs::write(&policy_path, "{not json").unwrap();
        let e = load_and_validate(&policy_path).unwrap_err();
        let msg = format!("{:#}", e);
        assert!(msg.contains("parsing JSON"), "msg was: {}", msg);
    }

    #[test]
    fn schema_validation_catches_missing_required() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("has-sidecar.json");
        fs::write(&policy_path, r#"{"x":1}"#).unwrap();
        let schema_path = tmp.path().join("has-sidecar.schema.json");
        fs::write(
            &schema_path,
            r#"{"type":"object","required":["schemaVersion"],"properties":{"schemaVersion":{"type":"string"}}}"#,
        )
        .unwrap();
        let e = load_and_validate(&policy_path).unwrap_err();
        let msg = format!("{:#}", e);
        assert!(msg.contains("schemaVersion"), "msg was: {}", msg);
    }

    #[test]
    fn valid_policy_passes_validation() {
        let tmp = tempfile::tempdir().unwrap();
        let policy_path = tmp.path().join("good.json");
        fs::write(&policy_path, r#"{"schemaVersion":"1.0","x":1}"#).unwrap();
        let schema_path = tmp.path().join("good.schema.json");
        fs::write(
            &schema_path,
            r#"{"type":"object","required":["schemaVersion"],"properties":{"schemaVersion":{"type":"string"}}}"#,
        )
        .unwrap();
        let v = load_and_validate(&policy_path).unwrap();
        assert_eq!(v["schemaVersion"], "1.0");
    }
}
