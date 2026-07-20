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
use std::path::{Path, PathBuf};

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
/// **Best-effort per-task policy for `determinism-env.json`:** a file that is
/// absent *or* present but unparseable is silently skipped — the loop
/// continues to the next task rather than aborting.  Only genuine write or
/// rename failures (disk full, permission denied, etc.) are propagated as
/// errors, because those reflect environmental problems that should surface.
/// The `policies/container.json` update is processed once and is not
/// subject to the per-task skip logic.
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
    // Best-effort: absent or corrupt files are skipped; only write/rename
    // failures (real IO errors) are propagated.
    for task_id in compute_task_ids {
        let det_env = pkg
            .join("runtime")
            .join("outputs")
            .join(task_id)
            .join("determinism-env.json");
        if !det_env.exists() {
            continue;
        }
        match update_determinism_env(&det_env, digest) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                // Corrupt / non-object JSON — skip this task, keep going.
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    Ok(())
}

/// Determinism-envelope fields that must be pinned identically across all
/// tasks of a run: the run-stable `SOURCE_DATE_EPOCH` plus the locale /
/// hash-seed pinning (`ecaa_workflow_core::determinism_seeds`). Keys as they
/// appear in `determinism-env.json` (lower-snake), NOT the env-var names.
const DETERMINISM_PIN_FIELDS: &[&str] =
    &["source_date_epoch", "tz", "lang", "lc_all", "pythonhashseed"];

/// A determinism-env value is "populated" when `source_date_epoch` carries a
/// real value — a non-empty string (the canonical shape the agent wrapper
/// writes it as) or a number. The emitter's stub and any un-stamped stage
/// leave it as an empty string, which reads as "not populated".
fn determinism_env_populated(v: &Value) -> bool {
    match v.get("source_date_epoch") {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Number(_)) => true,
        _ => false,
    }
}

/// Backfill a pinned determinism-env onto any task whose
/// `determinism-env.json` recorded EMPTY pinning (DR-12 / T5.9).
///
/// The input-staging (`data_acquisition`) stage is the canonical case: it is
/// frequently pre-staged / pre-completed at emit and so never dispatched
/// through the harness determinism-env-stamp seam
/// (`main::stamp_determinism_env`), leaving `source_date_epoch` / `tz` /
/// `lang` / `lc_all` / `pythonhashseed` as empty strings while every executed
/// sibling recorded the run-stable envelope. This copies the pin fields (and
/// `captured_env_vars`) from a fully-populated SIBLING task's
/// determinism-env, so the staging stage records the SAME run-stable
/// envelope as every other stage. `task_container_digest`, `pkg_root`,
/// `schema_version`, and any other keys already on the target are preserved.
///
/// Best-effort per this module's contract: a missing / corrupt file is
/// skipped; only genuine write/rename errors propagate. Returns the number
/// of task files backfilled (0 = nothing to do — no populated sibling to
/// copy from, or every present task already pinned).
pub fn backfill_missing_determinism_env(pkg: &Path) -> io::Result<usize> {
    let outputs = pkg.join("runtime").join("outputs");
    let mut task_dirs: Vec<PathBuf> = match std::fs::read_dir(&outputs) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect(),
        // No outputs tree yet → nothing to backfill.
        Err(_) => return Ok(0),
    };
    // Deterministic order so the chosen reference is stable across runs.
    task_dirs.sort();

    // Parse each present determinism-env.json once (skip absent / non-object).
    let mut envs: Vec<(PathBuf, Value)> = Vec::new();
    for dir in &task_dirs {
        let p = dir.join("determinism-env.json");
        let Ok(raw) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if v.is_object() {
            envs.push((p, v));
        }
    }

    // Reference envelope: the first (sorted) task that actually recorded the
    // run-stable pinning. Absent → nothing to copy from (degenerate run where
    // no stage captured determinism), so no-op.
    let Some(reference) = envs
        .iter()
        .find(|(_, v)| determinism_env_populated(v))
        .map(|(_, v)| v.clone())
    else {
        return Ok(0);
    };

    let mut backfilled = 0usize;
    for (path, mut v) in envs {
        if determinism_env_populated(&v) {
            continue;
        }
        let Some(obj) = v.as_object_mut() else {
            continue;
        };
        for k in DETERMINISM_PIN_FIELDS {
            if let Some(rv) = reference.get(*k) {
                obj.insert((*k).to_string(), rv.clone());
            }
        }
        // Match the sibling's captured-key list too, so the staging stage's
        // determinism-env is structurally identical (minus the digest).
        if let Some(cev) = reference.get("captured_env_vars") {
            obj.insert("captured_env_vars".to_string(), cev.clone());
        }
        atomic_write_json(&path, &v)?;
        backfilled += 1;
    }
    Ok(backfilled)
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

    #[test]
    fn overwrites_preexisting_container_digest() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path().to_path_buf();
        let policies = pkg.join("policies");
        fs::create_dir_all(&policies).unwrap();
        // container.json already has a digest — record_digest must overwrite it.
        fs::write(
            policies.join("container.json"),
            r#"{"image": null, "digest": "sha256:old"}"#,
        )
        .unwrap();

        record_digest(&pkg, "sha256:new", &[]).unwrap();

        let cj = read_json(&pkg.join("policies").join("container.json"));
        assert_eq!(cj["digest"], "sha256:new", "old digest must be overwritten");
        // image was null, so it should be promoted to the new digest.
        assert_eq!(cj["image"], "sha256:new", "null image must be promoted to new digest");
    }

    #[test]
    fn corrupt_determinism_env_is_skipped_other_tasks_still_updated() {
        let tmp = TempDir::new().unwrap();
        let pkg = make_pkg(&tmp, &["good_task", "corrupt_task"], "sha256:base");

        // Overwrite corrupt_task's determinism-env.json with invalid JSON.
        let corrupt_path = pkg
            .join("runtime")
            .join("outputs")
            .join("corrupt_task")
            .join("determinism-env.json");
        fs::write(&corrupt_path, b"not valid json {{{{").unwrap();

        let listed: Vec<String> = vec!["good_task".into(), "corrupt_task".into()];
        // Must return Ok — corrupt file is skipped, not propagated.
        record_digest(&pkg, "sha256:new", &listed).unwrap();

        // good_task was updated.
        let good = read_json(
            &pkg.join("runtime")
                .join("outputs")
                .join("good_task")
                .join("determinism-env.json"),
        );
        assert_eq!(
            good["task_container_digest"], "sha256:new",
            "good_task must be updated despite corrupt sibling"
        );

        // corrupt_task's file was left as-is (still invalid; read back as raw
        // bytes to confirm record_digest did not clobber it with valid JSON).
        let raw = fs::read_to_string(&corrupt_path).unwrap();
        assert!(
            raw.contains("not valid json"),
            "corrupt_task file must not have been overwritten"
        );
    }

    // -----------------------------------------------------------------------
    // T5.9 / DR-12 — staging-stage determinism-env backfill
    // -----------------------------------------------------------------------

    /// Write a `determinism-env.json` under `<pkg>/runtime/outputs/<task>/`.
    fn write_det_env(pkg: &Path, task: &str, body: Value) {
        let dir = pkg.join("runtime").join("outputs").join(task);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("determinism-env.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    fn read_det_env(pkg: &Path, task: &str) -> Value {
        read_json(
            &pkg.join("runtime")
                .join("outputs")
                .join(task)
                .join("determinism-env.json"),
        )
    }

    /// A populated (agent-stamped) envelope, matching the shape
    /// `scripts/agent-claude.sh` writes.
    fn populated_env() -> Value {
        serde_json::json!({
            "schema_version": "1",
            "captured_env_vars": ["PYTHONHASHSEED", "SOURCE_DATE_EPOCH", "TZ", "LANG", "LC_ALL"],
            "source_date_epoch": "1700000000",
            "lang": "C.UTF-8",
            "lc_all": "C.UTF-8",
            "tz": "UTC",
            "pythonhashseed": "0",
            "task_container_digest": "sha256:analysis"
        })
    }

    /// The emitter/un-stamped stub: pin fields all empty strings.
    fn empty_env() -> Value {
        serde_json::json!({
            "schema_version": "1",
            "captured_env_vars": [],
            "source_date_epoch": "",
            "lang": "",
            "lc_all": "",
            "tz": "",
            "pythonhashseed": "",
            "task_container_digest": "sha256:staged"
        })
    }

    #[test]
    fn backfill_populates_empty_staging_env_from_sibling() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        // data_acquisition recorded EMPTY pinning; differential_expression
        // (a dispatched sibling) recorded the run-stable envelope.
        write_det_env(pkg, "data_acquisition", empty_env());
        write_det_env(pkg, "differential_expression", populated_env());

        let n = backfill_missing_determinism_env(pkg).unwrap();
        assert_eq!(n, 1, "exactly the staging stage must be backfilled");

        let da = read_det_env(pkg, "data_acquisition");
        assert_eq!(da["source_date_epoch"], "1700000000");
        assert_eq!(da["tz"], "UTC");
        assert_eq!(da["lang"], "C.UTF-8");
        assert_eq!(da["lc_all"], "C.UTF-8");
        assert_eq!(da["pythonhashseed"], "0");
        // captured_env_vars matches the sibling; the digest is preserved.
        assert_eq!(
            da["captured_env_vars"],
            serde_json::json!(["PYTHONHASHSEED", "SOURCE_DATE_EPOCH", "TZ", "LANG", "LC_ALL"])
        );
        assert_eq!(
            da["task_container_digest"], "sha256:staged",
            "the staging stage's own digest must be preserved"
        );

        // The already-populated sibling is untouched.
        let de = read_det_env(pkg, "differential_expression");
        assert_eq!(de["task_container_digest"], "sha256:analysis");
    }

    #[test]
    fn backfill_noop_when_no_populated_sibling() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        write_det_env(pkg, "data_acquisition", empty_env());
        // Only empty envelopes present → nothing to copy from.
        let n = backfill_missing_determinism_env(pkg).unwrap();
        assert_eq!(n, 0, "no populated sibling → no-op");
        let da = read_det_env(pkg, "data_acquisition");
        assert_eq!(da["source_date_epoch"], "", "unchanged when no reference");
    }

    #[test]
    fn backfill_leaves_already_populated_untouched() {
        let tmp = TempDir::new().unwrap();
        let pkg = tmp.path();
        write_det_env(pkg, "alignment", populated_env());
        write_det_env(pkg, "differential_expression", populated_env());
        let n = backfill_missing_determinism_env(pkg).unwrap();
        assert_eq!(n, 0, "every stage already pinned → no-op");
    }

    #[test]
    fn backfill_absent_outputs_tree_is_ok() {
        let tmp = TempDir::new().unwrap();
        assert_eq!(backfill_missing_determinism_env(tmp.path()).unwrap(), 0);
    }
}
