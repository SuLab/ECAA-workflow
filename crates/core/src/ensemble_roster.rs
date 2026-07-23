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
    /// Reserved for multi-family models; deferred, unused in v1.
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
    root: std::path::PathBuf,
}

impl EnsembleRosterProvider {
    /// Load every `<modality>.yaml` under `dir`. `_`-prefixed files
    /// (schema sidecars) are skipped; parse failures warn-and-continue;
    /// a missing dir yields an empty provider (never panics).
    pub fn from_dir(dir: &Path) -> Self {
        let mut by_modality = BTreeMap::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Self {
                by_modality,
                root: dir.to_path_buf(),
            };
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
        Self {
            by_modality,
            root: dir.to_path_buf(),
        }
    }

    /// The roster for a modality, or `None` when unconfigured.
    pub fn roster_for(&self, modality: &str) -> Option<&EnsembleRoster> {
        self.by_modality.get(modality)
    }

    /// The persona directory the ensemble synthesis pass reads
    /// `InterpretiveLens::persona_ref` files from: `<root>/personas`,
    /// where `root` is the directory `from_dir` was loaded from (i.e.
    /// `<config_dir>/ensemble-rosters`).
    pub fn personas_dir(&self) -> std::path::PathBuf {
        self.root.join("personas")
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

    /// The (stat_idx, lens_idx) interpretation cells this roster expands
    /// into. `Full` → every pair in deterministic k-outer/m-inner order
    /// (K*M cells). `Fractional` → a Latin-square-style balanced subset
    /// `(0..max(K,M)).map(|i| (i % K, i % M))` (len = max(K,M); every
    /// stat index and every lens index appears at least once). Empty when
    /// K==0 or M==0 (the fractional modulo would otherwise divide by zero).
    pub fn selected_cells(&self) -> Vec<(usize, usize)> {
        let k = self.statistical_variants.len();
        let m = self.interpretive_lenses.len();
        if k == 0 || m == 0 {
            return Vec::new();
        }
        match self.factorial {
            FactorialMode::Full => {
                let mut cells = Vec::with_capacity(k * m);
                for ki in 0..k {
                    for mi in 0..m {
                        cells.push((ki, mi));
                    }
                }
                cells
            }
            FactorialMode::Fractional => (0..k.max(m)).map(|i| (i % k, i % m)).collect(),
        }
    }

    /// Ensemble members under the roster's factorial mode: K statistical
    /// + M contextualization + one interpretation cell per
    /// [`Self::selected_cells`] entry (the two aggregator nodes are not
    /// counted). `Full` = K + M + K*M; `Fractional` = K + M + max(K,M).
    pub fn member_count(&self) -> u32 {
        let k = self.statistical_variants.len() as u32;
        let m = self.interpretive_lenses.len() as u32;
        k + m + self.selected_cells().len() as u32
    }

    /// The `max_ensemble_members` cap must hold the factorial-mode
    /// expansion ([`Self::member_count`] — fractional-aware).
    pub fn validate_caps(&self) -> Result<(), String> {
        let needed = self.member_count();
        if self.caps.max_ensemble_members < needed {
            return Err(format!(
                "ensemble roster '{}' caps.max_ensemble_members={} < {:?} expansion {} \
                 (K={} + M={} + {} cells)",
                self.modality,
                self.caps.max_ensemble_members,
                self.factorial,
                needed,
                self.statistical_variants.len(),
                self.interpretive_lenses.len(),
                self.selected_cells().len()
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
    fn personas_dir_is_root_join_personas() {
        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let provider = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"));
        assert_eq!(
            provider.personas_dir(),
            cfg.join("ensemble-rosters").join("personas")
        );

        // Also holds for a missing dir (never panics, root still recorded).
        let missing = EnsembleRosterProvider::from_dir(std::path::Path::new("/nonexistent/xyz"));
        assert_eq!(
            missing.personas_dir(),
            std::path::Path::new("/nonexistent/xyz/personas")
        );
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

    /// A K=3, M=2 fractional roster derived from `MINIMAL_ROSTER`.
    fn fractional_k3_m2() -> EnsembleRoster {
        let mut roster: EnsembleRoster = serde_yaml_ng::from_str(MINIMAL_ROSTER).unwrap();
        roster.factorial = FactorialMode::Fractional;
        // MINIMAL has K=2 (deseq2, edger), M=1 (molecular_mechanism); grow to K=3, M=2.
        roster.statistical_variants.push(StatisticalVariant {
            id: "limma".into(),
            tool: "limma_voom".into(),
            bootstrap_replicates: 0,
        });
        roster.interpretive_lenses.push(InterpretiveLens {
            id: "clinical_translational".into(),
            persona_ref: "clinical_translational.md".into(),
            model_tier: "sonnet".into(),
            retrieval: "foundational".into(),
            model: None,
        });
        roster
    }

    #[test]
    fn full_selected_cells_len_eq_km() {
        // MINIMAL_ROSTER: K=2, M=1, Full → K*M = 2 cells, k-outer/m-inner order.
        let roster: EnsembleRoster = serde_yaml_ng::from_str(MINIMAL_ROSTER).unwrap();
        let k = roster.statistical_variants.len();
        let m = roster.interpretive_lenses.len();
        let cells = roster.selected_cells();
        assert_eq!(cells.len(), k * m, "Full → K*M cells");
        assert_eq!(cells, vec![(0, 0), (1, 0)], "deterministic k-outer/m-inner order");
    }

    #[test]
    fn fractional_selected_cells_balanced_and_deterministic() {
        let roster = fractional_k3_m2();
        let k = roster.statistical_variants.len(); // 3
        let m = roster.interpretive_lenses.len(); // 2
        let cells = roster.selected_cells();
        assert_eq!(cells.len(), k.max(m), "Fractional → max(K,M) cells");

        // Every stat index and every lens index appears at least once.
        for ki in 0..k {
            assert!(cells.iter().any(|&(a, _)| a == ki), "stat idx {ki} present");
        }
        for mi in 0..m {
            assert!(cells.iter().any(|&(_, b)| b == mi), "lens idx {mi} present");
        }

        // Determinism: identical output on two calls.
        assert_eq!(cells, roster.selected_cells(), "byte-stable");
    }

    #[test]
    fn member_count_fractional() {
        let roster = fractional_k3_m2();
        let k = roster.statistical_variants.len() as u32; // 3
        let m = roster.interpretive_lenses.len() as u32; // 2
        // Fractional → K + M + max(K,M) = 3 + 2 + 3 = 8.
        assert_eq!(roster.member_count(), k + m + k.max(m));
        assert_eq!(roster.member_count(), 8);
    }

    #[test]
    fn ships_valid_bulk_rnaseq_roster_and_personas() {
        use crate::atom_registry::AtomRegistry;
        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let provider = EnsembleRosterProvider::from_dir(&cfg.join("ensemble-rosters"));
        let roster = provider.roster_for("bulk_rnaseq").expect("bulk_rnaseq roster ships");

        // Caps hold the full expansion.
        roster.validate_caps().expect("caps valid");

        // Every method is a real DE candidate_tool.
        let reg = AtomRegistry::load_from_dir(&cfg.join("stage-atoms")).expect("atoms");
        let de = reg.get("differential_expression").expect("DE atom");
        validate_variant_tools(roster, de).expect("variant tools valid");

        // Every persona file exists and passes the honest-lens lint.
        for lens in &roster.interpretive_lenses {
            let p = cfg.join("ensemble-rosters/personas").join(&lens.persona_ref);
            let text = std::fs::read_to_string(&p)
                .unwrap_or_else(|_| panic!("persona file missing: {}", p.display()));
            lint_persona_text(&lens.id, &text).expect("persona is an honest lens");
        }
    }
}
