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

/// Embedded schema for `config/ensemble-lenses/lenses.yaml` — the global
/// epistemic-core lens set (5 fixed reasoning-style lenses shared by every
/// modality), distinct from the per-modality `_ensemble.schema.json`.
const EPISTEMIC_CORE_LENSES_SCHEMA_JSON: &str =
    include_str!("../../../config/ensemble-lenses/_lenses.schema.json");

/// Schema-layout version `load_epistemic_core` accepts. Mirrors
/// `modality_registry::CURRENT_MODALITY_SCHEMA_VERSION`'s role: a
/// `lenses.yaml` whose `schema_version` disagrees is rejected before
/// generic JSON-Schema validation runs, so the error names the mismatch
/// explicitly rather than failing on an opaque `const` violation.
pub const CURRENT_EPISTEMIC_CORE_SCHEMA_VERSION: &str = "0.1";

/// On-disk shape of `config/ensemble-lenses/lenses.yaml`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct EpistemicCoreFile {
    /// Checked against [`CURRENT_EPISTEMIC_CORE_SCHEMA_VERSION`] on the raw
    /// `serde_json::Value` before this struct is built; kept as a field
    /// (rather than dropped) only so `deny_unknown_fields` still maps every
    /// key the schema requires.
    #[allow(dead_code)]
    schema_version: String,
    lenses: Vec<InterpretiveLens>,
}

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
    /// Per-modality interpretive lenses. `#[serde(default)]` because
    /// rosters no longer declare this block directly — the effective
    /// per-analysis lens list is now composed at runtime from the global
    /// epistemic core plus deterministically-selected subfields (see
    /// [`EnsembleRosterProvider::compose_lenses`]).
    #[serde(default)]
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
    /// The rendered persona prompt text for this lens. Absent from raw
    /// YAML (`persona_ref` names the file instead); the ensemble
    /// synthesis provider reads the referenced persona file and fills
    /// this in at runtime. `#[serde(default)]` keeps `deny_unknown_fields`
    /// happy against config that predates this field.
    #[serde(default)]
    pub persona_text: Option<String>,
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
    /// The 5 fixed global epistemic-core lenses (shared by every
    /// modality), loaded via [`Self::load_epistemic_core`].
    epistemic_core: Vec<InterpretiveLens>,
    /// `<config_dir>/ensemble-lenses/personas` — where the epistemic
    /// core's `persona_ref` files live, read at [`Self::compose_lenses`]
    /// time.
    epistemic_personas_dir: std::path::PathBuf,
    /// The curated biomedical subfield catalog the deterministic
    /// selector picks from per-analysis.
    subfields: crate::ensemble_subfield::SubfieldCatalog,
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
                ..Default::default()
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
            ..Default::default()
        }
    }

    /// Load the full provider from a `config/` directory root: the
    /// per-modality rosters (`<config_dir>/ensemble-rosters`), the global
    /// epistemic core (`<config_dir>/ensemble-lenses`), and the curated
    /// subfield catalog (`<config_dir>/ensemble-subfields`). Both the
    /// epistemic core and the subfield catalog soft-fail to empty on load
    /// error — a config-loading defect there degrades the ensemble
    /// composition (fewer lenses) rather than blocking the compiler,
    /// mirroring `from_dir`'s never-panics contract.
    pub fn from_config_dir(config_dir: &Path) -> Self {
        let mut me = Self::from_dir(&config_dir.join("ensemble-rosters"));
        me.epistemic_core =
            Self::load_epistemic_core(&config_dir.join("ensemble-lenses")).unwrap_or_default();
        me.epistemic_personas_dir = config_dir.join("ensemble-lenses").join("personas");
        me.subfields = crate::ensemble_subfield::SubfieldCatalog::load_from_dir(
            &config_dir.join("ensemble-subfields"),
        )
        .unwrap_or_default();
        me
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

    /// Load the GLOBAL epistemic-core lens set from
    /// `<dir>/lenses.yaml` (schema-guarded by `<dir>/_lenses.schema.json`),
    /// shared by every modality — distinct from the per-modality
    /// `interpretive_lenses` block in [`Self::from_dir`]'s rosters.
    ///
    /// Mirrors `ModalityRegistry::load_from_dir`'s schema-guard shape: YAML
    /// → reshaped `serde_json::Value` → typed `schema_version` pre-check
    /// (a clearer error than a generic `const` violation) → JSON-Schema
    /// validate → typed deserialize. Every persona file named by
    /// `persona_ref` is read from `<dir>/personas/` and linted via
    /// [`lint_persona_text`] so a load-time honesty violation surfaces
    /// before the lens set is ever consumed. Lenses are returned in
    /// `lenses.yaml` file order (stable, deterministic).
    pub fn load_epistemic_core(dir: &Path) -> Result<Vec<InterpretiveLens>, String> {
        let schema = crate::schema_helpers::compile_schema_cached(
            "ensemble_lenses",
            EPISTEMIC_CORE_LENSES_SCHEMA_JSON,
        )
        .map_err(|e| format!("compiling ensemble_lenses schema: {e}"))?;

        let path = dir.join("lenses.yaml");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("reading epistemic-core lenses {}: {e}", path.display()))?;
        let yaml_val: serde_yaml_ng::Value = serde_yaml_ng::from_str(&raw)
            .map_err(|e| format!("parsing epistemic-core lenses YAML {}: {e}", path.display()))?;
        let parsed: serde_json::Value = serde_json::to_value(&yaml_val)
            .map_err(|e| format!("yaml→json reshape for {}: {e}", path.display()))?;

        if let Some(found) = parsed.get("schema_version").and_then(|v| v.as_str()) {
            if found != CURRENT_EPISTEMIC_CORE_SCHEMA_VERSION {
                return Err(format!(
                    "epistemic-core lenses {} schema_version_mismatch: expected {}, found {}",
                    path.display(),
                    CURRENT_EPISTEMIC_CORE_SCHEMA_VERSION,
                    found,
                ));
            }
        }

        if let Err(errors) = schema.validate(&parsed) {
            let msgs: Vec<String> = errors
                .map(|e| format!("{} at {}", e, e.instance_path))
                .collect();
            return Err(format!(
                "epistemic-core lenses {} failed schema validation:\n  - {}",
                path.display(),
                msgs.join("\n  - ")
            ));
        }

        let file: EpistemicCoreFile = serde_json::from_value(parsed).map_err(|e| {
            format!(
                "deserializing epistemic-core lenses {}: {e}",
                path.display()
            )
        })?;

        let personas_dir = dir.join("personas");
        for lens in &file.lenses {
            let persona_path = personas_dir.join(&lens.persona_ref);
            let text = std::fs::read_to_string(&persona_path).map_err(|e| {
                format!(
                    "reading persona file {} for lens '{}': {e}",
                    persona_path.display(),
                    lens.id
                )
            })?;
            lint_persona_text(&lens.id, &text)?;
        }

        Ok(file.lenses)
    }

    /// Compose the effective per-analysis lens list for `modality`: the 5
    /// fixed epistemic-core lenses (file order) followed by the
    /// deterministically-selected subfields ([`crate::ensemble_subfield_select::select_subfields`],
    /// ranked by score desc then id asc), each with its persona file read
    /// and its `{entity}`/`{entities}` placeholders substituted for
    /// `entity` (`(singular, plural)`). Plural is substituted BEFORE
    /// singular so `{entities}` never gets clobbered by a `{entity}`
    /// prefix match. Every selected subfield is recorded as a
    /// [`crate::decision_substrate::VerifierDecision::EnsembleSubfieldSelected`]
    /// row so the choice is reconstructable without re-running the
    /// matcher. A load or IO failure on a persona file degrades to an
    /// empty `persona_text` (never panics) — consistent with this
    /// module's soft-fail load contract elsewhere.
    pub fn compose_lenses(
        &self,
        modality: &str,
        goal_text: &str,
        entity: (&str, &str),
    ) -> Vec<InterpretiveLens> {
        let sub = |raw: &str| raw.replace("{entities}", entity.1).replace("{entity}", entity.0);
        let mut out: Vec<InterpretiveLens> = self
            .epistemic_core
            .iter()
            .map(|l| {
                let raw = std::fs::read_to_string(self.epistemic_personas_dir.join(&l.persona_ref))
                    .unwrap_or_default();
                InterpretiveLens {
                    persona_text: Some(sub(&raw)),
                    ..l.clone()
                }
            })
            .collect();
        for sel in crate::ensemble_subfield_select::select_subfields(
            goal_text,
            &self.subfields,
            crate::ensemble_subfield_select::S_MAX,
            crate::ensemble_subfield_select::MIN_SELECT_SCORE,
        ) {
            crate::decision_substrate::record(
                crate::decision_substrate::VerifierDecision::EnsembleSubfieldSelected {
                    id: crate::decision_substrate::stable_id(
                        "ensemble_subfield",
                        modality,
                        &sel.id,
                    ),
                    timestamp: crate::decision_substrate::timestamp(),
                    modality: modality.to_string(),
                    subfield_id: sel.id.clone(),
                    matched_keywords: sel.matched_keywords.clone(),
                },
            );
            if let Some(sf) = self.subfields.by_id.get(&sel.id) {
                let raw = std::fs::read_to_string(self.subfields.persona_path(&sf.id))
                    .unwrap_or_default();
                out.push(InterpretiveLens {
                    id: sf.id.clone(),
                    persona_ref: sf.persona_ref.clone(),
                    model_tier: sf.model_tier.clone(),
                    retrieval: sf.retrieval.clone(),
                    model: None,
                    persona_text: Some(sub(&raw)),
                });
            }
        }
        out
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
            persona_text: None,
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

    #[test]
    fn loads_five_epistemic_core_lenses() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/ensemble-lenses");
        let core = EnsembleRosterProvider::load_epistemic_core(&dir).expect("core loads");
        let ids: Vec<_> = core.iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "mechanistic",
                "systems",
                "translational",
                "skeptical",
                "exploratory"
            ]
        );
        assert_eq!(
            core.iter()
                .find(|l| l.id == "skeptical")
                .unwrap()
                .model_tier,
            "opus"
        );
    }

    /// Process-unique counter for test session ids so parallel test
    /// threads never collide on the same `decision_substrate` bucket key
    /// (each session-scoped bucket is otherwise fully isolated already;
    /// this just guarantees the key itself is unique per test run).
    static TEST_SESSION_COUNTER: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    fn unique_test_session(prefix: &str) -> String {
        let n = TEST_SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        format!("{prefix}-{n}")
    }

    #[test]
    fn compose_lenses_bulk_rnaseq_immune_goal() {
        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let provider = EnsembleRosterProvider::from_config_dir(cfg);
        let session_id = unique_test_session("p6t6");
        let _ = crate::decision_substrate::drain_session(&session_id);

        let lenses;
        {
            let _scope = crate::decision_substrate::enter_session(session_id.clone());
            lenses = provider.compose_lenses(
                "bulk_rnaseq",
                "role of T cell inflammation in chronic disease progression",
                ("gene", "genes"),
            );
        }

        assert_eq!(lenses.len(), 6, "5 core + 1 selected subfield (immunology)");
        let core_ids: Vec<&str> = lenses[..5].iter().map(|l| l.id.as_str()).collect();
        assert_eq!(
            core_ids,
            ["mechanistic", "systems", "translational", "skeptical", "exploratory"],
            "core lenses appear first, in file order"
        );
        assert!(
            lenses.iter().any(|l| l.id == "immunology"),
            "immunology subfield must be selected for a T-cell/inflammation goal"
        );
        for lens in &lenses {
            let text = lens
                .persona_text
                .as_ref()
                .unwrap_or_else(|| panic!("lens {} missing persona_text", lens.id));
            assert!(
                text.contains("genes"),
                "lens {} persona_text must contain substituted plural 'genes': {text}",
                lens.id
            );
            assert!(
                !text.contains("{entit"),
                "lens {} persona_text must not retain a literal placeholder: {text}",
                lens.id
            );
        }

        let drained = crate::decision_substrate::drain_session(&session_id);
        assert!(
            drained.iter().any(|d| matches!(
                d,
                crate::decision_substrate::VerifierDecision::EnsembleSubfieldSelected { subfield_id, .. }
                    if subfield_id == "immunology"
            )),
            "expected a recorded EnsembleSubfieldSelected row for immunology, got {drained:?}"
        );
    }

    #[test]
    fn compose_lenses_no_subfield_match_returns_core_only() {
        let cfg = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config"));
        let provider = EnsembleRosterProvider::from_config_dir(cfg);
        let session_id = unique_test_session("p6t6-nomatch");
        let _ = crate::decision_substrate::drain_session(&session_id);

        let lenses;
        {
            let _scope = crate::decision_substrate::enter_session(session_id.clone());
            lenses = provider.compose_lenses(
                "bulk_rnaseq",
                "quarterly widget throughput report for the factory line",
                ("gene", "genes"),
            );
        }

        assert_eq!(lenses.len(), 5, "no subfield keyword match -> core only");
        assert!(crate::decision_substrate::drain_session(&session_id).is_empty());
    }
}
