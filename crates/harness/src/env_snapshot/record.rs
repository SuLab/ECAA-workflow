//! Record a captured snapshot digest back into the emitted package.
//!
//! `record_digest` is called after the snapshot image has been built and
//! stored.  It writes the content digest into two places so that replay can
//! pull the exact image:
//!
//! * `policies/container.json` — receives a `"digest"` key; if `"image"` was
//!   null or absent it is set to the digest as well (so replay has a pull ref).
//! * `runtime/outputs/<task_id>/determinism-env.json` — the
//!   `"task_container_digest"` field is overwritten for each listed compute
//!   task id.
//!
//! Both files are updated with a read-modify-write using `serde_json::Value`
//! so that unknown or future keys are preserved.  Each write is atomic
//! (temp-file + rename in the same directory).  Missing files are silently
//! skipped (best-effort per file).

use std::io;
use std::path::Path;

use serde_json::Value;

/// Record `digest` into the package at `pkg`.
///
/// * Updates `<pkg>/policies/container.json`: sets `"digest"` to `digest`;
///   if `"image"` is currently null or absent, sets it to `digest` as well.
///   Preserves every other existing key.
/// * For each id in `compute_task_ids`, updates
///   `<pkg>/runtime/outputs/<id>/determinism-env.json`: overwrites
///   `"task_container_digest"` with `digest`, preserving all sibling keys.
///
/// Any file that does not exist is silently skipped — no error is returned.
/// A compute task whose id is not in `compute_task_ids` is left untouched.
pub fn record_digest(
    pkg: &Path,
    digest: &str,
    compute_task_ids: &[String],
) -> io::Result<()> {
    // --- policies/container.json ---
    let container_json = pkg.join("policies").join("container.json");
    if container_json.exists() {
        update_container_json(&container_json, digest)?;
    }

    // --- runtime/outputs/<task>/determinism-env.json ---
    for task_id in compute_task_ids {
        let det_env = pkg
            .join("runtime")
            .join("outputs")
            .join(task_id)
            .join("determinism-env.json");
        if det_env.exists() {
            update_determinism_env(&det_env, digest)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Read-modify-write `policies/container.json`, atomically.
fn update_container_json(path: &Path, digest: &str) -> io::Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let mut val: Value = serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(io::ErrorKind::InvalidData, format!("container.json parse error: {e}"))
    })?;

    let obj = val.as_object_mut().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "container.json is not a JSON object")
    })?;

    // Set digest unconditionally.
    obj.insert("digest".to_owned(), Value::String(digest.to_owned()));

    // Set image only if it was null or absent.
    let image_is_null_or_absent = obj
        .get("image")
        .map(|v| v.is_null())
        .unwrap_or(true);
    if image_is_null_or_absent {
        obj.insert("image".to_owned(), Value::String(digest.to_owned()));
    }

    atomic_write_json(path, &val)
}

/// Read-modify-write a `determinism-env.json`, atomically.
fn update_determinism_env(path: &Path, digest: &str) -> io::Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let mut val: Value = serde_json::from_str(&raw).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("determinism-env.json parse error: {e}"),
        )
    })?;

    let obj = val.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "determinism-env.json is not a JSON object",
        )
    })?;

    obj.insert(
        "task_container_digest".to_owned(),
        Value::String(digest.to_owned()),
    );

    atomic_write_json(path, &val)
}

/// Serialize `val` to a temp file in the same directory as `path`, then
/// rename to `path`.  This guarantees that a concurrent reader never sees a
/// partial write.
fn atomic_write_json(path: &Path, val: &Value) -> io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;

    // Write to a temp file in the same directory so the rename is atomic
    // (same filesystem).
    let (mut tmp_file, tmp_path) = tempfile::Builder::new()
        .prefix(".record-tmp-")
        .suffix(".json")
        .tempfile_in(dir)?
        .keep()
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;

    let serialized = serde_json::to_string_pretty(val)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("JSON serialization: {e}")))?;

    use std::io::Write as _;
    tmp_file.write_all(serialized.as_bytes())?;
    tmp_file.flush()?;
    drop(tmp_file);

    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Create a minimal package tree:
    ///   <root>/policies/container.json  ({"image": null})
    ///   <root>/runtime/outputs/<task_id>/determinism-env.json
    fn make_pkg(tmp: &TempDir, task_ids: &[&str], base_digest: &str) -> std::path::PathBuf {
        let pkg = tmp.path().to_path_buf();

        // policies/container.json
        let policies = pkg.join("policies");
        fs::create_dir_all(&policies).unwrap();
        fs::write(policies.join("container.json"), r#"{"image": null}"#).unwrap();

        // runtime/outputs/<task>/determinism-env.json
        for task_id in task_ids {
            let dir = pkg.join("runtime").join("outputs").join(task_id);
            fs::create_dir_all(&dir).unwrap();
            let content = serde_json::json!({
                "captured_env_vars": {},
                "lang": "R",
                "lc_all": "C",
                "pythonhashseed": "0",
                "schema_version": "1.0.0",
                "source_date_epoch": 123,
                "task_container_digest": base_digest,
                "tz": "UTC"
            });
            fs::write(
                dir.join("determinism-env.json"),
                serde_json::to_string_pretty(&content).unwrap(),
            )
            .unwrap();
        }

        pkg
    }

    fn read_json(path: &std::path::Path) -> Value {
        let raw = fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[test]
    fn records_digest_into_container_json_and_listed_tasks() {
        let tmp = TempDir::new().unwrap();
        let pkg = make_pkg(
            &tmp,
            &["differential_expression", "normalisation", "unlisted_task"],
            "sha256:base",
        );

        let listed: Vec<String> = vec![
            "differential_expression".into(),
            "normalisation".into(),
        ];
        record_digest(&pkg, "sha256:new", &listed).unwrap();

        // 1. container.json: digest set + image promoted from null.
        let cj = read_json(&pkg.join("policies").join("container.json"));
        assert_eq!(cj["digest"], "sha256:new", "container.json digest mismatch");
        assert_eq!(cj["image"], "sha256:new", "container.json image should be promoted from null");

        // 2. Both listed tasks updated.
        for task_id in &["differential_expression", "normalisation"] {
            let det = read_json(
                &pkg.join("runtime")
                    .join("outputs")
                    .join(task_id)
                    .join("determinism-env.json"),
            );
            assert_eq!(
                det["task_container_digest"], "sha256:new",
                "{task_id}: task_container_digest not updated"
            );

            // 3. Sibling keys preserved.
            assert_eq!(det["lang"], "R", "{task_id}: lang key was clobbered");
            assert_eq!(
                det["source_date_epoch"], 123,
                "{task_id}: source_date_epoch key was clobbered"
            );
        }

        // 4. Unlisted task is untouched.
        let unlisted = read_json(
            &pkg.join("runtime")
                .join("outputs")
                .join("unlisted_task")
                .join("determinism-env.json"),
        );
        assert_eq!(
            unlisted["task_container_digest"], "sha256:base",
            "unlisted task should not be modified"
        );
    }

    #[test]
    fn preserves_non_null_image_in_container_json() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().to_path_buf();
        let policies = pkg.join("policies");
        fs::create_dir_all(&policies).unwrap();
        // image is already set to a real registry ref.
        fs::write(
            policies.join("container.json"),
            r#"{"image": "ghcr.io/example/myimage:latest", "extra_key": true}"#,
        )
        .unwrap();

        record_digest(&pkg, "sha256:new", &[]).unwrap();

        let cj = read_json(&pkg.join("policies").join("container.json"));
        assert_eq!(
            cj["image"], "ghcr.io/example/myimage:latest",
            "non-null image must not be overwritten"
        );
        assert_eq!(cj["digest"], "sha256:new");
        assert_eq!(cj["extra_key"], true, "extra_key must be preserved");
    }

    #[test]
    fn missing_container_json_returns_ok() {
        let tmp = TempDir::new().unwrap();
        // No policies/ directory at all — container.json absent.
        let result = record_digest(tmp.path(), "sha256:new", &[]);
        assert!(result.is_ok(), "absent container.json should not error");
    }

    #[test]
    fn missing_determinism_env_for_task_returns_ok() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().to_path_buf();
        // Create container.json but no task dirs.
        let policies = pkg.join("policies");
        fs::create_dir_all(&policies).unwrap();
        fs::write(policies.join("container.json"), r#"{"image": null}"#).unwrap();

        let listed: Vec<String> = vec!["nonexistent_task".into()];
        let result = record_digest(&pkg, "sha256:new", &listed);
        assert!(result.is_ok(), "absent determinism-env.json should not error");
    }
}
