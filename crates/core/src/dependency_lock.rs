//! Per-package dependency lock. Two columns per package: the
//! human-authored REQUESTED range (from `RuntimePrereqs`, offline +
//! byte-reproducible) and the RESOLVED exact version (filled at runtime
//! by the install-proxy fold — never at emit). The requested-side
//! writer stands alone so emission never blocks on a solver.

use crate::runtime_prereqs::RuntimePrereqs;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One package row: name, the requested range (None when the spec was a
/// bare name), and the runtime-resolved exact version (None until the
/// install-proxy fold runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockEntry {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved: Option<String>,
}

/// Requested-side lock, derived deterministically from `RuntimePrereqs`.
/// `Vec`s are built from `BTreeSet` iteration so ordering is byte-stable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestedLock {
    pub schema_version: String,
    pub r: Vec<LockEntry>,
    pub python: Vec<LockEntry>,
    pub conda: Vec<LockEntry>,
}

/// Package-relative path of the dependency-lock sidecar.
const LOCK_REL_PATH: &str = "runtime/dependency-lock.json";

/// `capture_status`: at least one column carries a run-resolved exact version.
pub const CAPTURE_STATUS_CAPTURED_FROM_RUN: &str = "captured_from_run";
/// `capture_status`: packages were requested, but no run evidence folded in.
pub const CAPTURE_STATUS_REQUESTED_ONLY_NOT_CAPTURED: &str = "requested_only_not_captured";
/// `capture_status`: nothing requested and nothing captured. NOT a claim that
/// the package has no dependencies.
pub const CAPTURE_STATUS_NOT_CAPTURED: &str = "not_captured";

fn split_spec(spec: &str) -> LockEntry {
    // Split on the first comparison operator. CRAN/pip specs look like
    // "Seurat>=5.0", "scanpy>=1.10", "numpy==1.26", "pandas".
    for op in ["==", ">=", "<=", "~=", ">", "<", "="] {
        if let Some(idx) = spec.find(op) {
            return LockEntry {
                name: spec[..idx].trim().to_string(),
                requested: Some(spec[idx..].trim().to_string()),
                resolved: None,
            };
        }
    }
    LockEntry {
        name: spec.trim().to_string(),
        requested: None,
        resolved: None,
    }
}

fn entries(set: &BTreeSet<String>) -> Vec<LockEntry> {
    set.iter().map(|s| split_spec(s)).collect()
}

impl RequestedLock {
    /// Build the requested-side lock from aggregated prereqs. Pure,
    /// offline, byte-reproducible.
    pub fn from_prereqs(p: &RuntimePrereqs) -> Self {
        Self {
            schema_version: "1".to_string(),
            r: entries(&p.language_packages.r),
            python: entries(&p.language_packages.python),
            conda: entries(&p.language_packages.conda),
        }
    }

    /// Fold a runtime-resolved exact version into the matching entry.
    /// `lang` is "r" / "python" / "conda". No-op when the name is absent
    /// (warn-and-continue at the call site). Used by the install-proxy
    /// fold (OPERATOR-GATED runtime path).
    pub fn fold_resolved(&mut self, lang: &str, name: &str, exact: &str) {
        let col = match lang {
            "r" => &mut self.r,
            "python" => &mut self.python,
            "conda" => &mut self.conda,
            _ => return,
        };
        if let Some(e) = col.iter_mut().find(|e| e.name == name) {
            e.resolved = Some(exact.to_string());
        }
    }

    /// Aggregate the resolved exact versions recorded in a package's per-task
    /// lock files into this requested-side lock. Scans
    /// `runtime/outputs/<task>/{env.lock,env.explicit.lock}` in sorted task
    /// order; the first resolved version seen for a `(lang, name)` pair wins.
    /// A resolved package with no matching requested entry is APPENDED
    /// (`requested: None`) so the deposit's `dependency-lock.json` reflects what
    /// ACTUALLY ran — not just what the composer requested — while a requested
    /// entry gets its `resolved` column filled.
    ///
    /// POST-EXECUTION path. At a fresh emit `runtime/outputs/` does not exist,
    /// so this is a no-op and the requested-only lock is unchanged (byte-stable
    /// — the emit determinism contract holds). The deposit / finalize re-emit —
    /// where the per-task env.lock snapshots exist — folds the real installed
    /// versions in, wiring the otherwise-caller-less [`Self::fold_resolved`].
    pub fn fold_from_package_outputs(&mut self, package_root: &Path) {
        let outputs = package_root.join("runtime").join("outputs");
        let Ok(rd) = std::fs::read_dir(&outputs) else {
            return;
        };
        let mut task_dirs: Vec<std::path::PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        task_dirs.sort();

        // (lang, name) -> exact version; first occurrence across all tasks wins.
        let mut seen: BTreeMap<(String, String), String> = BTreeMap::new();
        for dir in &task_dirs {
            for fname in ["env.lock", "env.explicit.lock"] {
                if let Ok(content) = std::fs::read_to_string(dir.join(fname)) {
                    parse_lock_lines(&content, &mut seen);
                }
            }
        }
        for ((lang, name), version) in &seen {
            self.record_resolved(lang, name, version);
        }
    }

    /// How much of this lock is real — the value stamped onto
    /// `runtime/dependency-lock.json` as `capture_status`.
    ///
    /// Bare empty columns conflate three distinct states, and the ambiguity is
    /// load-bearing: atoms declare no package prereqs, so a fresh emit produces
    /// `{"r":[],"python":[],"conda":[]}`, which reads as "this package has no
    /// dependencies" when the truth is "nothing has been captured yet".
    ///
    /// - [`CAPTURE_STATUS_CAPTURED_FROM_RUN`] — at least one entry carries a
    ///   `resolved` exact version folded out of a per-task `env.lock` /
    ///   `env.explicit.lock`. This is what ACTUALLY ran.
    /// - [`CAPTURE_STATUS_REQUESTED_ONLY_NOT_CAPTURED`] — the composer
    ///   requested packages but no run evidence has been folded in yet.
    /// - [`CAPTURE_STATUS_NOT_CAPTURED`] — nothing requested and nothing
    ///   captured. NOT a claim that the package has no dependencies.
    ///
    /// Pure. The sidecar writer and [`backfill_package_lock`] both derive the
    /// stamped value from here so the two can never disagree.
    pub fn capture_status(&self) -> &'static str {
        let columns = [&self.r, &self.python, &self.conda];
        if columns
            .iter()
            .any(|c| c.iter().any(|e| e.resolved.is_some()))
        {
            CAPTURE_STATUS_CAPTURED_FROM_RUN
        } else if columns.iter().any(|c| !c.is_empty()) {
            CAPTURE_STATUS_REQUESTED_ONLY_NOT_CAPTURED
        } else {
            CAPTURE_STATUS_NOT_CAPTURED
        }
    }

    /// Fill an existing requested entry's `resolved` column (first-wins, like
    /// [`Self::fold_resolved`]) or, when the package was not requested,
    /// APPEND a `requested: None` entry and re-sort the column by name so the
    /// serialization stays byte-stable regardless of task-scan order.
    fn record_resolved(&mut self, lang: &str, name: &str, exact: &str) {
        let col = match lang {
            "r" => &mut self.r,
            "python" => &mut self.python,
            "conda" => &mut self.conda,
            _ => return,
        };
        if let Some(e) = col.iter_mut().find(|e| e.name == name) {
            if e.resolved.is_none() {
                e.resolved = Some(exact.to_string());
            }
        } else {
            col.push(LockEntry {
                name: name.to_string(),
                requested: None,
                resolved: Some(exact.to_string()),
            });
            col.sort_by(|a, b| a.name.cmp(&b.name));
        }
    }
}

/// Re-fold a package's per-task run evidence into `runtime/dependency-lock.json`
/// and re-stamp its `capture_status`. Returns whether the file changed.
///
/// POST-EXECUTION path, safe to run repeatedly. Loads the existing sidecar (or
/// starts from an empty [`RequestedLock`] when the package has none), unions in
/// every `runtime/outputs/<task>/{env.lock,env.explicit.lock}` via
/// [`RequestedLock::fold_from_package_outputs`] (sorted task order, first-wins
/// — so the result is independent of directory-read order), recomputes
/// [`RequestedLock::capture_status`], and rewrites atomically (tmp + rename via
/// [`crate::fs_helpers::atomic_write_bytes_sync`]).
///
/// Unrelated top-level keys in the existing file are preserved. The one
/// exception is `capture_note`, the reader-facing "not yet captured" caveat the
/// sidecar writer attaches: once the status reaches
/// [`CAPTURE_STATUS_CAPTURED_FROM_RUN`] that note is false, so it is dropped.
///
/// Idempotent: a second call over unchanged evidence produces byte-identical
/// output and returns `false`.
///
/// Unlike `runtime/determinism-shim.json`, `runtime/dependency-lock.json` is a
/// HASHED BagIt payload entity, so a caller that runs this after the manifest
/// was sealed MUST reseal (`emitter::regenerate_bagit_manifest`).
pub fn backfill_package_lock(package_root: &Path) -> std::io::Result<bool> {
    let path = package_root.join(LOCK_REL_PATH);
    let existing = std::fs::read(&path).ok();

    let mut lock: RequestedLock = match existing.as_deref() {
        Some(bytes) => serde_json::from_slice(bytes).map_err(std::io::Error::other)?,
        None => RequestedLock::from_prereqs(&RuntimePrereqs::new()),
    };
    lock.fold_from_package_outputs(package_root);

    let mut value = serde_json::to_value(&lock).map_err(std::io::Error::other)?;
    let status = lock.capture_status();
    let obj = value
        .as_object_mut()
        .ok_or_else(|| std::io::Error::other("dependency-lock payload is not a JSON object"))?;
    // Carry forward any key this function does not own. The lock's own columns
    // are already present, so `contains_key` keeps them authoritative.
    if let Some(prev) = existing
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
    {
        if let Some(prev_obj) = prev.as_object() {
            for (key, prev_value) in prev_obj {
                if !obj.contains_key(key) {
                    obj.insert(key.clone(), prev_value.clone());
                }
            }
        }
    }
    obj.insert(
        "capture_status".to_string(),
        serde_json::Value::String(status.to_string()),
    );
    if status == CAPTURE_STATUS_CAPTURED_FROM_RUN {
        obj.remove("capture_note");
    }

    let body = serde_json::to_vec_pretty(&value).map_err(std::io::Error::other)?;
    if existing.as_deref() == Some(body.as_slice()) {
        return Ok(false);
    }
    crate::fs_helpers::atomic_write_bytes_sync(&path, &body)?;
    Ok(true)
}

/// Parse an `env.lock` / `env.explicit.lock` body into `(lang, name) -> version`
/// entries (first occurrence wins — callers accumulate across files by passing
/// the same map). Four line shapes are recognised (matching the recorded-env
/// snapshots the agent writes per task); everything else is skipped:
///   1. conda `@EXPLICIT` URL list (every line AFTER an `@EXPLICIT` marker)
///      -> lang "conda", via [`parse_explicit_url`]
///   2. pip pin `name==version`                     -> lang "python"
///   3. conda pin `name: version` (value digit-led) -> lang "conda"
///   4. R sessionInfo "other attached packages" block `Name_version` tokens
///      (optionally `[N]`-index-prefixed) -> lang "r"
///
/// The `@EXPLICIT` latch is FILE-scoped and must be checked before shapes 2-4:
/// an explicit-lock URL contains `:` (from `https://`) and would otherwise fall
/// into the shape-3 branch as name `"https"`, whose non-numeric version is then
/// dropped — leaving the conda column empty even after a backfill.
fn parse_lock_lines(content: &str, seen: &mut BTreeMap<(String, String), String>) {
    let mut in_r_attached = false;
    let mut in_explicit = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_r_attached = false;
            continue;
        }
        if trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "@EXPLICIT" {
            in_explicit = true;
            continue;
        }
        if in_explicit {
            if let Some((name, version)) = parse_explicit_url(trimmed) {
                seen.entry(("conda".into(), name)).or_insert(version);
            }
            continue;
        }
        let lower = trimmed.to_ascii_lowercase();
        if lower.contains("other attached packages:") {
            in_r_attached = true;
            if let Some((_, after)) = trimmed.split_once("packages:") {
                parse_r_tokens(after, seen);
            }
            continue;
        }
        if in_r_attached {
            if lower.contains("loaded via") || lower.contains("namespace") {
                in_r_attached = false;
            } else {
                parse_r_tokens(trimmed, seen);
                continue;
            }
        }
        // pip pin: name==version
        if let Some((name, ver)) = trimmed.split_once("==") {
            let (name, ver) = (name.trim(), ver.trim());
            if !name.is_empty() && !ver.is_empty() {
                seen.entry(("python".into(), name.into()))
                    .or_insert_with(|| ver.into());
            }
            continue;
        }
        // conda pin: name: version where version starts with a digit
        // (metadata lines like `conda env: deseq2_env` are skipped because
        // their value does not start with a digit).
        if let Some((name, ver)) = trimmed.split_once(':') {
            let (name, ver) = (name.trim(), ver.trim());
            if !name.is_empty() && ver.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                seen.entry(("conda".into(), name.into()))
                    .or_insert_with(|| ver.into());
            }
            continue;
        }
    }
}

/// Parse one conda `@EXPLICIT` line into `(name, version)`.
///
/// The line is a package URL (or local path) as written by
/// `conda list --explicit --md5`:
/// `https://conda.anaconda.org/conda-forge/noarch/_r-mutex-1.0.1-anacondar_1.tar.bz2#19f9db5f…`
///
/// Take the basename, drop the `#<hash>` fragment and the `.conda` /
/// `.tar.bz2` extension, then split the remaining `name-version-build` stem
/// from the RIGHT — package names themselves contain hyphens
/// (`bioconductor-org.hs.eg.db-3.22.0-r45hdfd78af_0`), so only the last two
/// hyphens are structural. Returns `None` when the stem has fewer than two
/// hyphens or the version is not digit-led, matching the digit-led-version
/// guard the other line shapes use.
fn parse_explicit_url(line: &str) -> Option<(String, String)> {
    let without_fragment = match line.split_once('#') {
        Some((url, _hash)) => url,
        None => line,
    };
    let base = without_fragment.rsplit('/').next()?;
    let stem = base
        .strip_suffix(".conda")
        .or_else(|| base.strip_suffix(".tar.bz2"))
        .unwrap_or(base);
    let mut parts = stem.rsplitn(3, '-');
    let _build = parts.next()?;
    let version = parts.next()?;
    let name = parts.next()?;
    if name.is_empty() || !version.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((name.to_string(), version.to_string()))
}

/// Parse whitespace-separated `Name_version` (optionally `[N]`-index-prefixed)
/// R sessionInfo tokens into `("r", Name) -> version`. A bare `[N]` index
/// marker is skipped; the version must start with a digit.
fn parse_r_tokens(s: &str, seen: &mut BTreeMap<(String, String), String>) {
    for raw in s.split_whitespace() {
        // Drop a leading "[N]" index marker; a standalone "[N]" is skipped.
        let tok = match raw.split_once(']') {
            Some((_, rest)) if !rest.is_empty() => rest,
            Some(_) => continue,
            None => raw,
        };
        if let Some((name, ver)) = tok.rsplit_once('_') {
            if !name.is_empty() && ver.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                seen.entry(("r".into(), name.into()))
                    .or_insert_with(|| ver.into());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_prereqs::{LanguagePackages, RuntimePrereqs};

    fn prereqs() -> RuntimePrereqs {
        let mut p = RuntimePrereqs::new();
        p.language_packages = LanguagePackages {
            r: ["Seurat>=5.0".into(), "BPCells".into()].into(),
            python: ["scanpy>=1.10".into()].into(),
            conda: Default::default(),
        };
        p
    }

    #[test]
    fn requested_lock_is_byte_reproducible() {
        let a = serde_json::to_vec(&RequestedLock::from_prereqs(&prereqs())).unwrap();
        let b = serde_json::to_vec(&RequestedLock::from_prereqs(&prereqs())).unwrap();
        assert_eq!(a, b, "requested lock must be byte-stable");
    }

    #[test]
    fn requested_lock_splits_name_and_range() {
        let lock = RequestedLock::from_prereqs(&prereqs());
        let seurat = lock
            .r
            .iter()
            .find(|e| e.name == "Seurat")
            .expect("Seurat present");
        assert_eq!(seurat.requested.as_deref(), Some(">=5.0"));
        assert!(seurat.resolved.is_none(), "resolved filled at runtime only");
        // No range => requested None, name is the whole token.
        let bpcells = lock
            .r
            .iter()
            .find(|e| e.name == "BPCells")
            .expect("BPCells present");
        assert_eq!(bpcells.requested, None);
    }

    #[test]
    fn fold_resolved_fills_exact_versions() {
        let mut lock = RequestedLock::from_prereqs(&prereqs());
        lock.fold_resolved("r", "Seurat", "5.1.0");
        let seurat = lock.r.iter().find(|e| e.name == "Seurat").unwrap();
        assert_eq!(seurat.resolved.as_deref(), Some("5.1.0"));
    }

    fn seed_env_lock(root: &std::path::Path, task: &str, name: &str, content: &str) {
        let dir = root.join("runtime/outputs").join(task);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(name), content).unwrap();
    }

    #[test]
    fn fold_from_package_outputs_is_noop_without_outputs() {
        // At a fresh emit runtime/outputs/ does not exist -> requested-only
        // lock is untouched (byte-stable).
        let tmp = tempfile::tempdir().unwrap();
        let mut lock = RequestedLock::from_prereqs(&prereqs());
        let before = serde_json::to_vec(&lock).unwrap();
        lock.fold_from_package_outputs(tmp.path());
        assert_eq!(serde_json::to_vec(&lock).unwrap(), before);
    }

    #[test]
    fn fold_from_package_outputs_fills_requested_and_appends_installed() {
        let tmp = tempfile::tempdir().unwrap();
        // himes-shaped env.lock: pip pin + conda pin.
        seed_env_lock(
            tmp.path(),
            "differential_expression",
            "env.lock",
            "pydeseq2==0.5.4\nbioconductor-deseq2: 1.50.2\nscanpy==1.10.0\n",
        );
        let mut p = RuntimePrereqs::new();
        // scanpy is requested (should get its resolved filled); pydeseq2 +
        // bioconductor-deseq2 are only installed (should be appended).
        p.language_packages.python = ["scanpy>=1.10".into()].into();
        let mut lock = RequestedLock::from_prereqs(&p);
        lock.fold_from_package_outputs(tmp.path());

        let scanpy = lock.python.iter().find(|e| e.name == "scanpy").unwrap();
        assert_eq!(scanpy.requested.as_deref(), Some(">=1.10"));
        assert_eq!(scanpy.resolved.as_deref(), Some("1.10.0"));

        let pydeseq2 = lock
            .python
            .iter()
            .find(|e| e.name == "pydeseq2")
            .expect("installed-but-unrequested pip pkg appended");
        assert_eq!(pydeseq2.requested, None);
        assert_eq!(pydeseq2.resolved.as_deref(), Some("0.5.4"));

        let deseq = lock
            .conda
            .iter()
            .find(|e| e.name == "bioconductor-deseq2")
            .expect("conda pin appended to conda column");
        assert_eq!(deseq.resolved.as_deref(), Some("1.50.2"));

        // The lock is now non-empty and reflects what actually ran.
        assert!(!lock.python.is_empty() && !lock.conda.is_empty());
    }

    #[test]
    fn fold_from_package_outputs_is_byte_reproducible_and_task_order_independent() {
        // Two tasks in a non-lexical dir order must still fold deterministically.
        let build = |names: &[(&str, &str, &str)]| {
            let tmp = tempfile::tempdir().unwrap();
            for (task, file, content) in names {
                seed_env_lock(tmp.path(), task, file, content);
            }
            let mut lock = RequestedLock::from_prereqs(&RuntimePrereqs::new());
            lock.fold_from_package_outputs(tmp.path());
            (tmp, serde_json::to_vec(&lock).unwrap())
        };
        let (_a, a) = build(&[
            ("zeta_task", "env.lock", "numpy==1.26.4\n"),
            ("alpha_task", "env.lock", "pandas==2.2.0\n"),
        ]);
        let (_b, b) = build(&[
            ("alpha_task", "env.lock", "pandas==2.2.0\n"),
            ("zeta_task", "env.lock", "numpy==1.26.4\n"),
        ]);
        assert_eq!(a, b, "fold must be byte-stable regardless of task order");
    }

    #[test]
    fn parse_lock_lines_handles_r_session_info() {
        let mut seen: BTreeMap<(String, String), String> = BTreeMap::new();
        parse_lock_lines(
            "other attached packages:\n [1] DESeq2_1.50.2 SummarizedExperiment_1.40.0\n\nloaded via a namespace (and not attached):\n [1] Rcpp_1.0.12\n",
            &mut seen,
        );
        assert_eq!(seen.get(&("r".into(), "DESeq2".into())).map(String::as_str), Some("1.50.2"));
        assert_eq!(
            seen.get(&("r".into(), "SummarizedExperiment".into())).map(String::as_str),
            Some("1.40.0")
        );
        // "loaded via a namespace" packages are NOT attached -> excluded.
        assert!(!seen.contains_key(&("r".into(), "Rcpp".into())));
    }

    #[test]
    fn fold_from_install_log_lines_is_order_independent() {
        let mut lock = RequestedLock::from_prereqs(&{
            let mut p = crate::runtime_prereqs::RuntimePrereqs::new();
            p.language_packages.python = ["scanpy>=1.10".into(), "numpy".into()].into();
            p
        });
        // Two install-log lines in arbitrary order resolve deterministically.
        lock.fold_resolved("python", "numpy", "1.26.4");
        lock.fold_resolved("python", "scanpy", "1.10.2");
        let scanpy = lock.python.iter().find(|e| e.name == "scanpy").unwrap();
        let numpy = lock.python.iter().find(|e| e.name == "numpy").unwrap();
        assert_eq!(scanpy.resolved.as_deref(), Some("1.10.2"));
        assert_eq!(numpy.resolved.as_deref(), Some("1.26.4"));
        // Re-folding the same value is idempotent.
        lock.fold_resolved("python", "numpy", "1.26.4");
        assert_eq!(lock.python.iter().filter(|e| e.name == "numpy").count(), 1);
    }

    // -----------------------------------------------------------------
    // conda `@EXPLICIT` lock parsing + finalize-time backfill
    // -----------------------------------------------------------------

    /// A `conda list --explicit --md5` capture, verbatim in shape: comment
    /// header, `@EXPLICIT` marker, then one package URL per line with a
    /// `#<hash>` fragment. Covers both archive extensions, a leading-underscore
    /// name, and a hyphen-bearing name with dots.
    const EXPLICIT_LOCK: &str = "\
# This file may be used to create an environment using:
# $ conda create --name <env> --file <this file>
# platform: linux-64
@EXPLICIT
https://conda.anaconda.org/conda-forge/noarch/_r-mutex-1.0.1-anacondar_1.tar.bz2#19f9db5f4f1b7f5ef5f6d67207f25f38
https://conda.anaconda.org/conda-forge/linux-64/libzlib-1.3.2-h25fd6f3_2.conda#d87ff7921124eccd67248aa483c23fec
https://conda.anaconda.org/bioconda/linux-64/bioconductor-deseq2-1.50.2-r45ha27e39d_0.conda#d324936eb205e984f7e1a2a573df98f8
https://conda.anaconda.org/bioconda/noarch/bioconductor-org.hs.eg.db-3.22.0-r45hdfd78af_0.conda#4a0953c26d85acf873354860512135ae
";

    fn conda_version(seen: &BTreeMap<(String, String), String>, name: &str) -> Option<String> {
        seen.get(&("conda".to_string(), name.to_string())).cloned()
    }

    #[test]
    fn explicit_lock_urls_parse_to_name_and_version() {
        let mut seen: BTreeMap<(String, String), String> = BTreeMap::new();
        parse_lock_lines(EXPLICIT_LOCK, &mut seen);

        assert_eq!(
            conda_version(&seen, "_r-mutex").as_deref(),
            Some("1.0.1"),
            "a .tar.bz2 URL with a leading-underscore, hyphenated name must parse"
        );
        assert_eq!(
            conda_version(&seen, "libzlib").as_deref(),
            Some("1.3.2"),
            "a .conda URL must parse"
        );
        assert_eq!(
            conda_version(&seen, "bioconductor-deseq2").as_deref(),
            Some("1.50.2"),
            "only the last two hyphens are structural — the name keeps its own"
        );
        assert_eq!(
            conda_version(&seen, "bioconductor-org.hs.eg.db").as_deref(),
            Some("3.22.0"),
            "dotted package names must survive the rsplit"
        );
        // The old `split_once(':')` reading of a URL.
        assert!(
            !seen.contains_key(&("conda".to_string(), "https".to_string())),
            "an explicit URL must never be parsed as a package called `https`"
        );
        assert_eq!(seen.len(), 4, "exactly the four package lines parsed");
    }

    #[test]
    fn ordinary_lock_lines_parse_unchanged() {
        // Regression: the `@EXPLICIT` branch is latched, so a file WITHOUT the
        // marker must parse byte-for-byte as before.
        let mut seen: BTreeMap<(String, String), String> = BTreeMap::new();
        parse_lock_lines(
            "# recorded env\n\
             conda env: deseq2_env\n\
             pydeseq2==0.5.4\n\
             scanpy==1.10.0\n\
             bioconductor-deseq2: 1.50.2\n\
             \n\
             other attached packages:\n \
             [1] DESeq2_1.50.2 SummarizedExperiment_1.40.0\n\
             \n\
             loaded via a namespace (and not attached):\n \
             [1] Rcpp_1.0.12\n",
            &mut seen,
        );
        assert_eq!(
            seen.get(&("python".to_string(), "pydeseq2".to_string()))
                .map(String::as_str),
            Some("0.5.4"),
            "pip pins still parse"
        );
        assert_eq!(
            seen.get(&("python".to_string(), "scanpy".to_string()))
                .map(String::as_str),
            Some("1.10.0")
        );
        assert_eq!(
            conda_version(&seen, "bioconductor-deseq2").as_deref(),
            Some("1.50.2"),
            "`name: version` conda pins still parse"
        );
        assert_eq!(
            seen.get(&("r".to_string(), "DESeq2".to_string()))
                .map(String::as_str),
            Some("1.50.2"),
            "R sessionInfo attached packages still parse"
        );
        assert!(
            !seen.contains_key(&("conda".to_string(), "conda env".to_string())),
            "non-digit-led metadata values are still skipped"
        );
        assert!(
            !seen.contains_key(&("r".to_string(), "Rcpp".to_string())),
            "`loaded via a namespace` packages are still excluded"
        );
        // Without the marker, a bare URL line is NOT treated as explicit.
        let mut unmarked: BTreeMap<(String, String), String> = BTreeMap::new();
        parse_lock_lines(
            "https://conda.anaconda.org/conda-forge/linux-64/libzlib-1.3.2-h25fd6f3_2.conda#abc\n",
            &mut unmarked,
        );
        assert!(
            unmarked.is_empty(),
            "the explicit branch must be gated on the @EXPLICIT marker, got: {unmarked:?}"
        );
    }

    #[test]
    fn backfill_populates_conda_from_per_task_explicit_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();

        // Two tasks each captured a conda explicit lock; one also recorded a
        // pip pin in env.lock.
        seed_env_lock(
            root,
            "differential_expression",
            "env.lock",
            "scanpy==1.10.0\n",
        );
        seed_env_lock(
            root,
            "differential_expression",
            "env.explicit.lock",
            EXPLICIT_LOCK,
        );
        seed_env_lock(
            root,
            "validate_pathway_enrichment",
            "env.explicit.lock",
            "@EXPLICIT\nhttps://conda.anaconda.org/bioconda/linux-64/bioconductor-apeglm-1.32.0-r45ha27e39d_0.conda#3547c7\n",
        );

        // The emitted sidecar: empty columns, honestly stamped not_captured.
        let runtime = root.join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let emitted = serde_json::json!({
            "schema_version": "1",
            "r": [],
            "python": [],
            "conda": [],
            "capture_status": "not_captured",
            "capture_note": "Empty columns mean NOT-YET-CAPTURED. See runtime/outputs/<task_id>/env.explicit.lock.",
        });
        std::fs::write(
            runtime.join("dependency-lock.json"),
            serde_json::to_vec_pretty(&emitted).unwrap(),
        )
        .unwrap();

        assert!(
            backfill_package_lock(root).unwrap(),
            "the backfill must report a change"
        );

        let body = std::fs::read(runtime.join("dependency-lock.json")).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            value["capture_status"], "captured_from_run",
            "run evidence was folded in, so the status must flip"
        );
        assert!(
            value.get("capture_note").is_none(),
            "the not-yet-captured caveat is false once the lock is captured"
        );

        let lock: RequestedLock = serde_json::from_slice(&body).unwrap();
        assert!(
            !lock.conda.is_empty(),
            "the conda column must be populated from the @EXPLICIT locks"
        );
        let deseq = lock
            .conda
            .iter()
            .find(|e| e.name == "bioconductor-deseq2")
            .expect("bioconductor-deseq2 folded out of the explicit lock");
        assert_eq!(deseq.resolved.as_deref(), Some("1.50.2"));
        assert_eq!(
            deseq.requested, None,
            "installed-but-unrequested packages carry no requested range"
        );
        let apeglm = lock
            .conda
            .iter()
            .find(|e| e.name == "bioconductor-apeglm")
            .expect("the second task's explicit lock is unioned in too");
        assert_eq!(apeglm.resolved.as_deref(), Some("1.32.0"));
        let scanpy = lock
            .python
            .iter()
            .find(|e| e.name == "scanpy")
            .expect("env.lock pip pins still fold into the python column");
        assert_eq!(scanpy.resolved.as_deref(), Some("1.10.0"));
        // Byte-stable ordering: the column is name-sorted as written, so the
        // serialization does not depend on task-scan order.
        let names: Vec<&str> = lock.conda.iter().map(|e| e.name.as_str()).collect();
        let mut expected = names.clone();
        expected.sort_unstable();
        assert_eq!(
            names, expected,
            "the conda column must already be name-sorted on disk"
        );

        // Idempotent: nothing new to fold, so nothing is rewritten.
        assert!(
            !backfill_package_lock(root).unwrap(),
            "a second backfill over unchanged evidence must report no change"
        );
        assert_eq!(
            std::fs::read(runtime.join("dependency-lock.json")).unwrap(),
            body,
            "the second backfill must leave the file byte-identical"
        );
    }

    #[test]
    fn backfill_creates_a_lock_when_the_sidecar_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        seed_env_lock(tmp.path(), "task_a", "env.explicit.lock", EXPLICIT_LOCK);
        assert!(
            backfill_package_lock(tmp.path()).unwrap(),
            "a missing sidecar is created from run evidence"
        );
        let value: serde_json::Value = serde_json::from_slice(
            &std::fs::read(tmp.path().join("runtime/dependency-lock.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["capture_status"], "captured_from_run");
        assert_eq!(value["schema_version"], "1");
    }
}
