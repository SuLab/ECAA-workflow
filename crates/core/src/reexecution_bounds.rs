//! Per-modality semantic-equivalence bounds for re-execution
//! classification. Loaded from `config/reexecution-bounds/<modality>.yaml`.
//! Unconfigured modalities fall back to the generic ±5% relative
//! tolerance (the historical placeholder), so adding a modality is a
//! config change, never a code change.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Relative + absolute numeric tolerances for one modality.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct ModalityBounds {
    /// Relative tolerance: `|a-b| / max(|a|,|b|,1e-9) <= relative_tolerance`.
    #[serde(default = "default_relative")]
    pub relative_tolerance: f64,
    /// Absolute tolerance: a cell also passes when `|a-b| <= absolute_tolerance`.
    #[serde(default)]
    pub absolute_tolerance: f64,
}

fn default_relative() -> f64 {
    0.05
}

impl Default for ModalityBounds {
    /// The historical placeholder: ±5% relative, no absolute slack.
    fn default() -> Self {
        Self {
            relative_tolerance: 0.05,
            absolute_tolerance: 0.0,
        }
    }
}

impl ModalityBounds {
    /// True when `b` is within bounds of `a` (relative OR absolute).
    pub fn within(&self, a: f64, b: f64) -> bool {
        if a == b {
            return true;
        }
        let abs_ok = (a - b).abs() <= self.absolute_tolerance;
        let denom = a.abs().max(b.abs()).max(1e-9);
        let rel_ok = (a - b).abs() / denom <= self.relative_tolerance;
        abs_ok || rel_ok
    }
}

/// Registry of per-modality bounds. `bounds_for` falls back to the
/// generic ±5% placeholder for unconfigured modalities.
#[derive(Debug, Clone, Default)]
pub struct ModalityBoundsProvider {
    by_modality: BTreeMap<String, ModalityBounds>,
}

impl ModalityBoundsProvider {
    /// Load every `<modality>.yaml` under `dir`. Files that fail to
    /// parse are skipped with a warning (warn-and-continue). A missing
    /// dir yields a fallback-only provider — never panics.
    pub fn from_dir(dir: &Path) -> Self {
        let mut by_modality = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self { by_modality };
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            if stem.starts_with('_') {
                continue; // schema sidecars like `_bounds.schema.json`
            }
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_yaml_ng::from_str::<ModalityBounds>(&s).ok())
            {
                Some(b) => {
                    by_modality.insert(stem.to_string(), b);
                }
                None => tracing::warn!(
                    "reexecution_bounds: failed to parse {}, skipping",
                    path.display()
                ),
            }
        }
        Self { by_modality }
    }

    /// Bounds for a modality, or the generic ±5% fallback.
    pub fn bounds_for(&self, modality: &str) -> ModalityBounds {
        self.by_modality.get(modality).copied().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unconfigured_modality_uses_generic_fallback() {
        let provider = ModalityBoundsProvider::default(); // empty registry
        let b = provider.bounds_for("some_unknown_modality");
        assert_eq!(b.relative_tolerance, 0.05, "fallback is the ±5% placeholder");
    }

    #[test]
    fn within_bounds_is_true_just_inside_and_false_just_outside() {
        let bounds = ModalityBounds {
            relative_tolerance: 0.01,
            absolute_tolerance: 0.0,
        };
        assert!(bounds.within(100.0, 100.5), "0.5% < 1% => within");
        assert!(!bounds.within(100.0, 102.0), "2% > 1% => outside");
    }

    #[test]
    fn loads_configured_modalities_from_dir() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../config/reexecution-bounds");
        let provider = ModalityBoundsProvider::from_dir(&dir);
        // bulk_rnaseq.yaml + variant_calling.yaml ship in this phase.
        let rna = provider.bounds_for("bulk_rnaseq");
        assert!(rna.relative_tolerance > 0.0);
        let vc = provider.bounds_for("variant_calling");
        assert!(vc.relative_tolerance >= 0.0);
    }

    #[test]
    fn missing_dir_yields_fallback_only_provider() {
        // Always-emits discipline: a missing config dir must NOT panic.
        let provider =
            ModalityBoundsProvider::from_dir(std::path::Path::new("/nonexistent/xyz"));
        assert_eq!(provider.bounds_for("anything").relative_tolerance, 0.05);
    }
}
