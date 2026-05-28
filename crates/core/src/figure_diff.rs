//! Cross-version figure diff. Walks `runtime/outputs/*/figures/` in two
//! emitted packages, hashes every figure artifact (raster + vector), and
//! classifies each figure-id as Identical / Drifted / NewInChild /
//! DroppedInParent / OnlyVectorFormat / AbsentInBoth.
//!
//! The plotting library promises byte-determinism on a pinned env, so
//! `Drifted` is a strong signal: either the inputs changed (expected
//! after an amendment), the renderer changed (CI bump), or determinism
//! itself regressed. The UI's Compare tab surfaces drifted figures
//! alongside the existing row-level cross-version diff so the SME can
//! tell at a glance whether re-emitting actually moved any results.
//!
//! Hashes every recognised figure extension (PNG/PDF/SVG/HTML and a few
//! near-relatives). A figure id with a raster format absent in both
//! versions but a vector-only artifact present in either is
//! `OnlyVectorFormat`, not `Identical` — the prior "both PNGs absent
//! match" rule would silently classify two unrelated SVG-only figures as
//! identical because their (None, None) raster pair compared equal.
//!
//! Sibling to `cross_version_diff.rs`. Pure stdlib; no LLM, no network.
//!
//! ## Functional-core boundary (C22 / R-7)
//!
//! `diff_figures` is the load-bearing pure-compute function: given two
//! `&Path`s it walks both package layouts and returns the typed
//! `FigureDiffReport`. Directory enumeration + file hashing are the
//! necessary deterministic inputs to the classification — there is no
//! way to compute a figure diff without reading both packages.
//!
//! The sidecar-writing convenience (serialising `FigureDiffReport`
//! to `runtime/figure-diff.json` inside the child package) lives at
//! `crates/conversation/src/emit/mod.rs::write_figure_diff`, an
//! already-I/O context — that's the right place for the side-effect,
//! not core. Core retains only the pure diff + report shape; the
//! conversation emit pipeline owns the serialisation + write.

use crate::hash_utils::sha256_hex;
use crate::ids::StageId;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
// R6-U7: ts-rs derives removed — FigureDiffReport and friends are
// server-internal diff outputs; the UI never reads these shapes.

/// Recognised figure file extensions, lowercased. Order is irrelevant
/// for classification; iteration order is determined by `BTreeMap`
/// keying inside `enumerate_figures`. The list intentionally covers
/// both raster (png) and vector (pdf/svg) outputs plus the
/// interactive-HTML and EPS variants the plotting library occasionally
/// emits — so a new renderer that ships vector-only figures still gets
/// hashed and diffed deterministically.
const FIGURE_EXTENSIONS: &[&str] = &["png", "pdf", "svg", "html", "eps", "jpeg", "jpg", "webp"];

/// Extensions that are vector-format (PDF / SVG / EPS). Used to decide
/// `OnlyVectorFormat` when no raster representation is present in
/// either package.
const VECTOR_EXTENSIONS: &[&str] = &["pdf", "svg", "eps"];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
/// FigureClassification discriminant.
pub enum FigureClassification {
    /// Every recognised extension is byte-identical between parent and
    /// child (and at least one extension is present in both).
    Identical,
    /// At least one extension differs by hash.
    Drifted,
    /// Figure exists only in the child (new stage, new figure id).
    NewInChild,
    /// Figure exists only in the parent (figure id was removed).
    DroppedInParent,
    /// Neither version emitted a raster (PNG) artifact, but at least
    /// one version produced a vector (PDF/SVG/EPS) artifact. The
    /// pre-fix path would have classified this `Identical` because
    /// the absent/absent raster pair compared equal — a false-negative
    /// that masked real vector-only divergence.
    OnlyVectorFormat,
    /// Neither version emitted any artifact for this figure id under
    /// any recognised extension. Reported for completeness (an
    /// upstream gate that records figure ids without producing files
    /// can be flagged); does not contribute to drift counts.
    AbsentInBoth,
}

/// SHA-256 hash record keyed by lowercase extension. Per-extension
/// hashing keeps the diff classification honest when a renderer ships
/// only a vector artifact for a figure.
pub type ExtensionHashes = BTreeMap<String, String>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
/// FigureDiffEntry data.
pub struct FigureDiffEntry {
    pub stage_id: StageId,
    pub figure_id: String,
    /// Classification.
    pub classification: FigureClassification,
    /// SHA-256 of the parent PNG; None when the figure is NewInChild
    /// or the parent's PNG is absent. Kept for backwards-compatible
    /// readers; the authoritative per-extension hash map lives in
    /// `parent_hashes` / `child_hashes`.
    pub parent_png_sha: Option<String>,
    /// SHA-256 of the child PNG; None when the figure is DroppedInParent
    /// or the child's PNG is absent.
    pub child_png_sha: Option<String>,
    /// SHA-256 of the parent PDF, if produced.
    pub parent_pdf_sha: Option<String>,
    /// SHA-256 of the child PDF, if produced.
    pub child_pdf_sha: Option<String>,
    /// Per-extension SHA-256 hashes for the parent package (canonical
    /// surface). Includes PNG/PDF/SVG/HTML/etc. when present.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub parent_hashes: ExtensionHashes,
    /// Per-extension SHA-256 hashes for the child package.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub child_hashes: ExtensionHashes,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
/// FigureDiffReport data.
pub struct FigureDiffReport {
    /// Parent package.
    pub parent_package: String,
    /// Child package.
    pub child_package: String,
    /// N identical.
    pub n_identical: usize,
    /// N drifted.
    pub n_drifted: usize,
    /// N new in child.
    pub n_new_in_child: usize,
    /// N dropped in parent.
    pub n_dropped_in_parent: usize,
    /// Count of figures whose raster representation is absent in both
    /// versions but a vector format is present in either.
    #[serde(default)]
    pub n_only_vector_format: usize,
    /// Count of figures with no artifact in either version under any
    /// recognised extension.
    #[serde(default)]
    pub n_absent_in_both: usize,
    /// Entries.
    pub entries: Vec<FigureDiffEntry>,
}

/// Per-figure path map: lowercase-extension → file path on disk.
/// Walk `runtime/outputs/*/figures/` and return a map of
/// `(stage_id, figure_id) → {ext → path}`. Every recognised extension
/// is hashed; unknown extensions are dropped (so `manifest.json`
/// noise stays out of the diff).
fn enumerate_figures(package_dir: &Path) -> BTreeMap<(String, String), BTreeMap<String, PathBuf>> {
    let mut out: BTreeMap<(String, String), BTreeMap<String, PathBuf>> = BTreeMap::new();
    let outputs = package_dir.join("runtime").join("outputs");
    let Ok(stage_dirs) = fs::read_dir(&outputs) else {
        return out;
    };
    for stage in stage_dirs.flatten() {
        let stage_path = stage.path();
        if !stage_path.is_dir() {
            continue;
        }
        let stage_id = stage_path
            .file_name()
            .and_then(|s| s.to_str())
            .map(String::from)
            .unwrap_or_else(|| "unknown".into());
        let figures_dir = stage_path.join("figures");
        let Ok(files) = fs::read_dir(&figures_dir) else {
            continue;
        };
        for entry in files.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem == "manifest" {
                continue;
            }
            let Some(ext) = p
                .extension()
                .and_then(|s| s.to_str())
                .map(str::to_ascii_lowercase)
            else {
                continue;
            };
            if !FIGURE_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            let key = (stage_id.clone(), stem.to_string());
            out.entry(key).or_default().insert(ext, p);
        }
    }
    out
}

fn sha256_of(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(sha256_hex(&bytes))
}

/// Hash every recognised extension under `paths`, returning a
/// deterministic map keyed by lowercase extension.
fn hash_all(paths: &BTreeMap<String, PathBuf>) -> ExtensionHashes {
    let mut out = ExtensionHashes::new();
    for (ext, path) in paths {
        if let Some(sha) = sha256_of(path) {
            out.insert(ext.clone(), sha);
        }
    }
    out
}

/// Diff figures.
pub fn diff_figures(parent: &Path, child: &Path) -> Result<FigureDiffReport> {
    let parent_map = enumerate_figures(parent);
    let child_map = enumerate_figures(child);

    // Union of keys, sorted (stage_id, figure_id) for deterministic
    // output. BTreeMap iteration is already sorted; we collect into
    // the union via a third BTreeMap keyed identically.
    let mut union: BTreeMap<(String, String), ()> = BTreeMap::new();
    for k in parent_map.keys() {
        union.insert(k.clone(), ());
    }
    for k in child_map.keys() {
        union.insert(k.clone(), ());
    }

    let mut entries: Vec<FigureDiffEntry> = Vec::with_capacity(union.len());
    let mut n_identical = 0usize;
    let mut n_drifted = 0usize;
    let mut n_new_in_child = 0usize;
    let mut n_dropped_in_parent = 0usize;
    let mut n_only_vector_format = 0usize;
    let mut n_absent_in_both = 0usize;

    let empty: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (key, _) in union {
        let (stage_id, figure_id) = key;
        let parent_paths = parent_map
            .get(&(stage_id.clone(), figure_id.clone()))
            .unwrap_or(&empty);
        let child_paths = child_map
            .get(&(stage_id.clone(), figure_id.clone()))
            .unwrap_or(&empty);

        let parent_hashes = hash_all(parent_paths);
        let child_hashes = hash_all(child_paths);

        // Legacy single-extension fields for backwards-compatible
        // readers — derive from the per-extension map.
        let parent_png = parent_hashes.get("png").cloned();
        let parent_pdf = parent_hashes.get("pdf").cloned();
        let child_png = child_hashes.get("png").cloned();
        let child_pdf = child_hashes.get("pdf").cloned();

        let parent_present = !parent_hashes.is_empty();
        let child_present = !child_hashes.is_empty();

        let classification = match (parent_present, child_present) {
            (true, false) => {
                n_dropped_in_parent += 1;
                FigureClassification::DroppedInParent
            }
            (false, true) => {
                n_new_in_child += 1;
                FigureClassification::NewInChild
            }
            (true, true) => {
                // Compare every extension that appears in either side.
                // A missing extension on one side but present on the
                // other counts as drift. Equality requires every
                // observed extension to hash identically.
                let mut all_exts: std::collections::BTreeSet<&str> =
                    std::collections::BTreeSet::new();
                for k in parent_hashes.keys() {
                    all_exts.insert(k.as_str());
                }
                for k in child_hashes.keys() {
                    all_exts.insert(k.as_str());
                }
                let drift = all_exts.iter().any(|ext| {
                    let p = parent_hashes.get(*ext);
                    let c = child_hashes.get(*ext);
                    p != c
                });
                if drift {
                    n_drifted += 1;
                    FigureClassification::Drifted
                } else {
                    n_identical += 1;
                    FigureClassification::Identical
                }
            }
            (false, false) => {
                // Neither side had any recognised artifact under this
                // (stage, figure) key. This branch is unreachable
                // because the union is built from parent_map and
                // child_map keys — but if either map keys a figure
                // whose hash pass produced an empty result (e.g. all
                // files failed to read), classify it explicitly
                // rather than silently calling it Identical.
                let parent_has_vector = VECTOR_EXTENSIONS
                    .iter()
                    .any(|v| parent_paths.contains_key(*v));
                let child_has_vector = VECTOR_EXTENSIONS
                    .iter()
                    .any(|v| child_paths.contains_key(*v));
                if parent_has_vector || child_has_vector {
                    n_only_vector_format += 1;
                    FigureClassification::OnlyVectorFormat
                } else {
                    n_absent_in_both += 1;
                    FigureClassification::AbsentInBoth
                }
            }
        };

        entries.push(FigureDiffEntry {
            stage_id: stage_id.into(),
            figure_id,
            classification,
            parent_png_sha: parent_png,
            parent_pdf_sha: parent_pdf,
            child_png_sha: child_png,
            child_pdf_sha: child_pdf,
            parent_hashes,
            child_hashes,
        });
    }

    Ok(FigureDiffReport {
        parent_package: parent.display().to_string(),
        child_package: child.display().to_string(),
        n_identical,
        n_drifted,
        n_new_in_child,
        n_dropped_in_parent,
        n_only_vector_format,
        n_absent_in_both,
        entries,
    })
}

// `write_figure_diff` previously lived here, serialising the report to
// `runtime/figure-diff.json` inside the child package. The sole
// production caller is `crates/conversation/src/emit/mod.rs`, an
// already-I/O context. Per C22 / R-7 (FunctionalCoreBoundary) the
// serialise-then-write side-effect now lives in that caller; core
// retains only `diff_figures` (the deterministic compute).

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(path: &Path, bytes: &[u8]) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn classifies_identical_drifted_new_dropped() {
        let parent = TempDir::new().unwrap();
        let child = TempDir::new().unwrap();
        let pp = parent.path();
        let cp = child.path();

        // identical: same bytes in both
        touch(
            &pp.join("runtime/outputs/clustering/figures/umap_clusters.png"),
            b"AAAA",
        );
        touch(
            &cp.join("runtime/outputs/clustering/figures/umap_clusters.png"),
            b"AAAA",
        );
        touch(
            &pp.join("runtime/outputs/clustering/figures/umap_clusters.pdf"),
            b"BBBB",
        );
        touch(
            &cp.join("runtime/outputs/clustering/figures/umap_clusters.pdf"),
            b"BBBB",
        );

        // drifted: PNG matches, PDF differs
        touch(
            &pp.join("runtime/outputs/clustering/figures/cluster_size_bar.png"),
            b"CCCC",
        );
        touch(
            &cp.join("runtime/outputs/clustering/figures/cluster_size_bar.png"),
            b"CCCC",
        );
        touch(
            &pp.join("runtime/outputs/clustering/figures/cluster_size_bar.pdf"),
            b"DDDD",
        );
        touch(
            &cp.join("runtime/outputs/clustering/figures/cluster_size_bar.pdf"),
            b"DEFG",
        );

        // new in child
        touch(
            &cp.join("runtime/outputs/differential_expression/figures/volcano.png"),
            b"X",
        );

        // dropped in parent
        touch(
            &pp.join("runtime/outputs/normalization/figures/hvg_count_bar.png"),
            b"Y",
        );

        let report = diff_figures(pp, cp).unwrap();
        assert_eq!(report.n_identical, 1);
        assert_eq!(report.n_drifted, 1);
        assert_eq!(report.n_new_in_child, 1);
        assert_eq!(report.n_dropped_in_parent, 1);
        assert_eq!(report.entries.len(), 4);
    }

    #[test]
    fn drift_detected_when_png_changes() {
        let parent = TempDir::new().unwrap();
        let child = TempDir::new().unwrap();
        touch(
            &parent.path().join("runtime/outputs/x/figures/y.png"),
            b"old",
        );
        touch(
            &child.path().join("runtime/outputs/x/figures/y.png"),
            b"new",
        );
        let r = diff_figures(parent.path(), child.path()).unwrap();
        assert_eq!(r.n_drifted, 1);
        assert_eq!(r.entries[0].classification, FigureClassification::Drifted);
    }
}
