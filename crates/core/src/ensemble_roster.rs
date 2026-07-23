//! Per-modality ensemble rosters: the fixed panel of statistical method
//! variants and interpretive subfield lenses a workflow fans out over.
//! Loaded from `config/ensemble-rosters/<modality>.yaml`. Mirrors
//! `reexecution_bounds::ModalityBoundsProvider` (per-modality YAML,
//! `_`-prefixed schema sidecars skipped, warn-and-continue on parse
//! failure, `BTreeMap` for deterministic iteration).

use crate::atom::AtomDefinition;
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

/// Confirmation-seeking / advocacy phrases forbidden in a persona file.
/// PNAS (Bertran, Fogliato & Wu 2026, arXiv:2602.18710) shows *passive*
/// prior-framing barely moves conclusions while *active* confirmation-
/// seeking drives 34–66pp support-rate swings — the ensemble's honesty
/// requires lenses to be subfield viewpoints, never advocates.
///
/// NON-EXHAUSTIVE literal-phrase blocklist: a load-time backstop over
/// operator-authored, human-reviewed persona files, not a complete
/// adversarial filter.
const CONFIRMATION_SEEKING_PATTERNS: &[&str] = &[
    "maximize evidence",
    "maximize the evidence",
    "maximise evidence",
    "maximise the evidence",
    "find support",
    "finding support",
    "find supporting",
    "most supportive",
    "p-hack",
    "p hack",
    "p hacking",
    "specification search",
    "seek confirmation",
    "seeks confirmation",
    "seeking confirmation",
    "confirm the hypothesis",
    "confirms the hypothesis",
    "confirming the hypothesis",
    "prove the hypothesis",
    "proves the hypothesis",
    "proving the hypothesis",
];

/// Reject persona text containing any phrase from the fixed
/// [`CONFIRMATION_SEEKING_PATTERNS`] blocklist (case-insensitive substring
/// match). This is a non-exhaustive backstop over human-reviewed persona
/// files, not an exhaustive adversarial filter — it will not catch every
/// paraphrase of confirmation-seeking language. Returns the offending
/// phrase in the error.
pub fn lint_persona_text(persona_id: &str, text: &str) -> Result<(), String> {
    let lower = text.to_lowercase();
    for pat in CONFIRMATION_SEEKING_PATTERNS {
        if lower.contains(pat) {
            return Err(format!(
                "persona '{persona_id}' contains forbidden confirmation-seeking \
                 language '{pat}'; lenses must be honest subfield viewpoints, \
                 never advocacy"
            ));
        }
    }
    Ok(())
}

impl EnsembleRoster {
    /// Members under `full` factorial: K statistical + M contextualization
    /// + K*M interpretation cells (the two aggregator nodes are not counted).
    pub fn full_member_count(&self) -> u32 {
        let k = self.statistical_variants.len() as u32;
        let m = self.interpretive_lenses.len() as u32;
        k + m + k * m
    }

    /// The `max_ensemble_members` cap must hold the full expansion.
    pub fn validate_caps(&self) -> Result<(), String> {
        let needed = self.full_member_count();
        if self.caps.max_ensemble_members < needed {
            return Err(format!(
                "ensemble roster '{}' caps.max_ensemble_members={} < full expansion {} \
                 (K={} + M={} + K*M)",
                self.modality,
                self.caps.max_ensemble_members,
                needed,
                self.statistical_variants.len(),
                self.interpretive_lenses.len()
            ));
        }
        Ok(())
    }
}

/// Every `statistical_variant.tool` MUST be one of the base analytical
/// atom's declared `attributes.candidate_tools`. Mirrors the
/// candidate_tools read shape in `atom_registry::validate_de_substrate`.
pub fn validate_variant_tools(
    roster: &EnsembleRoster,
    base_atom: &AtomDefinition,
) -> Result<(), String> {
    let tools: Vec<String> = base_atom
        .attributes
        .get("candidate_tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    for v in &roster.statistical_variants {
        if !tools.iter().any(|t| t == &v.tool) {
            return Err(format!(
                "ensemble roster '{}' statistical variant '{}' names tool '{}' \
                 not in base atom '{}' candidate_tools {:?}",
                roster.modality, v.id, v.tool, base_atom.id, tools
            ));
        }
    }
    Ok(())
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

    #[test]
    fn honest_lens_passes_clean_persona() {
        let ok = "You are a molecular-mechanism biologist. Interpret each result \
                  strictly against the evidence; note mechanisms salient to your field.";
        assert!(lint_persona_text("molecular_mechanism", ok).is_ok());
    }

    #[test]
    fn honest_lens_rejects_confirmation_seeking() {
        let bad = "Conduct a specification search to maximize the evidence in favor \
                   of the hypothesis and present the most supportive result.";
        let err = lint_persona_text("bad_lens", bad).unwrap_err();
        assert!(err.contains("bad_lens"), "error names the persona: {err}");
        assert!(
            err.to_lowercase().contains("confirmation-seeking"),
            "error explains the rule: {err}"
        );
    }

    #[test]
    fn honest_lens_catches_inflected_variant() {
        assert!(
            lint_persona_text("x", "This analysis confirms the hypothesis strongly.").is_err()
        );
    }

    #[test]
    fn honest_lens_allows_legitimate_one_sided_test() {
        assert!(lint_persona_text("x", "Where appropriate, report a one-sided test.").is_ok());
    }

    #[test]
    fn variant_tools_pass_when_in_candidate_tools() {
        use crate::atom_registry::AtomRegistry;
        let reg = AtomRegistry::load_from_dir(std::path::Path::new(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/stage-atoms"),
        ))
        .expect("atom load");
        let de = reg.get("differential_expression").expect("DE atom");

        let roster: EnsembleRoster =
            serde_yaml_ng::from_str(MINIMAL_ROSTER).expect("roster parses"); // deseq2 + edger
        assert!(validate_variant_tools(&roster, de).is_ok());
    }

    #[test]
    fn variant_tools_reject_unknown_tool() {
        use crate::atom_registry::AtomRegistry;
        let reg = AtomRegistry::load_from_dir(std::path::Path::new(
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/stage-atoms"),
        ))
        .expect("atom load");
        let de = reg.get("differential_expression").expect("DE atom");

        let mut roster: EnsembleRoster =
            serde_yaml_ng::from_str(MINIMAL_ROSTER).expect("roster parses");
        roster.statistical_variants.push(StatisticalVariant {
            id: "made_up".into(),
            tool: "not_a_real_de_tool".into(),
            bootstrap_replicates: 0,
        });
        let err = validate_variant_tools(&roster, de).unwrap_err();
        assert!(err.contains("not_a_real_de_tool"), "names the bad tool: {err}");
    }

    #[test]
    fn member_count_is_k_plus_m_plus_km() {
        let roster: EnsembleRoster = serde_yaml_ng::from_str(MINIMAL_ROSTER).unwrap();
        // K=2 (deseq2, edger), M=1 lens → 2 + 1 + 2*1 = 5.
        assert_eq!(roster.full_member_count(), 5);
    }

    #[test]
    fn validate_caps_rejects_too_small_cap() {
        let mut roster: EnsembleRoster = serde_yaml_ng::from_str(MINIMAL_ROSTER).unwrap();
        assert!(roster.validate_caps().is_ok(), "24 >= 5");
        roster.caps.max_ensemble_members = 3; // < 5
        let err = roster.validate_caps().unwrap_err();
        assert!(err.contains("max_ensemble_members"), "explains the cap: {err}");
    }
}
