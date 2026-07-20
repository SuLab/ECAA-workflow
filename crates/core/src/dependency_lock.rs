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

/// Parse an `env.lock` / `env.explicit.lock` body into `(lang, name) -> version`
/// entries (first occurrence wins — callers accumulate across files by passing
/// the same map). Three line shapes are recognised (matching the recorded-env
/// snapshots the agent writes per task); everything else is skipped:
///   1. pip pin `name==version`                     -> lang "python"
///   2. conda pin `name: version` (value digit-led) -> lang "conda"
///   3. R sessionInfo "other attached packages" block `Name_version` tokens
///      (optionally `[N]`-index-prefixed) -> lang "r"
fn parse_lock_lines(content: &str, seen: &mut BTreeMap<(String, String), String>) {
    let mut in_r_attached = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            in_r_attached = false;
            continue;
        }
        if trimmed.starts_with('#') {
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
}
