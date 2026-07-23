//! Per-modality ensemble rosters: the fixed panel of statistical method
//! variants and interpretive subfield lenses a workflow fans out over.
//! Loaded from `config/ensemble-rosters/<modality>.yaml`. Mirrors
//! `reexecution_bounds::ModalityBoundsProvider` (per-modality YAML,
//! `_`-prefixed schema sidecars skipped, warn-and-continue on parse
//! failure, `BTreeMap` for deterministic iteration).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsembleRoster {
    pub schema_version: String,
    pub modality: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_factorial")]
    pub factorial: FactorialMode,
    pub statistical_variants: Vec<StatisticalVariant>,
    pub interpretive_lenses: Vec<InterpretiveLens>,
    pub caps: EnsembleCaps,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactorialMode {
    Full,
    Fractional,
}

fn default_factorial() -> FactorialMode {
    FactorialMode::Full
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatisticalVariant {
    pub id: String,
    pub tool: String,
    #[serde(default)]
    pub bootstrap_replicates: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterpretiveLens {
    pub id: String,
    pub persona_ref: String,
    pub model_tier: String,
    #[serde(default)]
    pub retrieval: String,
    /// Reserved for multi-family models (Phase 2 of the spec); unused in v1.
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnsembleCaps {
    pub max_ensemble_members: u32,
    pub per_ensemble_budget_usd: f64,
    pub min_quorum_per_axis: u32,
}

/// Registry of per-modality rosters. Absent modality → `None` (the
/// ensemble pass is a no-op for that modality).
#[derive(Debug, Clone, Default)]
pub struct EnsembleRosterProvider {
    by_modality: BTreeMap<String, EnsembleRoster>,
}

impl EnsembleRosterProvider {
    /// Load every `<modality>.yaml` under `dir`. `_`-prefixed files
    /// (schema sidecars) are skipped; parse failures warn-and-continue;
    /// a missing dir yields an empty provider (never panics).
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
                continue;
            }
            match std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_yaml_ng::from_str::<EnsembleRoster>(&s).ok())
            {
                Some(r) => {
                    by_modality.insert(stem.to_string(), r);
                }
                None => tracing::warn!(
                    "ensemble_roster: failed to parse {}, skipping",
                    path.display()
                ),
            }
        }
        Self { by_modality }
    }

    /// The roster for a modality, or `None` when unconfigured.
    pub fn roster_for(&self, modality: &str) -> Option<&EnsembleRoster> {
        self.by_modality.get(modality)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_roster(dir: &std::path::Path, name: &str, body: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    const MINIMAL_ROSTER: &str = r#"
schema_version: "0.1"
modality: bulk_rnaseq
enabled: true
factorial: full
statistical_variants:
  - {id: deseq2, tool: deseq2, bootstrap_replicates: 0}
  - {id: edger,  tool: edger}
interpretive_lenses:
  - {id: molecular_mechanism, persona_ref: molecular_mechanism.md, model_tier: opus, retrieval: recent}
caps:
  max_ensemble_members: 24
  per_ensemble_budget_usd: 60
  min_quorum_per_axis: 2
"#;

    #[test]
    fn loads_roster_and_parses_fields() {
        let tmp = std::env::temp_dir().join(format!("ens_roster_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        write_roster(&tmp, "bulk_rnaseq.yaml", MINIMAL_ROSTER);
        // A `_`-prefixed sidecar must be skipped, not parsed.
        write_roster(&tmp, "_ensemble.schema.json", "{}");

        let provider = EnsembleRosterProvider::from_dir(&tmp);
        let roster = provider.roster_for("bulk_rnaseq").expect("roster present");
        assert_eq!(roster.modality, "bulk_rnaseq");
        assert!(roster.enabled);
        assert_eq!(roster.factorial, FactorialMode::Full);
        assert_eq!(roster.statistical_variants.len(), 2);
        assert_eq!(roster.statistical_variants[0].tool, "deseq2");
        assert_eq!(roster.interpretive_lenses.len(), 1);
        assert_eq!(roster.interpretive_lenses[0].model_tier, "opus");
        assert_eq!(roster.caps.min_quorum_per_axis, 2);

        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn missing_dir_yields_empty_provider() {
        let provider = EnsembleRosterProvider::from_dir(std::path::Path::new("/nonexistent/xyz"));
        assert!(provider.roster_for("anything").is_none());
    }
}
