use crate::goal_spec::GoalSpec;
use crate::modality_registry::{detect_drift_against_legacy, ModalityRegistry};
use crate::project_class::ProjectClass;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ── Output types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
/// ClassificationResult data.
pub struct ClassificationResult {
    /// Modality.
    pub modality: String,
    /// Taxonomy path.
    pub taxonomy_path: String,
    /// Domain.
    pub domain: String,
    /// Workflow description.
    pub workflow_description: String,
    /// Edam topic.
    pub edam_topic: String,
    /// Edam operation.
    pub edam_operation: String,
    /// Confidence.
    pub confidence: f32,
    /// Confidence label.
    pub confidence_label: String,
    /// Organisms.
    pub organisms: Vec<OrganismInfo>,
    /// Methods specified.
    pub methods_specified: Vec<MethodSpec>,
    /// Data sources.
    pub data_sources: Vec<DataSourceRef>,
    /// Intake text.
    pub intake_text: String,
    /// What the SME wants the analysis to produce, as a
    /// typed (`edam_data`, `edam_format`, `modifiers`) tuple. Set by
    /// keyword-path extraction (S6.3, `goal_patterns:` block in
    /// `modality-keywords.yaml`) at classify time, OR by LLM-path
    /// extraction (S6.4, `classify_intake` tool) at chat-time.
    /// Composer (S7.2) consumes this to seed backward chaining when
    /// `composer_version >= 2`. `#[serde(default)]` keeps wire-shape
    /// compat: pre-S6 sessions persist + load with `goal: None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSpec>,
    /// Archetype id pinned at classify time when
    /// `composer_version >= 2` matched an archetype on the goal +
    /// project-class tuple. Forward-compat companion to
    /// `taxonomy_path`: legacy sessions and `composer_version == 1`
    /// flows leave this `None` and continue to route through
    /// `taxonomy_path`; archetype-driven flows populate this so the
    /// emitted package + session lineage carry the matched archetype
    /// id without re-running the matcher. A future rename will remove
    /// `taxonomy_path` and promote this field; for now both coexist
    /// so live sessions reload across the transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archetype_id: Option<String>,
    /// Additional modality candidates that scored at or above the
    /// cross-omics threshold AND for which
    /// `is_cross_omics_intent` detected an explicit conjunction in
    /// the intake text. Empty when only one modality scored or when
    /// no cross-omics conjunction was detected. Pre-amendment
    /// sessions deserialize with `Vec::new()` thanks to
    /// `#[serde(default)]`. The primary `modality` field continues
    /// to hold the top-1 winner — single-modality consumers see no
    /// behavior change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_modalities: Vec<ModalityCandidate>,
    /// Top-N modality candidates within a 5% score
    /// margin of the winner. Empty when there's a clear winner; non-
    /// empty (≥2 entries including the winner) surfaces a tie that
    /// the LLM resolves via `propose_quick_replies` rather than
    /// `max_by_key`-style silent picking. Mirrors the archetype-
    /// layer 5%-window pattern at `composer.rs:594-607`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tie_candidates: Vec<TieCandidate>,
}

/// One candidate in a modality-classification tie.
/// Surfaced to the SME via `propose_quick_replies` so the chat
/// surface never silently picks. Includes the score (keyword hits)
/// and confidence so the SME quick-reply card can render a tooltip
/// explaining why each candidate is plausible.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct TieCandidate {
    /// Modality.
    pub modality: String,
    /// Display name.
    pub display_name: String,
    /// Keyword hits.
    pub keyword_hits: usize,
    /// Confidence.
    pub confidence: f32,
}

/// One secondary modality candidate surfaced when SME prose hits 2+
/// modalities at high confidence
/// AND the intake text carries an explicit cross-omics conjunction
/// ("transcriptomics and proteomics", "RNA-seq plus mass-spec",
/// "joint analysis of expression and protein abundance", etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct ModalityCandidate {
    /// Modality.
    pub modality: String,
    /// Taxonomy path.
    pub taxonomy_path: String,
    /// Edam topic.
    pub edam_topic: String,
    /// Edam operation.
    pub edam_operation: String,
    /// Confidence.
    pub confidence: f32,
    /// Keyword hits.
    pub keyword_hits: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
/// DataSourceRef data.
pub struct DataSourceRef {
    /// Accession.
    pub accession: String,
    /// Kind.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Qualifier.
    pub qualifier: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    /// Children.
    pub children: Vec<DataSourceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, schemars::JsonSchema)]
/// OrganismInfo data.
pub struct OrganismInfo {
    /// Name.
    pub name: String,
    /// Taxon id.
    pub taxon_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// MethodSpec data.
pub struct MethodSpec {
    /// Stage.
    pub stage: String,
    /// Method.
    pub method: String,
}

// ── Config types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// ModalityKeywordsConfig data.
pub struct ModalityKeywordsConfig {
    /// Per-modality entries.
    ///
    /// The authoritative source is `config/modalities/<id>.yaml`,
    /// loaded into [`ModalityRegistry`] and merged in by
    /// [`Classifier::load`]. Any modalities present here in the
    /// legacy YAML are rejected at load time so the two sources
    /// cannot drift. Default empty for the YAML shape where the
    /// block is absent entirely.
    #[serde(default)]
    pub modalities: Vec<ModalityEntry>,
    /// Method keywords.
    pub method_keywords: Vec<MethodKeyword>,
    /// Organism keywords.
    pub organism_keywords: Vec<OrganismKeyword>,
    /// Data source patterns.
    pub data_source_patterns: Vec<DataSourcePattern>,
    /// Keyword phrases that route to a typed `GoalSpec`.
    /// Optional in the YAML so configs persisted before this block
    /// existed continue to load. Order in the YAML is significant:
    /// the FIRST matching pattern wins (lexical scan; no scoring).
    /// SMEs whose intake text doesn't contain any goal phrase get
    /// `ClassificationResult::goal = None`, which the composer
    /// (S7.2) treats as "no constraint, route by archetype only."
    #[serde(default)]
    pub goal_patterns: Vec<GoalPattern>,
}

/// Single keyword-path goal-extraction rule.
///
/// `phrases` are matched against the lowercased + space-normalized
/// intake text via substring search (same `normalize_for_match`
/// pipeline as the modality keywords). On match, the rule's
/// `edam_data` / `edam_format` / `modifiers` populate the
/// `GoalSpec` returned by `Classifier::extract_goal`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct GoalPattern {
    /// Phrases.
    pub phrases: Vec<String>,
    /// Edam data.
    pub edam_data: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Edam format.
    pub edam_format: Option<String>,
    #[serde(default)]
    /// Modifiers.
    pub modifiers: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// ModalityEntry data.
pub struct ModalityEntry {
    /// Id.
    pub id: String,
    /// Taxonomy.
    pub taxonomy: String,
    /// Keywords.
    pub keywords: Vec<String>,
    /// Edam topic.
    pub edam_topic: String,
    /// Edam operation.
    pub edam_operation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// MethodKeyword data.
pub struct MethodKeyword {
    /// Keyword.
    pub keyword: String,
    /// Stage.
    pub stage: String,
    /// Method.
    pub method: String,
    #[serde(default = "default_match")]
    /// Field value.
    pub r#match: String,
}

fn default_match() -> String {
    "word".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// OrganismKeyword data.
pub struct OrganismKeyword {
    /// Keyword.
    pub keyword: String,
    /// Name.
    pub name: String,
    /// Taxon id.
    pub taxon_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// DataSourcePattern data.
pub struct DataSourcePattern {
    /// Pattern.
    pub pattern: String,
    /// Label.
    pub label: String,
}

// ── Normalization ─────────────────────────────────────────────────────────────

/// Lowercase and collapse hyphens/underscores to spaces so that
/// "single-cell", "single cell", and "single_cell" all match each other.
pub fn normalize_for_match(s: &str) -> String {
    s.to_lowercase().replace(['-', '_'], " ")
}

/// Match an organism keyword against tokenized + full-text inputs.
/// Single alphanumeric tokens match as whole words (case-insensitive) so that
/// "rat" does not match inside "regulatory". Multi-word or punctuated keywords
/// ("homo sapiens", "e. coli") fall back to substring match, which is safe
/// because they're specific enough to not collide with common English.
fn organism_keyword_matches(keyword: &str, lower_text: &str, words: &[String]) -> bool {
    let k = keyword.to_lowercase();
    let has_separator = k.chars().any(|c| c.is_whitespace() || c == '.');
    if has_separator {
        lower_text.contains(&k)
    } else {
        words.iter().any(|w| w.eq_ignore_ascii_case(&k))
    }
}

// ── Classifier ────────────────────────────────────────────────────────────────

/// Classifier data.
pub struct Classifier {
    config: ModalityKeywordsConfig,
    /// IDF-style discriminative weight per normalized keyword. A keyword
    /// shared across N modalities gets weight 1/N; a keyword unique to
    /// one modality gets weight 1.0. Used by the scoring loop in
    /// [`Self::classify`] so modality-specific tokens (e.g. "sgrna",
    /// "perturb seq", "wnn integration") outweigh generic shared-
    /// infrastructure tokens (e.g. "rna seq", "10x") that match
    /// multiple modalities. Computed once at load time;
    /// `BTreeMap` ordering keeps it deterministic across processes.
    keyword_idf: std::collections::BTreeMap<String, f32>,
}

impl Classifier {
    /// Load classifier configuration.
    ///
    /// Modality definitions are sourced from
    /// `config/modalities/<id>.yaml` (loaded via [`ModalityRegistry`]
    /// from `path.parent()/modalities/`). The legacy
    /// `modality-keywords.yaml` retains only the cross-cutting
    /// blocks (`method_keywords`, `organism_keywords`,
    /// `data_source_patterns`, `goal_patterns`). The file's
    /// `modalities:` block is rejected if non-empty so the two
    /// sources cannot drift.
    ///
    /// `ECAA_MODALITY_DRIFT_MODE=warn` (default) logs a warning when
    /// the YAML still carries a `modalities:` block; `=fail` refuses
    /// the load. Either way, the registry is authoritative.
    ///
    /// C22 / R-7: thin shim that reads the env var once and dispatches
    /// to [`Self::load_with_drift_mode`]. Production callers in
    /// long-lived processes should snapshot `ECAA_MODALITY_DRIFT_MODE`
    /// into `Config` at boot (the field is `Config::modality_drift_mode`)
    /// and call `load_with_drift_mode(path, cfg.modality_drift_mode)`
    /// to avoid touching the process environment on every load.
    pub fn load(path: &Path) -> Result<Self> {
        // One bounded env read here so existing call sites compile
        // unchanged. New code should prefer `load_with_drift_mode` with
        // a `Config`-sourced mode.
        #[allow(clippy::disallowed_methods)]
        let env_mode = std::env::var("ECAA_MODALITY_DRIFT_MODE").ok();
        let mode = match env_mode.as_deref() {
            Some(s) if s.eq_ignore_ascii_case("fail") => crate::config::ModalityDriftMode::Fail,
            _ => crate::config::ModalityDriftMode::Warn,
        };
        Self::load_with_drift_mode(path, mode)
    }

    /// Inject the drift-mode policy explicitly. The preferred entry
    /// point for any binary that owns a `Config` — keeps env reads at
    /// the process boundary instead of inside the deterministic
    /// classifier load.
    ///
    /// C22 / R-7: this is the env-free counterpart to [`Self::load`].
    pub fn load_with_drift_mode(
        path: &Path,
        drift_mode: crate::config::ModalityDriftMode,
    ) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading classifier config '{}'", path.display()))?;
        let mut config: ModalityKeywordsConfig = serde_yml::from_str(&content)
            .with_context(|| format!("parsing classifier config '{}'", path.display()))?;

        let modalities_dir = path
            .parent()
            .map(|p| p.join("modalities"))
            .ok_or_else(|| anyhow!("classifier config path '{}' has no parent", path.display()))?;
        let registry = ModalityRegistry::load_from_dir(&modalities_dir).with_context(|| {
            format!(
                "loading modality registry from '{}'",
                modalities_dir.display()
            )
        })?;
        if registry.is_empty() {
            return Err(anyhow!(
                "modality registry at '{}' is empty; populate per-modality manifests \
                 before loading the classifier (Plan §A.S5)",
                modalities_dir.display()
            ));
        }

        if !config.modalities.is_empty() {
            let legacy_modalities: Vec<(String, String, String, Vec<String>)> = config
                .modalities
                .iter()
                .map(|m| {
                    (
                        m.id.clone(),
                        m.edam_topic.clone(),
                        m.edam_operation.clone(),
                        m.keywords.clone(),
                    )
                })
                .collect();
            let drifts = detect_drift_against_legacy(&registry, &legacy_modalities);
            let strict = matches!(drift_mode, crate::config::ModalityDriftMode::Fail);
            if !drifts.is_empty() || strict {
                return Err(anyhow!(
                    "classifier config '{}' still carries a `modalities:` block that disagrees \
                     with the registry at '{}' (or drift_mode=Fail); migrate \
                     per-modality entries to config/modalities/<id>.yaml and remove the legacy \
                     block (Plan §A.S5). Drift: {:#?}",
                    path.display(),
                    modalities_dir.display(),
                    drifts
                ));
            }
            tracing::warn!(
                config = %path.display(),
                manifests = %modalities_dir.display(),
                "modality-keywords.yaml still carries a `modalities:` block; the registry is \
                 authoritative. Drop the legacy block to silence this warning."
            );
        }

        config.modalities = registry
            .iter()
            .map(|(id, def)| ModalityEntry {
                id: id.clone(),
                taxonomy: def.taxonomy_path.clone().unwrap_or_default(),
                keywords: def.keywords.clone(),
                edam_topic: def.edam_topic.clone(),
                edam_operation: def.edam_operation.clone(),
            })
            .collect();

        // Discriminative IDF weights. For each normalized keyword,
        // count how many modalities use it; the weight is 1/count
        // (clamped to [0.1, 1.0]). The selective scorer in
        // [`Self::classify`] sums these weights so a Perturb-seq
        // prompt that hits "sgrna" + "perturb seq" + "cas9 knockout
        // screen" (all unique to crispr_screen_scrnaseq) outweighs
        // an scRNA prompt that hits "10x" + "chromium" + "scrna seq"
        // (some shared with single_cell_rnaseq).
        let mut keyword_modality_count: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for m in &config.modalities {
            let unique: std::collections::HashSet<String> = m
                .keywords
                .iter()
                .map(|kw| normalize_for_match(kw))
                .collect();
            for kw in unique {
                if !kw.is_empty() {
                    *keyword_modality_count.entry(kw).or_insert(0) += 1;
                }
            }
        }
        let keyword_idf: std::collections::BTreeMap<String, f32> = keyword_modality_count
            .into_iter()
            .map(|(kw, count)| (kw, (1.0_f32 / count as f32).clamp(0.1, 1.0)))
            .collect();

        Ok(Self {
            config,
            keyword_idf,
        })
    }

    /// Classify.
    pub fn classify(&self, text: &str) -> ClassificationResult {
        let normalized_text = normalize_for_match(text);
        let excluded_modalities = explicit_modality_exclusions(&normalized_text);

        // Word-boundary tokenisation for method keyword matching
        let lower = text.to_lowercase();
        let words: Vec<String> = lower
            .split(|c: char| !c.is_alphanumeric() && c != '-')
            .filter(|w| !w.is_empty())
            .map(|w| w.to_string())
            .collect();

        // Find best-matching modality by IDF-weighted keyword score.
        // Each matched keyword contributes `keyword_idf[keyword]` (1.0
        // for keywords unique to one modality; 1/N for keywords shared
        // across N modalities). Modality-specific tokens dominate
        // generic shared-infrastructure tokens — so a Perturb-seq
        // prompt that hits "sgrna" + "perturb seq" + "cas9 knockout
        // screen" routes to crispr_screen_scrnaseq even when an
        // scRNA-overlapping prompt has more raw hits via shared
        // 10x/Chromium/scrna-seq tokens. Score is multiplied by 1000
        // and truncated to integer so `max_by_key` stays deterministic
        // across platforms. The generic_omics modality has an empty
        // keyword list and is used as the fallback.
        let scored: Vec<(&ModalityEntry, usize, usize, u32)> = self
            .config
            .modalities
            .iter()
            .filter(|m| !m.keywords.is_empty())
            .map(|m| {
                // Deduplicate after normalization so "rna-seq" and "rna seq"
                // don't count as two separate hits.
                let unique: std::collections::HashSet<String> = m
                    .keywords
                    .iter()
                    .map(|kw| normalize_for_match(kw))
                    .collect();
                let mut hits = 0usize;
                let mut weighted = 0.0_f32;
                for nkw in &unique {
                    if normalized_text.contains(nkw.as_str()) {
                        hits += 1;
                        weighted += self.keyword_idf.get(nkw).copied().unwrap_or(1.0);
                    }
                }
                let (hits, weighted) = if excluded_modalities.contains(m.id.as_str()) {
                    (0, 0.0)
                } else {
                    (hits, weighted)
                };
                // Quantize to u32 so the tie-break order is stable
                // across f32-precision platforms. 1000x scale = 3
                // decimal places of weighted resolution, well below
                // the IDF granularity (min weight 0.1 → 100).
                let weighted_score = (weighted * 1000.0).round() as u32;
                (m, unique.len(), hits, weighted_score)
            })
            .collect();

        let (best_modality, total_unique, keyword_hits, _) = scored
            .iter()
            // Sort by weighted score primarily; raw hit count breaks
            // ties (so a 0-IDF-weight modality with zero hits doesn't
            // beat a positive-hit modality just by happening to come
            // first in the iteration order).
            .max_by_key(|(_, _, hits, weighted)| (*weighted, *hits))
            .copied()
            .unwrap_or_else(|| {
                let fallback = self
                    .config
                    .modalities
                    .iter()
                    .find(|m| m.keywords.is_empty())
                    .expect("config must have a generic fallback modality with empty keywords");
                (fallback, 0, 0, 0)
            });

        // Definitive-keyword precedence override. Some specialized
        // modalities (STARR-seq, HiChIP, Perturb-seq, scATAC-seq,
        // CUT&Tag, Ribo-seq, immunopeptidomics) carry SME-recognizable
        // platform names that are unambiguous: when the prose names
        // them, that modality MUST win primary regardless of how the
        // IDF scorer ranks them against more-keyword-rich generic
        // modalities. Without this override, the IDF scorer routed
        // STARR-seq prompts to bulk_rnaseq (transcriptomics stratum)
        // because "RNA" / "differential expression" keywords outranked
        // "starr-seq" on weighted-hit count.
        const DEFINITIVE_KEYWORDS: &[(&str, &str)] = &[
            ("starr seq", "starr_seq"),
            ("starrseq", "starr_seq"),
            ("mpra", "starr_seq"),
            ("lentimpra", "starr_seq"),
            ("cre seq", "starr_seq"),
            ("hichip", "hi_chip"),
            ("hi chip", "hi_chip"),
            ("fithichip", "hi_chip"),
            ("hichipper", "hi_chip"),
            // Perturb-seq / CROP-seq route to single_cell_rnaseq so
            // single_cell_de archetype + protocol=perturb_seq slot
            // expansion provides the canonical pipeline. The legacy
            // crispr_screen_scrnaseq archetype was consolidated into
            // single_cell_de's protocol slot.
            ("perturb seq", "single_cell_rnaseq"),
            ("perturbseq", "single_cell_rnaseq"),
            ("crop seq", "single_cell_rnaseq"),
            ("cropseq", "single_cell_rnaseq"),
            // scATAC / snATAC intentionally OMITTED from definitive
            // keywords. Multiome / SHARE-seq prompts mention "snATAC"
            // alongside "snRNA" — routing those to atac_seq primary
            // would prevent the cross-omics companion-force +
            // protocol-slot expansion from picking
            // cross_omics_rnaseq_atac. Pure scATAC-only scenarios
            // (no paired RNA) are routed to generic_omics via
            // NOVEL_METHOD_TOKENS in rebuild_dag instead.
            ("cut and tag", "cut_tag"),
            ("cut tag", "cut_tag"),
            ("cuttag", "cut_tag"),
            ("ribo seq", "ribo_seq"),
            ("riboseq", "ribo_seq"),
            ("immunopeptidomics", "immunopeptidomics"),
            ("hla peptide", "immunopeptidomics"),
            // chip-exo precedence — the IDF-weighted tie between
            // chip_seq + chip_exo + bulk_rnaseq was non-deterministic
            // because chip_exo only contributed 1 hit on "chip exo"
            // while chip_seq's broader keywords pulled multiple hits
            // through bio-domain prose ("chromatin", "histone" — both
            // now removed from chip_seq.yaml but adding chip_exo to
            // DEFINITIVE_KEYWORDS hardens the precedence regardless).
            ("chip exo", "chip_exo"),
            ("chipexo", "chip_exo"),
            ("chip-exo", "chip_exo"),
            ("exonuclease chip", "chip_exo"),
            ("peakzilla", "chip_exo"),
            ("mace peak", "chip_exo"),
            ("chexmix", "chip_exo"),
        ];
        let (best_modality, total_unique, keyword_hits) = {
            let mut precedence_match: Option<(&ModalityEntry, usize, usize)> = None;
            for (token, target_id) in DEFINITIVE_KEYWORDS {
                if normalized_text.contains(token) {
                    if let Some((m, total, hits, _)) = scored
                        .iter()
                        .find(|(m, _, _, _)| m.id == *target_id)
                        .copied()
                    {
                        // Allow override only when the target modality
                        // has at least 1 keyword hit (defends against
                        // an SME mentioning a method name in passing
                        // for a different modality's prose).
                        if hits >= 1 {
                            precedence_match = Some((m, total, hits));
                            break;
                        }
                    }
                }
            }
            if let Some((m, total, hits)) = precedence_match {
                if m.id != best_modality.id {
                    tracing::debug!(
                        original = %best_modality.id,
                        overridden_to = %m.id,
                        "definitive_keyword_precedence_override"
                    );
                }
                (m, total, hits)
            } else {
                (best_modality, total_unique, keyword_hits)
            }
        };

        // Confidence: saturate at 3 hits = 1.0. Old formula was
        // `hits / total_unique_keywords` which mis-scored modalities
        // with rich keyword catalogs (bulk_rnaseq has 17+ keywords;
        // 4 hits scored 0.22 even though the prompt was unmistakably
        // a bulk RNA-seq study). Empirically 3 distinct keyword
        // matches is enough signal — past that, the marginal benefit
        // of more hits doesn't change the routing decision. Anything
        // below 3 hits scales linearly so single-keyword matches are
        // Still flagged as low-confidence. (fix; the
        // 0.22 confidence on Plan 11 was the trigger.)
        let confidence = if total_unique == 0 || keyword_hits == 0 {
            0.0_f32
        } else {
            (keyword_hits as f32 / 3.0_f32).min(1.0)
        };
        let confidence_label = confidence_label(confidence);

        // When confidence is effectively zero but we matched something, check if the
        // matched modality actually had any keywords — if not, fall back to generic.
        let modality = if keyword_hits == 0 && total_unique > 0 {
            self.config
                .modalities
                .iter()
                .find(|m| m.keywords.is_empty())
                .unwrap_or(best_modality)
        } else {
            best_modality
        };

        // Method keywords
        let methods_specified = self
            .config
            .method_keywords
            .iter()
            .filter(|mk| match mk.r#match.as_str() {
                "word" => words.iter().any(|w| w.eq_ignore_ascii_case(&mk.keyword)),
                _ => lower.contains(&mk.keyword.to_lowercase()),
            })
            .map(|mk| MethodSpec {
                stage: mk.stage.clone(),
                method: mk.method.clone(),
            })
            .collect();

        // Organism keywords: word-boundary for single tokens, substring for multi-word.
        // See organism_keyword_matches for rationale (prevents "rat" matching "regulatory").
        let organisms: Vec<OrganismInfo> = {
            let mut seen = std::collections::HashSet::new();
            self.config
                .organism_keywords
                .iter()
                .filter(|ok| organism_keyword_matches(&ok.keyword, &lower, &words))
                .filter(|ok| seen.insert(ok.taxon_id))
                .map(|ok| OrganismInfo {
                    name: ok.name.clone(),
                    taxon_id: ok.taxon_id,
                })
                .collect()
        };

        let data_sources = self.extract_data_sources(text);

        // Keyword-path goal extraction. Reads the
        // optional `goal_patterns:` block from `modality-keywords.yaml`
        // and matches phrases against the intake text. None when no
        // pattern matches OR when the config doesn't carry the block;
        // composer falls through to LLM-extracted goal in that case.
        let goal = self.extract_goal(text);

        // Surface secondary modality candidates when the SME
        // explicitly asked for cross-omics
        // (two or more modalities each clearing threshold AND a
        // cross-omics conjunction phrase in the prose). The primary
        // `modality` is unchanged; this only populates the
        // `additional_modalities` companion vector.
        let additional_modalities =
            self.collect_cross_omics_candidates(&normalized_text, &scored, modality);

        // Modality-tie surfacing. When two or more
        // modalities score within the 5% margin AND the cross-omics
        // gate did NOT fire (otherwise the tie is the explicit
        // multi-modality intent, not a disambiguation request),
        // populate `tie_candidates` so the chat surface uses
        // `propose_quick_replies` instead of silently committing.
        let tie_candidates = if additional_modalities.is_empty() {
            collect_tie_candidates(&scored, keyword_hits)
        } else {
            Vec::new()
        };

        ClassificationResult {
            modality: modality.id.clone(),
            taxonomy_path: modality.taxonomy.clone(),
            domain: String::new(), // filled by caller after loading taxonomy
            workflow_description: String::new(),
            edam_topic: modality.edam_topic.clone(),
            edam_operation: modality.edam_operation.clone(),
            confidence,
            confidence_label,
            organisms,
            methods_specified,
            data_sources,
            intake_text: text.to_string(),
            goal,
            // Left None on the legacy classifier path;
            // the composer fast-path (S6.13 / composer_version >= 2)
            // populates this when an archetype matches.
            archetype_id: None,
            additional_modalities,
            tie_candidates,
        }
    }

    /// Gather modality candidates beyond the primary winner that
    /// should ride along as cross-omics companions. Returns empty
    /// unless BOTH (a) at least one
    /// non-primary modality clears the cross-omics threshold AND
    /// (b) `is_cross_omics_intent` finds a conjunction phrase in the
    /// normalized intake text.
    ///
    /// The two-gate design keeps single-modality intake bug-compatible:
    /// hitting a stray "deseq2" while talking about proteomics doesn't
    /// silently fan the DAG out to RNA-seq, because the conjunction
    /// gate filters out incidental keyword spillover.
    fn collect_cross_omics_candidates(
        &self,
        normalized_text: &str,
        scored: &[(&ModalityEntry, usize, usize, u32)],
        primary: &ModalityEntry,
    ) -> Vec<ModalityCandidate> {
        // Narrow-scope modalities legitimately incorporate
        // ChIP-seq / ATAC-seq / bulk RNA-seq / single_cell vocabulary
        // because they share infrastructure (CUT&Tag uses chip_seq
        // tooling; ChIP-exo cites variant-calling aligners; Perturb-seq
        // is built ON TOP OF scRNA-seq, etc.). Listing these as
        // cross-omics companions just blocks emission. The primary
        // modality already carries the right archetype, so suppress
        // companion surfacing entirely when the primary is one of
        // these specialized modalities.
        const NARROW_SCOPE_MODALITIES: &[&str] = &[
            // `methylation` is deliberately NOT in this list:
            // legitimate RNA+methylation cross-omics scenarios
            // (cross_omics_rnaseq_methylation archetype) need
            // methylation to surface companion modalities. The
            // SUPPRESSED_PAIRS list neuters the bulk_rnaseq +
            // variant_calling false-positive companions that
            // methylation prose's shared infrastructure (Bismark /
            // BWA) would otherwise pull in.
            "cut_tag",
            "chip_exo",
            "ribo_seq",
            "starr_seq",
            "hi_chip",
            "single_cell_vdj",
            "immunopeptidomics",
            "crispr_screen_scrnaseq",
            "spatial_transcriptomics",
            "long_read_rnaseq",
        ];
        if NARROW_SCOPE_MODALITIES.contains(&primary.id.as_str()) {
            return Vec::new();
        }

        // Modality-pair suppression for shared
        // infrastructure overlap. Pairs listed here are NEVER
        // promoted to cross-omics companions even when keyword
        // counts exceed the threshold, because the secondary's
        // keywords are infrastructure mentions inside the primary's
        // workflow, not a co-equal modality.
        //
        // Example: an scRNA-seq prompt typically mentions "STAR",
        // "Cell Ranger", "DESeq2", "differential expression" — all
        // valid scRNA tools but they're also bulk_rnaseq keywords.
        // Listing bulk_rnaseq as a cross-omics companion forces
        // a set-mismatch with the right multimodal archetype.
        //
        // The set is intentionally small — only suppress when the
        // overlap is established as common confusion, not as a
        // catch-all.
        const SUPPRESSED_PAIRS: &[(&str, &str)] = &[
            // scRNA prose mentions bulk-rna tools as infrastructure
            ("single_cell_rnaseq", "bulk_rnaseq"),
            // Methylation prose mentions Bismark / BWA / variant-calling
            // tooling but the workflow is methylation, not variant.
            ("methylation", "variant_calling"),
            ("methylation", "bulk_rnaseq"),
            // ATAC prose mentions BWA, MACS2, ENCODE blacklist —
            // chip_seq tooling that is shared infrastructure.
            ("atac_seq", "chip_seq"),
            // ChIP-seq prose mentions ATAC's Tn5 nomenclature when
            // discussing tagmentation alternatives but isn't doing ATAC.
            ("chip_seq", "atac_seq"),
            // bulk RNA-seq prose with proteomics keywords as comparison.
            ("bulk_rnaseq", "proteomics"),
            // Metagenomics prose with "rna-seq" of microbial transcripts.
            ("metagenomics", "bulk_rnaseq"),
        ];

        /// Per-marker legitimate-companion declarations. The
        /// `SUPPRESSED_PAIRS` override is *only* lifted for companions
        /// listed under at least one marker present in the prompt.
        ///
        /// Tuple shape: `(marker_phrase, &[primary, companion])` — both
        /// must be modality ids (matching `config/modalities/*.yaml`
        /// `id` field). The marker-phrase string is matched against
        /// `normalized_text` (lowercased + hyphens/underscores → spaces).
        ///
        /// Without an entry for a (marker, suppressed_pair) tuple the
        /// suppression *stays* — preventing bulk_rnaseq leakage on
        /// prompts whose strong marker (multiome, share-seq, joint
        /// embedding) implies a different companion set than bulk.
        const MARKER_COMPANION_INTENT: &[(&str, &[(&str, &str)])] = &[
            // sc-rna paired-omics protocols imply atac, not bulk.
            ("multiome", &[("single_cell_rnaseq", "atac_seq")]),
            ("multiome arc", &[("single_cell_rnaseq", "atac_seq")]),
            ("share seq", &[("single_cell_rnaseq", "atac_seq")]),
            ("share-seq", &[("single_cell_rnaseq", "atac_seq")]),
            ("shareseq", &[("single_cell_rnaseq", "atac_seq")]),
            ("joint embedding", &[("single_cell_rnaseq", "atac_seq")]),
            // Named cross-omics integrators imply bulk + proteomics.
            (
                "diablo",
                &[("bulk_rnaseq", "proteomics"), ("proteomics", "bulk_rnaseq")],
            ),
            (
                "mofa",
                &[("bulk_rnaseq", "proteomics"), ("proteomics", "bulk_rnaseq")],
            ),
            (
                "snf integration",
                &[("bulk_rnaseq", "proteomics"), ("proteomics", "bulk_rnaseq")],
            ),
            (
                "similarity network fusion",
                &[("bulk_rnaseq", "proteomics"), ("proteomics", "bulk_rnaseq")],
            ),
            // Generic cross-omics keywords lift everything (today's
            // behavior, narrowed under explicit markers).
            (
                "cross omics",
                &[
                    ("single_cell_rnaseq", "bulk_rnaseq"),
                    ("bulk_rnaseq", "proteomics"),
                    ("methylation", "variant_calling"),
                    ("methylation", "bulk_rnaseq"),
                    ("atac_seq", "chip_seq"),
                    ("chip_seq", "atac_seq"),
                    ("metagenomics", "bulk_rnaseq"),
                ],
            ),
            (
                "multi omics",
                &[
                    ("single_cell_rnaseq", "bulk_rnaseq"),
                    ("bulk_rnaseq", "proteomics"),
                    ("methylation", "variant_calling"),
                    ("methylation", "bulk_rnaseq"),
                    ("atac_seq", "chip_seq"),
                    ("chip_seq", "atac_seq"),
                    ("metagenomics", "bulk_rnaseq"),
                ],
            ),
            (
                "multiomics",
                &[
                    ("single_cell_rnaseq", "bulk_rnaseq"),
                    ("bulk_rnaseq", "proteomics"),
                    ("methylation", "variant_calling"),
                    ("methylation", "bulk_rnaseq"),
                    ("atac_seq", "chip_seq"),
                    ("chip_seq", "atac_seq"),
                    ("metagenomics", "bulk_rnaseq"),
                ],
            ),
            // Tri-omics: ATAC-chip-RNA explicit triad.
            (
                "three way analysis",
                &[
                    ("atac_seq", "chip_seq"),
                    ("chip_seq", "atac_seq"),
                    ("single_cell_rnaseq", "bulk_rnaseq"),
                ],
            ),
            (
                "tri omics",
                &[("atac_seq", "chip_seq"), ("chip_seq", "atac_seq")],
            ),
            (
                "tri-omics",
                &[("atac_seq", "chip_seq"), ("chip_seq", "atac_seq")],
            ),
        ];

        // Two independent gates:
        //
        // 1. **Suppression override**: when ANY cross-omics intent
        // marker is present (strong marker OR conjunction), the
        // `SUPPRESSED_PAIRS` shared-infrastructure heuristic stops
        // being defensible — the SME signaled multi-modality intent
        // explicitly. We override the suppression in that case.
        //
        // 2. **Threshold relaxation**: only the STRONG markers
        // (`cross omics`, `multi omics`, `multiomics`, named
        // integrators like `DIABLO` / `MOFA`) lower the hit-count
        // floor to 1. Bare conjunctions (`transcriptomics and
        // proteomics`) are not enough — single-keyword spillover
        // from sparse prose like "exploratory analysis of
        // transcriptomics and proteomics ideas" must not surface
        // companions. Confirmed by
        // `classify::tests::conjunction_alone_without_threshold_clearing_is_noop`.
        let strong_cross_omics_marker = has_strong_cross_omics_marker(normalized_text);
        let any_cross_omics_intent =
            strong_cross_omics_marker || is_cross_omics_intent(normalized_text);
        // ≥3 canonical-distinct comma-list modalities ("matched bulk
        // RNA-seq, ATAC-seq, and ChIP-seq") is a stronger signal than
        // 2-way conjunction intent: the SME has named three distinct
        // modalities by canonical synonym. In an n-way prompt each
        // modality naturally gets fewer keyword hits because attention
        // is split across branches, so the 2-hit floor under-counts
        // companions like chip_seq (which contributes just 1 hit:
        // "chip seq" itself) and drops them. Relaxing to 1 hit only
        // when `is_n_way_intent` fires keeps the
        // `conjunction_alone_without_threshold_clearing_is_noop`
        // regression intact (2-way conjunctions still demand ≥2 hits).
        let n_way_intent = is_n_way_intent(normalized_text);
        let (min_confidence, base_min_hits) = cross_omics_threshold();
        // Four-tier threshold by intent strength:
        // - Strong marker (named integrator like DIABLO, named
        //   protocol like multiome, explicit multi-omics keyword) →
        //   min_hits = 1. SME committed by name.
        // - N-way intent (≥3 canonical modalities in the comma list,
        //   or "tri-omics" / "three-way analysis" markers) →
        //   min_hits = 1. SME committed to ≥3 modalities by listing
        //   each one; each branch can carry only 1 dedicated keyword.
        // - 2-way conjunction-only intent ("transcriptomics and
        //   proteomics") → min_hits = 2. Sparse incidental prose
        //   ("transcriptomics and proteomics ideas") must not
        //   trigger false-positive companions. The
        //   `conjunction_alone_without_threshold_clearing_is_noop`
        //   regression test pins this lower bound.
        // - No intent → base_min_hits (3). Default keep-cross-omics-quiet
        //   for single-modality prose.
        let min_hits = if strong_cross_omics_marker || n_way_intent {
            1
        } else if any_cross_omics_intent {
            2
        } else {
            base_min_hits
        };
        let above_threshold: Vec<ModalityCandidate> = scored
            .iter()
            .filter(|(m, _, _, _)| m.id != primary.id)
            .filter(|(m, _, _, _)| {
                // Per-marker override: a SUPPRESSED_PAIR is lifted
                // only when SOME marker present in the prompt
                // explicitly lists (primary.id, m.id) as a legitimate
                // companion. Without a marker-specific permission
                // the suppression stays — even when other strong
                // cross-omics markers fire elsewhere in the prompt.
                let pair_is_suppressed = SUPPRESSED_PAIRS
                    .iter()
                    .any(|(p, s)| *p == primary.id.as_str() && *s == m.id.as_str());
                if !pair_is_suppressed {
                    return true;
                }
                let marker_lifts = MARKER_COMPANION_INTENT.iter().any(|(marker, pairs)| {
                    normalized_text.contains(marker)
                        && pairs
                            .iter()
                            .any(|(p, s)| *p == primary.id.as_str() && *s == m.id.as_str())
                });
                if marker_lifts {
                    return true;
                }
                // Suppression lift #2: when the SME explicitly conjoins
                // primary and companion via canonical synonyms ("rna seq
                // and bisulfite", "transcriptomics and proteomics" with
                // sufficient prose), the shared-infrastructure heuristic
                // is no longer defensible — the SME named both
                // modalities by name. Without this lift, cross-omics
                // prompts that don't use the academic terms "cross
                // omics" / "multi omics" / "multiomics" (the only
                // markers in MARKER_COMPANION_INTENT for these pairs)
                // would silently collapse to a single-modality DAG even
                // though the matching cross_omics_* archetype is
                // registered.
                //
                // The hit-count threshold (min_hits ≥ 2 for plain
                // conjunctions, base_min_hits=3 default) still protects
                // sparse-prose cases like the
                // `conjunction_alone_without_threshold_clearing_is_noop`
                // regression test.
                pair_explicitly_conjoined(normalized_text, primary.id.as_str(), m.id.as_str())
            })
            .filter_map(|(m, total, hits, _)| {
                if *hits < min_hits || *total == 0 {
                    return None;
                }
                let conf = (*hits as f32 / *total as f32).min(1.0);
                if conf < min_confidence {
                    return None;
                }
                Some(ModalityCandidate {
                    modality: m.id.clone(),
                    taxonomy_path: m.taxonomy.clone(),
                    edam_topic: m.edam_topic.clone(),
                    edam_operation: m.edam_operation.clone(),
                    confidence: conf,
                    keyword_hits: *hits,
                })
            })
            .collect();

        if above_threshold.is_empty() {
            return Vec::new();
        }
        if !is_cross_omics_intent(normalized_text) {
            return Vec::new();
        }
        above_threshold
    }
}

/// Detect explicit SME conjunction between two canonical modalities
/// by scanning the normalized text for any pair of
/// their synonyms separated by a conjunction word (with up to ~60
/// characters of intervening modifiers, to handle phrasing like
/// "matched bulk RNA-seq AND whole-genome bisulfite sequencing" where
/// the conjunction binds a noun phrase containing the synonym).
///
/// Used by [`Classifier::collect_cross_omics_candidates`] to lift the
/// [`SUPPRESSED_PAIRS`] shared-infrastructure heuristic when the SME
/// named both modalities by name. Without this lift, cross-omics
/// prompts that don't use the academic terms "cross omics" /
/// "multi omics" / "multiomics" silently collapse to a single-modality
/// DAG even though the matching `cross_omics_*` archetype exists in the
/// registry (config/archetypes/cross_omics_*.yaml).
fn pair_explicitly_conjoined(text: &str, primary: &str, companion: &str) -> bool {
    let syns_for = |id: &str| -> Vec<&'static str> {
        SYNONYM_TO_MODALITY
            .iter()
            .filter(|(_, mid)| *mid == id)
            .map(|(syn, _)| *syn)
            .collect()
    };
    let primary_syns = syns_for(primary);
    let companion_syns = syns_for(companion);
    if primary_syns.is_empty() || companion_syns.is_empty() {
        return false;
    }
    // Match the same conjunction tokens [`is_cross_omics_intent`] uses,
    // with surrounding spaces to enforce word boundaries.
    const CONJUNCTIONS: &[&str] = &[
        " and ",
        " plus ",
        " as well as ",
        " together with ",
        " alongside ",
        " paired with ",
        " matched with ",
        " combined with ",
    ];
    // Allow up to ~60 characters between the conjunction and the second
    // synonym so phrasing like "rna seq and whole-genome bisulfite
    // sequencing" matches (the conjunction binds a noun phrase that
    // contains the second synonym partway through).
    const POST_CONJ_WINDOW: usize = 60;
    for syn_a in &primary_syns {
        for syn_b in &companion_syns {
            for conj in CONJUNCTIONS {
                for &(first, second) in &[(*syn_a, *syn_b), (*syn_b, *syn_a)] {
                    let head = format!("{first}{conj}");
                    let mut search_start = 0usize;
                    while let Some(rel_idx) = text[search_start..].find(&head) {
                        let conj_end = search_start + rel_idx + head.len();
                        let lookup_end = (conj_end + POST_CONJ_WINDOW).min(text.len());
                        if text[conj_end..lookup_end].contains(second) {
                            return true;
                        }
                        search_start = conj_end;
                    }
                }
            }
        }
    }
    false
}

/// Strong cross-omics markers. When the SME wrote one of
/// these, the [`SUPPRESSED_PAIRS`] heuristic in
/// Detects explicit n-way (≥3 modality) intent: "three-way analysis",
/// "tri-omics", "trio analysis", or a comma-list with ≥3 distinct
/// modality nouns. Wider than [`has_strong_cross_omics_marker`] (which
/// captures any cross-omics intent including 2-way) — `is_n_way_intent`
/// is the gate that unlocks the cross-omics superset fallback in
/// `archetype_registry::find_match_cross_omics`.
pub fn is_n_way_intent(prose: &str) -> bool {
    let n = normalize_for_match(prose);
    const N_WAY_MARKERS: &[&str] = &[
        "three way analysis",
        "three way",
        "tri omics",
        "tri-omics",
        "trio analysis",
        "n way integration",
        "three omics",
        "tri analysis",
    ];
    if N_WAY_MARKERS.iter().any(|m| n.contains(m)) {
        return true;
    }
    // Comma-list ≥3-distinct path. SMEs commonly write
    // "matched bulk RNA-seq, ATAC-seq, and ChIP-seq" — three commas,
    // three distinct modality nouns, no strong marker. We count
    // *canonical modality IDs*, not surface tokens, so a prompt that
    // distributes multiple synonyms of the same modality across comma
    // segments (e.g. "RRBS, bisulfite, methylation") collapses to a
    // single modality and does not falsely fire n_way_intent. Single-
    // modality prose with incidental commas ("RNA-seq, paired-end,
    // 150bp") also stays at 1.
    count_distinct_canonical_modalities_in_comma_list(&n, SYNONYM_TO_MODALITY) >= 3
}

/// Synonym → canonical-modality-id map. Each row maps one surface
/// token (already normalized: lowercased, hyphens/underscores collapsed
/// to spaces) to the canonical modality id it represents. Multiple
/// synonyms point to the same id; the n-way counter dedupes per
/// canonical id, so a prompt like "RRBS, bisulfite, methylation"
/// counts as 1 modality (methylation), not 3.
///
/// Order matters: longer synonyms come first so that "bisulfite
/// sequencing" wins over a bare "bisulfite" match in the same segment
/// (the segment-scan in `count_distinct_canonical_modalities_in_comma_list`
/// picks the first hit it finds). The contained-set is kept tight to
/// the modalities the cross-omics matcher actually routes on; adding a
/// canonical id without a matching archetype is a pure no-op.
const SYNONYM_TO_MODALITY: &[(&str, &str)] = &[
    // methylation family
    ("methylation array", "methylation"),
    ("dna methylation", "methylation"),
    ("bisulfite sequencing", "methylation"),
    ("methylome", "methylation"),
    ("methylation", "methylation"),
    ("bisulfite", "methylation"),
    ("rrbs", "methylation"),
    ("wgbs", "methylation"),
    // bulk RNA-seq family
    ("bulk rna", "bulk_rnaseq"),
    ("rna sequencing", "bulk_rnaseq"),
    ("gene expression", "bulk_rnaseq"),
    ("transcriptomics", "bulk_rnaseq"),
    ("rna seq", "bulk_rnaseq"),
    ("rnaseq", "bulk_rnaseq"),
    // single-cell RNA-seq family (must come before bulk rna_seq tokens
    // would otherwise win — handled by the longest-first ordering
    // above + the per-segment first-hit scan)
    ("single nucleus rna", "single_cell_rnaseq"),
    ("single cell rna", "single_cell_rnaseq"),
    ("single nucleus", "single_cell_rnaseq"),
    ("single cell", "single_cell_rnaseq"),
    ("scrna seq", "single_cell_rnaseq"),
    ("snrna seq", "single_cell_rnaseq"),
    ("scrna", "single_cell_rnaseq"),
    ("snrna", "single_cell_rnaseq"),
    // scATAC family — must precede the bulk atac entries so a token
    // like "scatac" doesn't accidentally match "atac" first.
    ("single nucleus atac", "scatac_seq"),
    ("single cell atac", "scatac_seq"),
    ("scatac seq", "scatac_seq"),
    ("snatac seq", "scatac_seq"),
    ("scatac", "scatac_seq"),
    ("snatac", "scatac_seq"),
    // bulk ATAC-seq family
    ("chromatin accessibility", "atac_seq"),
    ("atac sequencing", "atac_seq"),
    ("atac seq", "atac_seq"),
    ("atacseq", "atac_seq"),
    // ChIP-exo family — precedes plain chip-seq to win on shared substring.
    ("chip exo", "chip_exo"),
    ("chipexo", "chip_exo"),
    // ChIP-seq family
    ("chip sequencing", "chip_seq"),
    ("chip seq", "chip_seq"),
    ("chipseq", "chip_seq"),
    // CUT&RUN / CUT&Tag family
    ("cut and run", "cut_tag"),
    ("cut and tag", "cut_tag"),
    ("cut tag", "cut_tag"),
    // proteomics family
    ("mass spectrometry", "proteomics"),
    ("phosphoproteomics", "proteomics"),
    ("proteomics", "proteomics"),
    ("mass spec", "proteomics"),
    // metabolomics family
    ("metabolomics", "metabolomics"),
    ("lipidomics", "metabolomics"),
    // metagenomics family
    ("metagenomics", "metagenomics"),
    ("microbiome", "metagenomics"),
    // glycomics
    ("glycomics", "glycomics"),
    // spatial transcriptomics family
    ("spatial transcriptomics", "spatial_transcriptomics"),
    ("slide seq", "spatial_transcriptomics"),
    ("visium", "spatial_transcriptomics"),
    ("merfish", "spatial_transcriptomics"),
    ("xenium", "spatial_transcriptomics"),
    // variant-calling family
    ("whole genome sequencing", "variant_calling"),
    ("whole exome sequencing", "variant_calling"),
    ("variant calling", "variant_calling"),
    ("variant call", "variant_calling"),
    ("exome", "variant_calling"),
    ("wgs", "variant_calling"),
    ("wes", "variant_calling"),
    // GWAS family
    ("genome wide association", "gwas"),
    ("summary statistics", "gwas"),
    ("gwas", "gwas"),
    // standalone families
    ("epigenomics", "epigenomics"),
    ("genomics", "genomics"),
];

/// Comma-list canonical-modality counter. Splits the normalized text
/// on commas; for each segment scans the synonym table and records
/// every canonical modality id whose synonym occurs in the segment
/// (deduping multiple synonyms of the same canonical id within a
/// segment to ONE id). Across all segments, returns the count of
/// *distinct canonical ids*.
///
/// Match semantics:
/// - synonyms are pre-normalized (lowercased, hyphens/underscores
///   collapsed to spaces) — caller must pass a `text` produced by
///   [`normalize_for_match`];
/// - longer synonyms come first in the table so multi-word tokens win
///   over their prefixes when a segment is consumed (e.g. "bisulfite
///   sequencing" is detected before "bisulfite" alone would be);
/// - after a synonym matches, its substring is masked from the
///   working copy of the segment so shorter prefixes don't re-fire
///   (prevents "scrna" from also matching "rna" + double-counting);
/// - a segment that matches no synonym is ignored, which preserves
///   the comma-list intent (incidental commas like "RNA-seq,
///   paired-end, 150bp" still count as 1).
///
/// This is the canonical-id-aware sibling of
/// [`count_distinct_modality_nouns_in_comma_list`] (which counts
/// distinct surface tokens). It avoids false-fire on prompts that
/// distribute multiple synonyms of the same modality across comma
/// segments — e.g. "RRBS, bisulfite, methylation" surface-counts as
/// 3 tokens but represents only 1 canonical modality — AND counts
/// genuine multi-modality phrases like "scRNA-seq paired with
/// snATAC-seq" as 2 even when no comma separates them.
fn count_distinct_canonical_modalities_in_comma_list(
    text: &str,
    synonyms: &[(&str, &str)],
) -> usize {
    let mut hit_ids: Vec<&str> = Vec::new();
    for segment in text.split(',') {
        let mut working = segment.trim().to_string();
        for (synonym, canonical_id) in synonyms {
            if working.contains(*synonym) {
                // Mask the matched substring so a shorter synonym that
                // is a substring of `synonym` doesn't re-fire on this
                // segment (e.g. once "scrna" matches, the residual
                // "rna" inside it must not also match "rna seq").
                working = working.replace(*synonym, " ");
                if !hit_ids.contains(canonical_id) {
                    hit_ids.push(*canonical_id);
                }
            }
        }
    }
    hit_ids.len()
}

/// [`Classifier::collect_cross_omics_candidates`] is overridden — the
/// suppression is for shared-infrastructure keyword spillover, not for
/// intent the SME has signaled by name.
fn has_strong_cross_omics_marker(normalized_text: &str) -> bool {
    const MARKERS: &[&str] = &[
        "cross omics",
        "multi omics",
        "multiomics",
        "multi omic",
        "multiomic",
        "cross omic",
        "joint analysis",
        "combined analysis",
        "integrated analysis",
        "joint omics",
        "joint embedding",
        // N-way analysis phrasings — SMEs writing "three-way analysis"
        // or "tri-omics" have committed to ≥3 modalities by name.
        "three way analysis",
        "three way",
        "tri omics",
        "tri-omics",
        "trio analysis",
        "n way integration",
        // Named cross-omics integrators — when the SME asks for these
        // by tool name they have committed to multi-modality.
        "diablo",
        "mofa",
        "snf integration",
        "similarity network fusion",
        // Multi-modality platforms / protocols — naming any of these
        // is an explicit commitment to ≥2 modalities. Strong marker
        // because each name is specific enough to never be incidental.
        "multiome",
        "share seq",
        "shareseq",
        "share-seq",
        "wnn integration",
        "weighted nearest neighbor",
        // Compound multi-modal phrases — when the SME writes "RNA and
        // ATAC", "RNA and proteomics", etc., they have committed to
        // both modalities even if one of them only contributes a
        // single keyword hit elsewhere in the prose.
        "rna and atac",
        "atac and rna",
        "rna and chip",
        "chip and rna",
        "rna and methylation",
        "methylation and rna",
        "rna and proteomics",
        "proteomics and rna",
        "rna and protein",
        "protein and rna",
        "rna and bisulfite",
        "bisulfite and rna",
        "atac and chip",
        "chip and atac",
        "scrna and scatac",
        "snrna and snatac",
        "paired rna and atac",
        "paired single nucleus rna",
        "paired single cell rna",
        "joint rna and atac",
        "rna seq and atac seq",
        "rna seq and chip seq",
        "rna seq and methylation",
        "rna seq and bisulfite",
        "rna seq and proteomics",
        // Perturb-seq / sgRNA-augmented scRNA — the SME naming sgRNA
        // identity + transcriptome together is a commitment to the
        // CRISPR-screen scRNA archetype.
        "perturb seq",
        "perturbseq",
        "perturb-seq",
        "crispr screen",
        "sgrna assignment",
    ];
    MARKERS.iter().any(|m| normalized_text.contains(m))
}

/// Cross-omics threshold tuple `(min_confidence, min_hits)`. A
/// non-primary modality must clear
/// both gates to be surfaced as a cross-omics companion.
///
/// **Hit-count is the real gate; confidence is a soft floor.**
/// Modalities have very different keyword-set sizes (proteomics has
/// 10, bulk_rnaseq has 13–14, single_cell has more). A flat
/// percentage threshold would unfairly punish modalities with large
/// keyword sets — an SME mentioning "RNA-seq differential
/// expression" hits 2 keywords but only 2/13 confidence. The real
/// signal of cross-omics intent is `is_cross_omics_intent` (the
/// conjunction predicate); the hit-count gate prevents stray
/// single-keyword spillover (e.g. mentioning "kallisto" while
/// describing proteomics) from triggering surfacing. We keep a
/// trivial confidence floor (0.0) as a placeholder so the threshold
/// stays tunable if a future workload demands it.
pub fn cross_omics_threshold() -> (f32, usize) {
    // Bumped from 2 → 3. A non-primary modality needed
    // only 2 keyword hits to surface as a cross-omics companion, and
    // shared-infrastructure keywords were tripping the gate constantly:
    //
    // - ChIP-exo prose ("BWA-MEM", "alignment to GRCh38", "Tn5"
    // referenced for one footprint sentence) triggered atac_seq +
    // variant_calling as companions, blocked emission;
    // - SMART-seq2 plate scRNA prose triggered bulk_rnaseq via
    // "STAR", "featureCounts", "ERCC";
    // - Spatial transcriptomics prose triggered single_cell via
    // shared Seurat/SCTransform keywords.
    //
    // Requiring 3 distinct keyword hits gives the secondary modality
    // a real foothold — legitimate cross-omics intake mentions the
    // companion modality by name and methods, easily clearing 3.
    (0.0, 3)
}

fn explicit_modality_exclusions(normalized_text: &str) -> std::collections::BTreeSet<&'static str> {
    let mut excluded = std::collections::BTreeSet::new();

    // ATAC-seq scenarios commonly mention chip-seq tooling
    // (MACS2, ENCODE blacklist, narrow peak) as shared infrastructure
    // without intending a cross-omics ChIP-seq analysis. Honour explicit
    // negations so an ATAC SME can write "this is a pure
    // chromatin-accessibility study" without tripping the cross-omics gate.
    const CHIP_SEQ_EXCLUSIONS: &[&str] = &[
        "no chip seq",
        "no chip-seq",
        "without chip seq",
        "without chip-seq",
        "exclude chip seq",
        "drop chip seq",
        "pure chromatin accessibility",
        "pure chromatin accessibility study",
        "this is a pure atac",
        "atac seq only",
        "atac only",
        "no chip seq layer",
    ];
    if CHIP_SEQ_EXCLUSIONS
        .iter()
        .any(|phrase| normalized_text.contains(phrase))
    {
        excluded.insert("chip_seq");
    }

    const ATAC_SEQ_EXCLUSIONS: &[&str] = &[
        "no atac seq",
        "no atac-seq",
        "without atac seq",
        "exclude atac seq",
        "drop atac seq",
        "chip seq only",
        "no atac",
    ];
    if ATAC_SEQ_EXCLUSIONS
        .iter()
        .any(|phrase| normalized_text.contains(phrase))
    {
        excluded.insert("atac_seq");
    }

    const PROTEOMICS_EXCLUSIONS: &[&str] = &[
        "no proteomics",
        "without proteomics",
        "exclude proteomics",
        "drop proteomics",
        "proteomics is a separate follow on package",
        "proteomics as a separate follow on package",
        "proteomics separate follow on",
        "proteomics follow on package",
        "proteomics follow on",
    ];
    if PROTEOMICS_EXCLUSIONS
        .iter()
        .any(|phrase| normalized_text.contains(phrase))
    {
        excluded.insert("proteomics");
    }

    const SINGLE_CELL_EXCLUSIONS: &[&str] = &[
        "no single cell",
        "no single nucleus",
        "no single nuclei",
        "without single cell",
        "without single nucleus",
        "exclude single cell",
        "exclude single nucleus",
        "drop single cell",
        "drop single nucleus",
        "bulk rna seq only",
        "bulk rnaseq only",
        "bulk transcriptomics only",
    ];
    if SINGLE_CELL_EXCLUSIONS
        .iter()
        .any(|phrase| normalized_text.contains(phrase))
    {
        excluded.insert("single_cell_rnaseq");
    }

    const BULK_RNASEQ_EXCLUSIONS: &[&str] = &[
        "no bulk rna seq",
        "no bulk rnaseq",
        "without bulk rna seq",
        "without bulk rnaseq",
        "exclude bulk rna seq",
        "exclude bulk rnaseq",
        "drop bulk rna seq",
        "drop bulk rnaseq",
        "single cell only",
        "single nucleus only",
        // Bare-form negations for prompts that say
        // "NO RNA-seq layer" / "without rna-seq" without the "bulk"
        // qualifier. Plan 11-style mentions of "RNA-seq" alongside
        // an ATAC-seq study were tripping cross-omics surfacing.
        "no rna seq",
        "no rna-seq",
        "without rna seq",
        "without rna-seq",
        "exclude rna seq",
        "drop rna seq",
        "rna seq is a separate follow on",
        "rna seq separate follow on",
        "rna seq follow on package",
    ];
    if BULK_RNASEQ_EXCLUSIONS
        .iter()
        .any(|phrase| normalized_text.contains(phrase))
    {
        excluded.insert("bulk_rnaseq");
    }

    excluded
}

/// Score-margin tie window for modality-layer tie
/// surfacing. Mirrors the archetype-layer 5%-window pattern at
/// `composer.rs:594-607`: when two or more modalities score within
/// `MODALITY_TIE_WINDOW` of the top hit count, the classifier
/// surfaces all of them via `tie_candidates` so the chat layer
/// resolves via `propose_quick_replies` rather than `max_by_key`.
pub const MODALITY_TIE_WINDOW: f32 = 0.05;

/// Gather modality candidates within the 5% tie
/// window of the top score. Returns empty when there's a clear
/// winner (top hit count > runner-up by more than 5%) OR when the
/// top score is zero (generic fallback path; no tie). The chat
/// layer interprets a non-empty Vec as "ask the SME via
/// propose_quick_replies; do NOT silently commit."
fn collect_tie_candidates(
    scored: &[(&ModalityEntry, usize, usize, u32)],
    top_hits: usize,
) -> Vec<TieCandidate> {
    if top_hits == 0 {
        // Zero-hit fallback case — there's no tie to surface; the
        // generic_omics fallback handles this path silently.
        return Vec::new();
    }
    // Score margin: top_hits is the absolute hit count; tie window
    // computed as (top_hits - 5%) rounded down. We compare on the
    // raw integer hit counts for determinism.
    let cutoff_pct = top_hits as f32 * (1.0 - MODALITY_TIE_WINDOW);
    let cutoff_hits = cutoff_pct.floor() as usize;

    let mut tied: Vec<TieCandidate> = scored
        .iter()
        .filter(|(_, _total, hits, _)| *hits >= cutoff_hits && *hits > 0)
        .map(|(m, total, hits, _)| {
            let confidence = if *total == 0 {
                0.0_f32
            } else {
                (*hits as f32 / *total as f32).min(1.0)
            };
            TieCandidate {
                modality: m.id.clone(),
                display_name: m.id.clone(), // Caller can substitute via ModalityRegistry post-S5.
                keyword_hits: *hits,
                confidence,
            }
        })
        .collect();
    if tied.len() < 2 {
        return Vec::new();
    }
    // Stable order — desc by hit count, then alpha by id, so
    // propose_quick_replies surfaces in a deterministic order.
    tied.sort_by(|a, b| {
        b.keyword_hits
            .cmp(&a.keyword_hits)
            .then_with(|| a.modality.cmp(&b.modality))
    });
    tied
}

/// Explicit cross-omics intent predicate.
///
/// Looks for conjunction phrasing that signals "I want both" rather
/// than "I happen to mention X and Y." Conservative: a single-modality
/// request that says "RNA-seq and DESeq2" must NOT trigger because
/// "DESeq2" is a method keyword, not a modality. We rely on the
/// caller having already filtered the candidate list down to 2+
/// modalities clearing the keyword-hit threshold; this predicate is
/// the second gate.
///
/// Match patterns (substring on the normalized text — lowercase +
/// hyphen/underscore→space):
/// - "and" appearing between any two modality nouns ("transcriptomics and proteomics")
/// - "cross omics", "cross-omics", "multi omics", "multi-omics", "multiomics"
/// - "joint", "combined", "integrated" + "analysis"
/// - "as well as", "together with", "alongside", "plus" between modality nouns
fn is_cross_omics_intent(normalized_text: &str) -> bool {
    // Named-integrator + paired-protocol markers force cross-omics
    // intent unconditionally. Single source of truth lives in
    // [`has_strong_cross_omics_marker`]; we delegate to keep the two
    // gates aligned. Without this delegation the threshold-relaxation
    // gate (`base_min_hits→1` for strong markers) can fire while the
    // intent gate (this fn) returns false, dropping the companion set
    // even though the SME explicitly named a cross-omics method.
    if has_strong_cross_omics_marker(normalized_text) {
        return true;
    }
    // Strong, unambiguous markers — match anywhere.
    const STRONG_MARKERS: &[&str] = &[
        "cross omics",
        "multi omics",
        "multiomics",
        "joint analysis",
        "combined analysis",
        "integrated analysis",
        "joint omics",
    ];
    if STRONG_MARKERS.iter().any(|m| normalized_text.contains(m)) {
        return true;
    }
    // Modality nouns SMEs use in conjunction phrasing. Kept narrow to
    // avoid false positives on incidental words.
    //
    // Coverage rule of thumb: include the bare canonical name + each
    // common abbreviation + each multi-modal compound phrase. Bare
    // single-letter or two-letter tokens (e.g. "dna", "wgs", "wes")
    // need an additional word-boundary guard at match time to avoid
    // matching inside longer words; see the substring guard at
    // line ~960 below.
    const MODALITY_NOUNS: &[&str] = &[
        // Single-modality omics names.
        "transcriptomics",
        "proteomics",
        "metabolomics",
        "epigenomics",
        "genomics",
        "metagenomics",
        "phosphoproteomics",
        "lipidomics",
        "glycomics",
        // RNA-seq variants — bare "rna" is unsafe (matches "rna polymerase")
        // but the suffixed and prefixed forms are specific enough.
        "rna seq",
        "rnaseq",
        "rna sequencing",
        "scrna",
        "snrna",
        "scrna seq",
        "snrna seq",
        // Proteomics + mass-spec variants.
        "mass spec",
        "mass spectrometry",
        "gene expression",
        "protein expression",
        "protein abundance",
        "phosphoproteome",
        "phosphoproteomic",
        // ATAC / chromatin accessibility.
        "atac seq",
        "atacseq",
        "scatac",
        "snatac",
        "chromatin accessibility",
        // ChIP-seq + variants.
        "chip seq",
        "chipseq",
        "cut and run",
        "cut and tag",
        "cut tag",
        // Methylation / bisulfite variants.
        "methylation",
        "bisulfite",
        "bisulfite sequencing",
        "wgbs",
        "rrbs",
        "methylome",
        "methylomics",
        // Generic single-cell / spatial.
        "single cell",
        "single nucleus",
        "spatial transcriptomics",
        // Variant / WGS / WES — guarded specifically so "wgs" doesn't
        // match inside other words via word boundaries; substring is
        // safe since the noun is specific.
        "whole genome sequencing",
        "whole exome sequencing",
        "variant calling",
    ];

    // Compound multi-modality phrases — when ANY of these match, we
    // short-circuit return true. They cover SME phrasing like "paired
    // RNA and ATAC" where the bare "rna" / "atac" tokens don't appear
    // in MODALITY_NOUNS (intentionally; they'd false-positive too
    // easily on their own). The full compound disambiguates.
    const COMPOUND_MULTI_MODAL: &[&str] = &[
        "rna and atac",
        "rna + atac",
        "atac and rna",
        "atac + rna",
        "rna and chip",
        "chip and rna",
        "rna and methylation",
        "methylation and rna",
        "rna and proteomics",
        "rna and proteome",
        "rna and protein",
        "proteomics and rna",
        "protein and rna",
        "rna and bisulfite",
        "bisulfite and rna",
        "atac and chip",
        "chip and atac",
        "scrna and scatac",
        "snrna and snatac",
        "scrna + scatac",
        "snrna + snatac",
        "single nucleus rna and single nucleus atac",
        "single nucleus rna and atac",
        "single cell rna and atac",
        "joint rna and atac",
        "multiome",
        "multi omic",
        "multi-omic",
        "multiomic",
        "cross omic",
        "cross-omic",
        "share seq",
        "shareseq",
        "trio analysis",
        "rna seq and atac seq",
        "rna seq and chip seq",
        "rna seq and methylation",
        "rna seq and bisulfite",
    ];
    for phrase in COMPOUND_MULTI_MODAL {
        if normalized_text.contains(phrase) {
            return true;
        }
    }
    // Conjunction tokens (with surrounding spaces to enforce word boundaries).
    const CONJUNCTIONS: &[&str] = &[
        " and ",
        " plus ",
        " as well as ",
        " together with ",
        " alongside ",
    ];
    // Find any pair (noun_a, conjunction, noun_b) where noun_a != noun_b
    // and conjunction is between them. We do this by scanning conjunctions
    // and checking that distinct modality nouns appear before AND after.
    for conj in CONJUNCTIONS {
        let mut search_start = 0;
        while let Some(rel_idx) = normalized_text[search_start..].find(conj) {
            let conj_idx = search_start + rel_idx;
            let before = &normalized_text[..conj_idx];
            let after = &normalized_text[conj_idx + conj.len()..];
            let before_match = MODALITY_NOUNS.iter().find(|n| before.contains(*n)).copied();
            let after_match = MODALITY_NOUNS.iter().find(|n| after.contains(*n)).copied();
            if let (Some(a), Some(b)) = (before_match, after_match) {
                if a != b {
                    return true;
                }
            }
            search_start = conj_idx + conj.len();
        }
    }

    // Oxford-comma + and-list detection. The conjunction
    // loop above handles "X and Y" / "X plus Y" but misses comma-only
    // lists like "transcriptomics, proteomics, metabolomics" or the
    // Oxford-comma form "transcriptomics, proteomics, and metabolomics".
    // The trailing-and form is technically caught by the loop (the
    // last comma's `and` triggers between proteomics and metabolomics),
    // but we want commas alone to count too — SMEs often drop the
    // closing "and" in lists. We require the list to span ≥2 distinct
    // modality nouns separated by commas to avoid catching incidental
    // commas in single-modality prose ("RNA-seq, paired-end, 150bp").
    if has_modality_comma_list(normalized_text, MODALITY_NOUNS) {
        return true;
    }
    false
}

/// Comma-list detection. Splits the normalized text on
/// commas, scans each segment for at most one modality noun match,
/// and returns true when the list contains ≥2 distinct modality
/// nouns. Conservative: ignores segments without modality nouns
/// (handles "RNA-seq, paired-end, 150bp" — only "RNA-seq" matches a
/// modality, so it doesn't trigger).
fn has_modality_comma_list(text: &str, modality_nouns: &[&str]) -> bool {
    count_distinct_modality_nouns_in_comma_list(text, modality_nouns) >= 2
}

/// Helper exposing the distinct-modality-noun count in a comma-separated
/// modality list. Used by [`is_n_way_intent`] which needs ≥3 distinct
/// modality nouns to lift the cross-omics matcher's set-equality gate
/// to subset matching for tri-omics scenarios.
///
/// Same scanning rules as [`has_modality_comma_list`]: one modality
/// per segment, distinct modalities only. SMEs often write
/// "RNA-seq, ATAC-seq, and ChIP-seq" — three commas, three distinct
/// modality nouns, no strong marker; this returns 3.
pub(crate) fn count_distinct_modality_nouns_in_comma_list(
    text: &str,
    modality_nouns: &[&str],
) -> usize {
    let mut hits: Vec<&str> = Vec::new();
    for segment in text.split(',') {
        let seg = segment.trim();
        let mut found: Option<&str> = None;
        for n in modality_nouns {
            if seg.contains(*n) {
                if found.is_some() {
                    found = None;
                    break;
                }
                found = Some(*n);
            }
        }
        if let Some(n) = found {
            if !hits.contains(&n) {
                hits.push(n);
            }
        }
    }
    hits.len()
}

impl Classifier {
    /// Integrator + protocol keyword → `goal.modifiers` entries.
    ///
    /// Scanned by `extract_goal` against the FULL prompt
    /// (post-`normalize_for_match`) so the composer's cross-omics
    /// archetype discriminator at `composer/dispatch.rs:412` can pick
    /// the right archetype variant.
    ///
    /// Tuple shape: `(needle, &[(modifier_key, modifier_value)])`.
    /// Multiple needles can set the SAME modifier — first match wins
    /// in scan order so list the more-specific phrases first
    /// (e.g. "sparse pls da" before "diablo" if both appear).
    const INTEGRATOR_KIND_SCAN: &'static [(
        &'static str,
        &'static [(&'static str, &'static str)],
    )] = &[
        // Named cross-omics integrators (most specific first).
        (
            "sparse pls da",
            &[("kind", "supervised_cross_omics"), ("integrator", "diablo")],
        ),
        (
            "diablo",
            &[("kind", "supervised_cross_omics"), ("integrator", "diablo")],
        ),
        (
            "mofa",
            &[
                ("kind", "unsupervised_latent_factor"),
                ("integrator", "mofa"),
            ],
        ),
        (
            "multi omics factor analysis",
            &[
                ("kind", "unsupervised_latent_factor"),
                ("integrator", "mofa"),
            ],
        ),
        (
            "similarity network fusion",
            &[("kind", "network_fusion"), ("integrator", "snf")],
        ),
        (
            "snf integration",
            &[("kind", "network_fusion"), ("integrator", "snf")],
        ),
        // Paired-omics protocols.
        ("multiome arc", &[("kind", "arc_demultiplex")]),
        ("10x multiome", &[("kind", "arc_demultiplex")]),
        ("share seq", &[("kind", "share_seq_barcode")]),
        ("share-seq", &[("kind", "share_seq_barcode")]),
        ("shareseq", &[("kind", "share_seq_barcode")]),
    ];

    /// Keyword-path goal extraction. Walks
    /// `goal_patterns` in YAML order; first matching `phrases` entry
    /// wins. Returns `None` when no pattern matches (composer falls
    /// through to LLM-extracted `goal` in that case) OR when the
    /// config has no `goal_patterns:` block.
    ///
    /// Every emitted `GoalSpec` runs through
    /// `is_well_formed()` before being returned. A pattern with
    /// malformed EDAM IRIs (missing `data:`/`format:`/`ecaax:`
    /// prefix, non-numeric id, etc.) drops to `None` rather than
    /// propagating an invalid goal. Defends against config drift +
    /// the LLM-extraction path that piggybacks on this output.
    pub fn extract_goal(&self, text: &str) -> Option<GoalSpec> {
        let normalized = normalize_for_match(text);

        // Pre-scan: integrator + protocol keywords → modifier entries.
        // Collected BEFORE the goal_patterns loop so we can merge them
        // into any goal_pattern match below; also returned standalone
        // when no goal_pattern matched but an integrator was named.
        let mut scanned_modifiers: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        // Negation-aware: a prompt that says "No DIABLO / MOFA / SNF
        // requested" must not plumb an integrator/kind modifier. The guard
        // only fires when the negation cue reaches the matched token
        // through other integrator-vocabulary words / separators, so an
        // unrelated "no clean class labels … MOFA" still matches MOFA.
        let integrator_scan_vocab = crate::archetype_slots::vocabulary_from_tokens(
            Self::INTEGRATOR_KIND_SCAN.iter().map(|(needle, _)| *needle),
        );
        for (needle, entries) in Self::INTEGRATOR_KIND_SCAN {
            if let Some(pos) = normalized.find(needle) {
                if crate::archetype_slots::is_list_negated(
                    &normalized[..pos],
                    &integrator_scan_vocab,
                ) {
                    continue;
                }
                for (k, v) in *entries {
                    // First-match-wins per key, so list more-specific
                    // phrases first in INTEGRATOR_KIND_SCAN.
                    scanned_modifiers
                        .entry((*k).to_string())
                        .or_insert_with(|| (*v).to_string());
                }
            }
        }

        if self.config.goal_patterns.is_empty() && scanned_modifiers.is_empty() {
            return None;
        }
        for pattern in &self.config.goal_patterns {
            for phrase in &pattern.phrases {
                let needle = normalize_for_match(phrase);
                if needle.is_empty() {
                    continue;
                }
                if normalized.contains(&needle) {
                    let mut modifiers = pattern.modifiers.clone();
                    // Merge integrator-scan modifiers — named
                    // integrator+protocol scan is MORE SPECIFIC than
                    // a generic goal_pattern's `kind` value (DIABLO /
                    // MOFA / SNF / multiome-arc / share-seq directly
                    // identify the cross-omics archetype variant the
                    // composer's discriminator at dispatch.rs:412
                    // needs), so the scan takes precedence on key
                    // collision. Without this, a prompt with both a
                    // DE-leaning goal_pattern phrase and an integrator
                    // name lands with kind=differential_expression
                    // and the cross-omics discriminator can't pick
                    // the right archetype.
                    for (k, v) in &scanned_modifiers {
                        modifiers.insert(k.clone(), v.clone());
                    }
                    // Named-integrator discriminator. The dispatch-time
                    // hoist (composer/dispatch.rs) chooses among
                    // cross-omics archetypes that share the same
                    // modality set by scanning goal.source_prose +
                    // goal.modifiers values for known integrator
                    // tokens. source_prose only echoes the matched
                    // goal phrase, so plumb any integrator name found
                    // in the FULL prompt onto modifiers["integrator"]
                    // here so the hoist can see it downstream.
                    // (phrase, canonical_id_token) pairs. The token is the
                    // substring that must appear in the matching archetype's
                    // id; the dispatch hoists by `archetype.id.contains(token)`.
                    // Order matters — more specific phrases come first.
                    const INTEGRATOR_TOKENS: &[(&str, &str)] = &[
                        ("diablo", "diablo"),
                        ("spls-da", "diablo"),
                        ("spls da", "diablo"),
                        ("mixomics", "diablo"),
                        ("mofa", "mofa"),
                        ("factor decomposition", "mofa"),
                        ("factor analysis", "mofa"),
                        ("snf integration", "snf"),
                        ("similarity network fusion", "snf"),
                        ("similarity-network fusion", "snf"),
                        ("wnn integration", "multiome"),
                        ("weighted nearest neighbor", "multiome"),
                        ("multiome arc", "multiome"),
                        ("multiome", "multiome"),
                        ("share seq", "share_seq"),
                        ("shareseq", "share_seq"),
                        ("share-seq", "share_seq"),
                    ];
                    // Negation-aware scan: a prompt that says "No DIABLO /
                    // MOFA / SNF requested" must NOT plumb an integrator
                    // onto modifiers. The negation guard treats a cue as
                    // governing the match only when it is reachable through
                    // other integrator-vocabulary words / separators, so
                    // "no clean class labels … MOFA" still selects MOFA.
                    let integrator_vocab = crate::archetype_slots::vocabulary_from_tokens(
                        INTEGRATOR_TOKENS.iter().map(|(p, _)| *p),
                    );
                    for (phrase, canonical) in INTEGRATOR_TOKENS {
                        let phrase_norm = normalize_for_match(phrase);
                        if phrase_norm.is_empty() {
                            continue;
                        }
                        if let Some(pos) = normalized.find(&phrase_norm) {
                            if crate::archetype_slots::is_list_negated(
                                &normalized[..pos],
                                &integrator_vocab,
                            ) {
                                continue;
                            }
                            modifiers
                                .entry("integrator".to_string())
                                .or_insert_with(|| (*canonical).to_string());
                            break;
                        }
                    }
                    let goal = GoalSpec {
                        edam_data: pattern.edam_data.clone(),
                        edam_format: pattern.edam_format.clone(),
                        modifiers,
                        // Keyword-path extraction is deterministic
                        // (substring match) so the matched phrase
                        // itself is the prose-of-record.
                        source_prose: Some(phrase.clone()),
                        // Keyword-path is a 1.0 — substring matched
                        // exactly. LLM-path extraction (S6.4) sets
                        // its own (typically 0.5–0.95) confidence.
                        confidence: 1.0,
                    };
                    // Drop on EDAM-shape mismatch.
                    if !goal.is_well_formed() {
                        return None;
                    }
                    return Some(goal);
                }
            }
        }
        // Fallback: no goal_pattern matched but an integrator/protocol
        // keyword was found. Synthesize a partial goal whose
        // modifiers["kind"]/["integrator"] still let the composer's
        // cross-omics discriminator pick the right archetype. Empty
        // edam_data lets the composer route on modifiers + modality
        // archetype default instead of triggering GoalUnreachable at
        // validation time.
        if !scanned_modifiers.is_empty() {
            return Some(GoalSpec {
                edam_data: String::new(),
                edam_format: None,
                modifiers: scanned_modifiers,
                source_prose: Some(text.to_string()),
                confidence: 0.6,
            });
        }
        None
    }

    /// Extract accession IDs using prefix-based matching, then build hierarchy:
    /// - Strip underscore suffixes (e.g. PMID38403470_NP → PMID38403470 + qualifier "NP")
    /// - Nest GSM samples under their co-occurring GSE parent
    fn extract_data_sources(&self, text: &str) -> Vec<DataSourceRef> {
        struct RawHit {
            accession: String,
            kind: String,
            qualifier: Option<String>,
        }

        let tokens: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == '(' || c == ')')
            .map(|s| s.trim_matches(|c: char| !c.is_alphanumeric() && c != '-'))
            .filter(|s| !s.is_empty())
            .collect();

        let mut raw: Vec<RawHit> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for pattern in &self.config.data_source_patterns {
            let prefix: String = pattern
                .pattern
                .chars()
                .take_while(|c| c.is_alphabetic() || *c == '-')
                .collect();
            if prefix.is_empty() {
                continue;
            }
            for token in &tokens {
                let upper = token.to_uppercase();
                let pfx_upper = prefix.to_uppercase();
                if upper.starts_with(&pfx_upper) {
                    let rest = &upper[pfx_upper.len()..];
                    if rest
                        .chars()
                        .next()
                        .map(|c| c.is_ascii_digit() || c == '-')
                        .unwrap_or(false)
                        && rest.chars().any(|c| c.is_ascii_digit())
                    {
                        // Strip trailing _ALPHA suffix as a qualifier
                        let (base, qualifier) = if let Some(idx) = upper.rfind('_') {
                            let suffix = &upper[idx + 1..];
                            if !suffix.is_empty() && suffix.chars().all(|c| c.is_alphabetic()) {
                                (upper[..idx].to_string(), Some(suffix.to_string()))
                            } else {
                                (upper.clone(), None)
                            }
                        } else {
                            (upper.clone(), None)
                        };

                        if seen.insert(upper.clone()) {
                            raw.push(RawHit {
                                accession: base,
                                kind: pattern.label.clone(),
                                qualifier,
                            });
                        }
                    }
                }
            }
        }

        // Collect GSE accessions for parent-child linking
        let gse_set: std::collections::HashSet<&str> = raw
            .iter()
            .filter(|h| h.accession.starts_with("GSE"))
            .map(|h| h.accession.as_str())
            .collect();

        // Deduplicate on base accession, merging qualifiers
        let mut deduped: Vec<DataSourceRef> = Vec::new();
        for hit in &raw {
            if hit.accession.starts_with("GSM") && !gse_set.is_empty() {
                // GSM children are handled below
                continue;
            }
            if let Some(existing) = deduped.iter_mut().find(|d| d.accession == hit.accession) {
                // Merge qualifier from a duplicate base accession
                if let Some(q) = &hit.qualifier {
                    if existing.qualifier.is_none() {
                        existing.qualifier = Some(q.clone());
                    } else if let Some(eq) = &existing.qualifier {
                        if !eq.contains(q.as_str()) {
                            existing.qualifier = Some(format!("{}, {}", eq, q));
                        }
                    }
                }
            } else {
                deduped.push(DataSourceRef {
                    accession: hit.accession.clone(),
                    kind: hit.kind.clone(),
                    qualifier: hit.qualifier.clone(),
                    children: Vec::new(),
                });
            }
        }

        // Nest GSM samples under GSE parents when both appear in the text
        if !gse_set.is_empty() {
            let gsm_children: Vec<DataSourceRef> = raw
                .iter()
                .filter(|h| h.accession.starts_with("GSM"))
                .map(|h| DataSourceRef {
                    accession: h.accession.clone(),
                    kind: h.kind.clone(),
                    qualifier: h.qualifier.clone(),
                    children: Vec::new(),
                })
                .collect();

            if !gsm_children.is_empty() {
                // When multiple GSEs are present we can't resolve which GSM belongs
                // to which GSE without an API call — attach all to the first GSE.
                if let Some(parent) = deduped.iter_mut().find(|d| d.accession.starts_with("GSE")) {
                    parent.children = gsm_children;
                }
            }
        }

        // If no GSE parents, GSMs stay top-level
        if gse_set.is_empty() {
            for hit in &raw {
                if hit.accession.starts_with("GSM")
                    && !deduped.iter().any(|d| d.accession == hit.accession)
                {
                    deduped.push(DataSourceRef {
                        accession: hit.accession.clone(),
                        kind: hit.kind.clone(),
                        qualifier: hit.qualifier.clone(),
                        children: Vec::new(),
                    });
                }
            }
        }

        deduped
    }
}

fn confidence_label(c: f32) -> String {
    if c >= 0.7 {
        "high".into()
    } else if c >= 0.3 {
        "medium".into()
    } else {
        "low".into()
    }
}

// ── Project-class classifier ─────────────────────────────────────────────────
// Keyword-only classification; an LLM-assisted fallback is deferred
// until fixture runs surface misclassification that keywords can't
// disambiguate.

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// ProjectClassKeywordsConfig data.
pub struct ProjectClassKeywordsConfig {
    /// Classes.
    pub classes: Vec<ProjectClassEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
/// ProjectClassEntry data.
pub struct ProjectClassEntry {
    /// Id.
    pub id: String,
    /// Keywords.
    pub keywords: Vec<String>,
}

/// Load the project-class keyword table from a YAML file. Format parallel
/// to `config/modality-keywords.yaml` but simpler — no taxonomy/EDAM
/// fields because ProjectClass is a framework primitive, not a DAG shape.
pub fn load_project_class_keywords(path: &Path) -> Result<ProjectClassKeywordsConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading project-class config '{}'", path.display()))?;
    let config: ProjectClassKeywordsConfig = serde_yml::from_str(&content)
        .with_context(|| format!("parsing project-class config '{}'", path.display()))?;
    Ok(config)
}

/// Route intake text to a `ProjectClass`. Conservative per D7:
/// Bioinformatics wins ties and wins when no non-bio class accumulates
/// any keyword hits. The highest-hitting non-bio class wins only when
/// it strictly out-scores every other class.
pub fn classify_project_class(text: &str, config: &ProjectClassKeywordsConfig) -> ProjectClass {
    let lower = text.to_lowercase();
    let normalized = normalize_for_match(text);
    let words: Vec<String> = lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_string())
        .collect();

    let mut best: Option<(&str, usize)> = None;
    for class in &config.classes {
        let mut hits: usize = 0;
        let mut seen = std::collections::HashSet::new();
        for kw in &class.keywords {
            let nk = normalize_for_match(kw);
            if !seen.insert(nk.clone()) {
                continue;
            }
            let has_separator = nk.chars().any(|c| c.is_whitespace() || c == '.');
            let matched = if has_separator {
                normalized.contains(nk.as_str())
            } else {
                words.iter().any(|w| w.eq_ignore_ascii_case(&nk))
            };
            if matched {
                hits += 1;
            }
        }
        if hits > 0 {
            match best {
                Some((_, prev)) if hits > prev => best = Some((class.id.as_str(), hits)),
                Some((_, prev)) if hits == prev => {
                    // Tie — bio wins, so drop any accumulated winner.
                    // Mark as ambiguous by clearing best; fallthrough to
                    // default at the end.
                    best = Some(("", prev));
                }
                None => best = Some((class.id.as_str(), hits)),
                _ => {}
            }
        }
    }

    match best {
        Some(("clinical_trial", _)) => ProjectClass::ClinicalTrial,
        Some(("time_series_forecast", _)) => ProjectClass::TimeSeriesForecast,
        _ => ProjectClass::Bioinformatics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_classifier() -> Classifier {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/modality-keywords.yaml");
        Classifier::load(&path).expect("should load modality-keywords.yaml")
    }

    fn load_project_class_config() -> ProjectClassKeywordsConfig {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/project-class-keywords.yaml");
        load_project_class_keywords(&path).expect("should load project-class-keywords.yaml")
    }

    #[test]
    fn project_class_defaults_to_bioinformatics_on_bio_prose() {
        let cfg = load_project_class_config();
        let class = classify_project_class(
            "I need to run differential expression on single-cell RNA-seq from GEO",
            &cfg,
        );
        assert_eq!(class, ProjectClass::Bioinformatics);
    }

    #[test]
    fn project_class_routes_clinical_trial_on_sap_vocabulary() {
        let cfg = load_project_class_config();
        let class = classify_project_class(
            "Phase III randomized controlled trial with frozen SAP, primary endpoint analyzed \
             per prespecified ITT population; CDISC ADaM inputs",
            &cfg,
        );
        assert_eq!(class, ProjectClass::ClinicalTrial);
    }

    #[test]
    fn project_class_routes_time_series_on_forecast_vocabulary() {
        let cfg = load_project_class_config();
        let class = classify_project_class(
            "We need an ARIMA forecast of the daily time series with a 30-day forecast horizon \
             and stationarity checks; track RMSE and MAPE",
            &cfg,
        );
        assert_eq!(class, ProjectClass::TimeSeriesForecast);
    }

    #[test]
    fn project_class_bioinformatics_wins_ties() {
        let cfg = load_project_class_config();
        // Prose mixes one clinical keyword and one time-series keyword —
        // neither out-scores the other, so bio wins per D7.
        let class = classify_project_class("trial primary endpoint and seasonality analysis", &cfg);
        assert_eq!(class, ProjectClass::Bioinformatics);
    }

    #[test]
    fn project_class_does_not_misroute_on_1000_genomes_phase_3() {
        // GWAS prose may mention "1000 Genomes Phase 3 EUR" — a genomics
        // reference panel name, not a clinical-trial phase. The
        // project-class keyword set must require multi-word trial-context
        // forms (`phase 3 trial`, `phase 3 rct`, etc.) so a bare
        // `phase 3` / `phase iii` token in genomics prose does not route
        // the entire GWAS DAG to clinical_trial_analysis.
        let cfg = load_project_class_config();
        for prose in [
            "LD reference panel (1000 Genomes Phase 3 EUR), tissue-prioritization rule",
            "Use 1000 Genomes Phase 3 superpopulations for LD",
            "Phase III reference panel from the GRCh38 build",
            "We have phase 3 imputation against the TOPMed panel",
        ] {
            assert_eq!(
                classify_project_class(prose, &cfg),
                ProjectClass::Bioinformatics,
                "genomics-panel Phase-N vocabulary must NOT route to ClinicalTrial: {:?}",
                prose,
            );
        }
    }

    #[test]
    fn project_class_does_not_misroute_on_statistical_endpoint_vocabulary() {
        // "primary endpoint" / "secondary endpoint" appears in
        // statistical prose (the primary outcome of an analysis) and
        // not only in clinical-trial-protocol prose (the primary
        // outcome of a trial). The keyword set must require multi-word
        // trial-context forms (`trial primary endpoint`, `co-primary
        // endpoint`, etc.) so statistical-endpoint prose stays in
        // bioinformatics rather than collapsing to
        // clinical_trial_analysis.
        let cfg = load_project_class_config();
        for prose in [
            "AUROC is not a valid primary endpoint for a generalization claim",
            "The primary endpoint of the meta-analysis is cross-cohort discrimination",
            "Secondary endpoint of the comparison is replication in the held-out cohort",
            "Use LOCO AUC as the primary endpoint of the LOCO benchmark",
        ] {
            assert_eq!(
                classify_project_class(prose, &cfg),
                ProjectClass::Bioinformatics,
                "statistical-endpoint vocabulary must NOT route to ClinicalTrial: {:?}",
                prose,
            );
        }
    }

    #[test]
    fn project_class_does_not_misroute_on_url_endpoint_vocabulary() {
        // Regression: Run #6 misclassified the IVD intake as
        // ClinicalTrial because the prose contained "CNGB HTTPS
        // endpoint" — the bare "endpoint" keyword scored 1 vs
        // bioinformatics' implicit 0. Bare "endpoint" is too loose;
        // real clinical-trial intake hits "primary endpoint" /
        // "endpoint analysis" / SAP / CDISC etc. Ensure URL/API
        // vocabulary doesn't trip the classifier.
        let cfg = load_project_class_config();
        for prose in [
            "Pull the dataset via the CNGB HTTPS endpoint at ftp.cngb.org",
            "Fetch from the GEO REST endpoint with paginated calls",
            "The SRA endpoint URL accepts SRR accessions",
            "POST to the /api/inputs endpoint to register the path",
        ] {
            assert_eq!(
                classify_project_class(prose, &cfg),
                ProjectClass::Bioinformatics,
                "URL/API endpoint vocabulary must NOT route to ClinicalTrial: {:?}",
                prose,
            );
        }
    }

    #[test]
    fn project_class_routes_clinical_trial_on_endpoint_analysis_vocabulary() {
        // Companion to the regression above: the multi-word forms
        // (primary endpoint, endpoint analysis, endpoint adjudication)
        // still route to ClinicalTrial. Bare "endpoint" is what we
        // dropped; the structural variants stay.
        let cfg = load_project_class_config();
        for prose in [
            "Primary endpoint per the SAP — 12-week change from baseline",
            "Endpoint analysis stratified by treatment arm and ITT population",
            "Endpoint adjudication committee blind to treatment assignment",
        ] {
            assert_eq!(
                classify_project_class(prose, &cfg),
                ProjectClass::ClinicalTrial,
                "endpoint-analysis vocabulary must still route to ClinicalTrial: {:?}",
                prose,
            );
        }
    }

    #[test]
    fn classifies_bulk_rnaseq() {
        let clf = load_classifier();
        let r = clf.classify(
            "I need to analyze bulk RNA-seq from 3 GEO datasets for differential expression",
        );
        assert_eq!(r.modality, "bulk_rnaseq");
        assert_eq!(r.edam_topic, "topic:3308");
        assert_eq!(r.edam_operation, "operation:3223");
        assert!(r.confidence > 0.0);
    }

    #[test]
    fn bulk_rnaseq_all_unique_keywords_reaches_full_confidence() {
        let clf = load_classifier();
        let r = clf.classify(
            "rnaseq rna-seq differential expression bulk rna transcriptomics \
             gene expression gene expression analysis expression analysis \
             gene expression and proteomics deseq2 edger limma \
             salmon kallisto star hisat2 featurecounts htseq",
        );
        assert_eq!(r.modality, "bulk_rnaseq");
        assert!(
            (r.confidence - 1.0).abs() < f32::EPSILON,
            "all unique keywords should yield confidence 1.0, got {:.4}",
            r.confidence
        );
    }

    #[test]
    fn classifies_single_cell() {
        let clf = load_classifier();
        let r = clf.classify("10x single cell chromium clustering and trajectory analysis");
        assert_eq!(r.modality, "single_cell_rnaseq");
    }

    #[test]
    fn classifies_generic_omics_as_fallback() {
        let clf = load_classifier();
        let r = clf.classify("analyze some data");
        assert_eq!(r.modality, "generic_omics");
        assert_eq!(r.confidence, 0.0);
        assert_eq!(r.confidence_label, "low");
    }

    #[test]
    fn classifies_atac_seq() {
        let clf = load_classifier();
        let r = clf.classify(
            "ATAC-seq chromatin accessibility profiling with Tn5 transposase, \
             call accessible regions and quantify TSS enrichment",
        );
        assert_eq!(r.modality, "atac_seq");
        assert_eq!(r.edam_topic, "topic:3179");
        assert!(r.confidence > 0.0);
    }

    #[test]
    fn atac_specific_prose_does_not_route_to_chip_seq() {
        let clf = load_classifier();
        // Pure ATAC vocabulary — should win over chip_seq even though
        // both share "macs3" / "chromatin" potential overlap.
        let r =
            clf.classify("Omni-ATAC nucleosome positioning, accessible chromatin, HMMRATAC peaks");
        assert_eq!(
            r.modality, "atac_seq",
            "ATAC-specific prose must route to atac_seq, got {}",
            r.modality
        );
    }

    #[test]
    fn confidence_high_on_many_keywords() {
        let clf = load_classifier();
        let r =
            clf.classify("single cell scRNA-seq 10x chromium cellranger Seurat scanpy clustering");
        assert_eq!(r.modality, "single_cell_rnaseq");
        assert!(
            r.confidence >= 0.3,
            "many matching keywords should give medium+ confidence"
        );
    }

    #[test]
    fn cell_ranger_two_words_matches() {
        let clf = load_classifier();
        let r = clf.classify("We ran Cell Ranger on the 10x single cell data");
        assert_eq!(r.modality, "single_cell_rnaseq");
        assert!(r.confidence > 0.0, "two-word 'Cell Ranger' should match");
    }

    #[test]
    fn sme_prose_achieves_medium_confidence() {
        let clf = load_classifier();
        let r = clf.classify(
            "single-cell RNA-seq analysis of cell type composition, \
             cell population proportions, and cell state changes in human tissue",
        );
        assert_eq!(r.modality, "single_cell_rnaseq");
        assert!(
            r.confidence >= 0.3,
            "biology-only SME prose should reach medium confidence, got {:.0}%",
            r.confidence * 100.0
        );
    }

    #[test]
    fn word_boundary_star_does_not_match_starting() {
        let clf = load_classifier();
        let r = clf.classify("I am starting the analysis for bulk RNA-seq differential expression");
        // "starting" contains "star" but word boundary matching should NOT match STAR tool
        let star_methods: Vec<_> = r
            .methods_specified
            .iter()
            .filter(|m| m.method == "STAR")
            .collect();
        assert!(
            star_methods.is_empty(),
            "'starting' should not match STAR method keyword"
        );
    }

    #[test]
    fn word_boundary_star_matches_star() {
        let clf = load_classifier();
        let r = clf
            .classify("bulk RNA-seq alignment using STAR and DESeq2 for differential expression");
        let star = r.methods_specified.iter().any(|m| m.method == "STAR");
        let deseq2 = r.methods_specified.iter().any(|m| m.method == "DESeq2");
        assert!(star, "should match STAR as alignment method");
        assert!(deseq2, "should match DESeq2 as DE method");
    }

    fn has_accession(sources: &[DataSourceRef], acc: &str) -> bool {
        sources.iter().any(|d| d.accession == acc)
            || sources
                .iter()
                .any(|d| d.children.iter().any(|c| c.accession == acc))
    }

    fn has_accession_prefix(sources: &[DataSourceRef], prefix: &str) -> bool {
        sources.iter().any(|d| d.accession.starts_with(prefix))
            || sources
                .iter()
                .any(|d| d.children.iter().any(|c| c.accession.starts_with(prefix)))
    }

    #[test]
    fn extracts_geo_accessions() {
        let clf = load_classifier();
        let r =
            clf.classify("bulk RNA-seq from GSE123456 and GSE789012 for differential expression");
        assert!(has_accession(&r.data_sources, "GSE123456"));
        assert!(has_accession(&r.data_sources, "GSE789012"));
    }

    #[test]
    fn extracts_geo_with_kind_label() {
        let clf = load_classifier();
        let r = clf.classify("bulk RNA-seq from GSE123456 for differential expression");
        let ds = r.data_sources.iter().find(|d| d.accession == "GSE123456");
        assert!(ds.is_some(), "should find GSE123456");
        assert_eq!(ds.unwrap().kind, "NCBI GEO Series");
    }

    #[test]
    fn extracts_sra_accessions() {
        let clf = load_classifier();
        let r = clf.classify("data from SRR12345678 and PRJNA654321");
        assert!(
            has_accession(&r.data_sources, "SRR12345678")
                || has_accession_prefix(&r.data_sources, "SRR"),
        );
        assert!(
            has_accession(&r.data_sources, "PRJNA654321")
                || has_accession_prefix(&r.data_sources, "PRJNA"),
        );
    }

    #[test]
    fn extracts_pmid_accessions() {
        let clf = load_classifier();
        let r = clf.classify("IVD datasets: PMID35265617 (Han 2022) and PMID38403470 (Swahn 2024)");
        assert!(
            has_accession(&r.data_sources, "PMID35265617"),
            "should extract PMID35265617, got {:?}",
            r.data_sources
        );
        assert!(
            has_accession(&r.data_sources, "PMID38403470"),
            "should extract PMID38403470, got {:?}",
            r.data_sources
        );
    }

    #[test]
    fn extracts_pmc_accessions() {
        let clf = load_classifier();
        let r = clf.classify("open-access copy available at PMC1234567");
        assert!(
            has_accession_prefix(&r.data_sources, "PMC"),
            "should extract PMC accession, got {:?}",
            r.data_sources
        );
    }

    #[test]
    fn gsm_nests_under_gse() {
        let clf = load_classifier();
        let r = clf.classify(
            "single cell data from GSE251686 including samples GSM7432171 GSM7432172 GSM7432173",
        );
        let gse = r.data_sources.iter().find(|d| d.accession == "GSE251686");
        assert!(gse.is_some(), "should have GSE251686 as top-level");
        let children = &gse.unwrap().children;
        assert!(
            children.iter().any(|c| c.accession == "GSM7432171"),
            "GSM7432171 should nest under GSE251686, got {:?}",
            children
        );
        assert!(
            children.iter().any(|c| c.accession == "GSM7432172"),
            "GSM7432172 should nest under GSE251686"
        );
        // GSMs should not appear at top level
        assert!(
            !r.data_sources.iter().any(|d| d.accession == "GSM7432171"),
            "GSM7432171 should not be a top-level entry"
        );
    }

    #[test]
    fn standalone_gsm_stays_toplevel() {
        let clf = load_classifier();
        let r = clf.classify("single cell from sample GSM9999999 only");
        assert!(
            has_accession(&r.data_sources, "GSM9999999"),
            "standalone GSM should be extracted"
        );
        assert!(
            r.data_sources.iter().any(|d| d.accession == "GSM9999999"),
            "GSM without a co-occurring GSE should stay top-level"
        );
    }

    #[test]
    fn pmid_suffix_stripped() {
        let clf = load_classifier();
        let r = clf.classify("IVD NP data from PMID38403470_NP and AF data from PMID38403470_AF");
        assert!(
            has_accession(&r.data_sources, "PMID38403470"),
            "suffixed PMIDs should be collapsed to base accession, got {:?}",
            r.data_sources
        );
        let pmid = r
            .data_sources
            .iter()
            .find(|d| d.accession == "PMID38403470");
        assert!(pmid.is_some());
        let q = pmid.unwrap().qualifier.as_deref().unwrap_or("");
        assert!(
            q.contains("NP") && q.contains("AF"),
            "qualifiers NP and AF should be captured, got {:?}",
            q
        );
    }

    #[test]
    fn extracts_organism_human() {
        let clf = load_classifier();
        let r = clf.classify("bulk RNA-seq from human PBMC samples differential expression");
        let human = r.organisms.iter().any(|o| o.taxon_id == 9606);
        assert!(human, "should extract Homo sapiens (9606)");
    }

    #[test]
    fn organism_rat_does_not_match_regulatory() {
        let clf = load_classifier();
        let r = clf.classify("gene regulatory networks and pseudobulk analysis in human IVD");
        assert!(
            r.organisms.iter().any(|o| o.taxon_id == 9606),
            "should still extract human"
        );
        assert!(
            !r.organisms.iter().any(|o| o.taxon_id == 10116),
            "'rat' must not match inside 'regulatory': got {:?}",
            r.organisms
        );
    }

    #[test]
    fn organism_rat_matches_standalone_word() {
        let clf = load_classifier();
        let r = clf.classify("rat brain cortex scRNA-seq analysis");
        assert!(
            r.organisms.iter().any(|o| o.taxon_id == 10116),
            "standalone 'rat' should match Rattus norvegicus"
        );
    }

    #[test]
    fn organism_multiword_homo_sapiens_still_matches() {
        let clf = load_classifier();
        let r = clf.classify("scRNA-seq from Homo sapiens samples");
        assert!(
            r.organisms.iter().any(|o| o.taxon_id == 9606),
            "'Homo sapiens' multi-word keyword should still match"
        );
    }

    #[test]
    fn confidence_label_thresholds() {
        assert_eq!(confidence_label(0.8), "high");
        assert_eq!(confidence_label(0.5), "medium");
        assert_eq!(confidence_label(0.1), "low");
        assert_eq!(confidence_label(0.0), "low");
    }

    /// Keyword-path goal extraction picks up the
    /// FIRST matching pattern in YAML order. `extract_goal`
    /// returns `None` when no pattern matches OR when the YAML
    /// has no `goal_patterns:` block.
    #[test]
    fn extract_goal_picks_first_matching_pattern() {
        let mut config = ModalityKeywordsConfig {
            modalities: vec![],
            method_keywords: vec![],
            organism_keywords: vec![],
            data_source_patterns: vec![],
            goal_patterns: vec![
                GoalPattern {
                    phrases: vec!["differential expression".into()],
                    edam_data: "data:0951".into(),
                    edam_format: Some("format:3475".into()),
                    modifiers: std::collections::BTreeMap::new(),
                },
                GoalPattern {
                    phrases: vec!["clustered annotation".into()],
                    edam_data: "data:3917".into(),
                    edam_format: Some("format:3590".into()),
                    modifiers: std::collections::BTreeMap::new(),
                },
            ],
        };
        let cls = Classifier {
            config: config.clone(),
            keyword_idf: std::collections::BTreeMap::new(),
        };

        // First pattern matches.
        let g = cls
            .extract_goal("I want differential expression results across arms.")
            .unwrap();
        assert_eq!(g.edam_data, "data:0951");
        assert_eq!(g.edam_format.as_deref(), Some("format:3475"));
        assert!((g.confidence - 1.0).abs() < f32::EPSILON);
        assert_eq!(
            g.source_prose.as_deref(),
            Some("differential expression"),
            "source_prose should echo the matched phrase"
        );

        // Second pattern matches when first doesn't.
        let g = cls
            .extract_goal("I want a clustered annotation per cell type.")
            .unwrap();
        assert_eq!(g.edam_data, "data:3917");

        // No pattern matches → None.
        assert!(cls.extract_goal("plain genome assembly only").is_none());

        // Empty patterns → None.
        config.goal_patterns.clear();
        let cls = Classifier {
            config,
            keyword_idf: std::collections::BTreeMap::new(),
        };
        assert!(cls
            .extract_goal("differential expression results please")
            .is_none());
    }

    /// Phrase matching reuses the modality-keyword
    /// `normalize_for_match` (lowercase + hyphen/underscore →
    /// space) so SME prose with various punctuation still matches.
    #[test]
    fn extract_goal_normalizes_phrasing() {
        let cls = Classifier {
            config: ModalityKeywordsConfig {
                modalities: vec![],
                method_keywords: vec![],
                organism_keywords: vec![],
                data_source_patterns: vec![],
                goal_patterns: vec![GoalPattern {
                    phrases: vec!["single cell anndata".into()],
                    edam_data: "data:3917".into(),
                    edam_format: None,
                    modifiers: std::collections::BTreeMap::new(),
                }],
            },
            keyword_idf: std::collections::BTreeMap::new(),
        };
        // Hyphenated SME prose should still match.
        assert!(cls.extract_goal("Need a single-cell AnnData").is_some());
        // Underscored SME prose should still match.
        assert!(cls.extract_goal("output: single_cell_anndata").is_some());
    }

    /// `ClassificationResult.goal` lands when the
    /// real `modality-keywords.yaml` has the `goal_patterns:`
    /// block. Validates the wire shape end-to-end.
    #[test]
    fn classify_threads_goal_when_yaml_pattern_matches() {
        // The committed `modality-keywords.yaml` may or may not
        // ship goal_patterns yet; this test asserts the threading
        // is non-fatal in either case (None when block is absent,
        // populated when it matches).
        let cls = load_classifier();
        let r = cls.classify("Differential expression analysis on bulk RNA-seq from human samples");
        // goal MAY be None (block absent) or Some (block matched);
        // either is acceptable post-S6.2. The contract is that
        // `goal` field is now in the wire shape, addressable by
        // downstream code (composer / archetype matcher).
        let _ = r.goal;
    }

    /// Autism + PMS + transcriptomics + proteomics intake: primary
    /// modality is one of the two; the other is surfaced as an
    /// additional candidate.
    #[test]
    fn detects_cross_omics_intent_rnaseq_proteomics() {
        let cls = load_classifier();
        let r = cls.classify(
            "I want to analyze all publicly available data comparing the gene expression and \
             proteomics of healthy subjects vs patients with autism spectrum disorder vs patients \
             with phelan mcdermid syndrome. RNA-seq differential expression and mass spec \
             proteomics from postmortem brain tissue.",
        );
        let primary = r.modality.clone();
        let additional_ids: Vec<String> = r
            .additional_modalities
            .iter()
            .map(|c| c.modality.clone())
            .collect();
        let all_ids: std::collections::HashSet<String> = std::iter::once(primary.clone())
            .chain(additional_ids.iter().cloned())
            .collect();
        assert!(
            all_ids.contains("bulk_rnaseq"),
            "bulk_rnaseq should be either primary or in additional, got primary={} additional={:?}",
            primary,
            additional_ids
        );
        assert!(
            all_ids.contains("proteomics"),
            "proteomics should be either primary or in additional, got primary={} additional={:?}",
            primary,
            additional_ids
        );
        assert!(
            !r.additional_modalities.is_empty(),
            "additional_modalities must be non-empty when SME asks for cross-omics"
        );
        // Hit count is the real threshold gate (see
        // `cross_omics_threshold` docs). Confidence is a soft floor.
        for c in &r.additional_modalities {
            assert!(
                c.keyword_hits >= 2,
                "candidate hit count below threshold: {:?}",
                c
            );
        }
    }

    /// Regression for the web UI session where explicit
    /// transcriptomics + proteomics was classified as proteomics-only
    /// for DAG construction. The key failure was goal extraction:
    /// "differential gene expression" did not hit the DE goal and
    /// "protein abundance" later won as a proteomics-only goal.
    #[test]
    fn live_multi_omics_session_goal_routes_to_cross_omics_de_shape() {
        let cls = load_classifier();
        let r = cls.classify(
            "Cross-omics analysis of postmortem human brain tissue comparing healthy controls, \
             autism spectrum disorder patients, and Phelan-McDermid syndrome patients. Goals: \
             differential gene expression in bulk RNA-seq transcriptomics and differential \
             protein abundance in LC-MS/MS proteomics. Include formal convergence testing and \
             overlap/Venn shared signal analysis.",
        );

        let primary = r.modality.as_str();
        let all_ids: std::collections::HashSet<&str> = std::iter::once(primary)
            .chain(r.additional_modalities.iter().map(|m| m.modality.as_str()))
            .collect();
        assert!(
            all_ids.contains("bulk_rnaseq") && all_ids.contains("proteomics"),
            "expected both bulk_rnaseq and proteomics, got primary={} additional={:?}",
            r.modality,
            r.additional_modalities
        );
        let goal = r
            .goal
            .as_ref()
            .expect("cross-omics DE-shaped goal must be extracted");
        assert_eq!(goal.edam_data, "data:0951");
        assert_eq!(goal.edam_format.as_deref(), Some("format:3475"));
        assert_eq!(
            goal.modifiers.get("kind").map(String::as_str),
            Some("differential_expression")
        );
    }

    #[test]
    fn user_gene_expression_proteomics_text_is_cross_omics() {
        let cls = load_classifier();
        // Prose carries an explicit "cross-omics" marker so the
        // strong-marker gate in `collect_cross_omics_candidates` lowers
        // `min_hits` to 1; sparse keyword counts for bulk_rnaseq/proteomics
        // would otherwise fall below the base threshold.
        let r = cls.classify(
            "i need to perform a cross-omics gene expression and proteomics analysis comparing \
             healthy subjects vs patients with autism spectrum disorder vs patients with phelan \
             mcdermid syndrome. we must perform a sweep of all publicly available repositories \
             to identify all available postmortem brain tissue from any region and during the \
             analysis the regions should be analyzed and compared within region. any data where \
             we do not have enough data available to meet statistical power requirements we \
             should drop the group. i need formal convergence between ASD/PMS as well as \
             general overlap and i also need a comparison of what is different between \
             healthy/asd/pms",
        );

        let all_ids: std::collections::HashSet<&str> = std::iter::once(r.modality.as_str())
            .chain(r.additional_modalities.iter().map(|m| m.modality.as_str()))
            .collect();
        assert!(
            all_ids.contains("bulk_rnaseq") && all_ids.contains("proteomics"),
            "gene-expression + proteomics prose must classify as cross-omics, got primary={} additional={:?}",
            r.modality,
            r.additional_modalities
        );
        let goal = r.goal.as_ref().expect("DE-shaped goal must be extracted");
        assert_eq!(
            goal.modifiers.get("kind").map(String::as_str),
            Some("differential_expression")
        );
    }

    #[test]
    fn later_scope_exclusions_remove_stale_modalities_from_accumulated_intake() {
        let cls = load_classifier();
        let r = cls.classify(
            "Cross-omics analysis: transcriptomics and proteomics, with bulk RNA-seq and \
             single-nucleus/single-cell RNA-seq where available. Run transcriptomics first, \
             proteomics as a separate follow-on package. Bulk RNA-seq transcriptomics \
             analysis only (no proteomics, no single-cell/single-nucleus this session). \
             Differential expression comparing healthy controls vs ASD vs PMS.",
        );

        assert_eq!(r.modality, "bulk_rnaseq");
        assert!(
            r.additional_modalities.is_empty(),
            "explicit no-proteomics/no-single-cell correction must remove stale modalities, got {:?}",
            r.additional_modalities
        );
        let goal = r.goal.as_ref().expect("DE goal should still be detected");
        assert_eq!(
            goal.modifiers.get("kind").map(String::as_str),
            Some("differential_expression")
        );
    }

    /// Single-modality request must NOT surface cross-omics
    /// companions even when the prose contains
    /// "and" near method names from another modality's keyword list
    /// (defensive against the false-positive case described in the
    /// `is_cross_omics_intent` predicate's docstring).
    #[test]
    fn single_modality_request_no_additional() {
        let cls = load_classifier();
        let r = cls.classify(
            "Bulk RNA-seq differential expression with DESeq2 and edgeR on human samples \
             from GSE123456",
        );
        assert_eq!(r.modality, "bulk_rnaseq");
        assert!(
            r.additional_modalities.is_empty(),
            "single-modality intake must NOT trigger cross-omics surfacing, got {:?}",
            r.additional_modalities
        );
    }

    /// Pure proteomics intake, no transcriptomics in sight. Must not
    /// silently fan to RNA-seq.
    #[test]
    fn pure_proteomics_no_rnaseq_companion() {
        let cls = load_classifier();
        let r = cls.classify(
            "DDA mass spec proteomics with MaxQuant and FragPipe, TMT labeling, \
             differential abundance analysis between healthy and disease cohorts",
        );
        assert_eq!(r.modality, "proteomics");
        assert!(
            r.additional_modalities.is_empty(),
            "pure proteomics intake must NOT add bulk_rnaseq companion, got {:?}",
            r.additional_modalities
        );
    }

    /// The predicate gate matters: two modality keyword hits without
    /// an explicit cross-omics
    /// conjunction phrase must NOT trigger surfacing. This guards
    /// the case where SME prose happens to use a stray keyword from
    /// another modality (e.g. mentioning "kallisto" while talking
    /// about proteomics) without intending cross-omics.
    #[test]
    fn conjunction_gate_blocks_incidental_keyword_spillover() {
        // Modality keyword hits are ambiguous in the intake, but
        // there's no conjunction phrase like "rnaseq AND proteomics".
        // The classifier must not unilaterally fan out the DAG.
        let cls = load_classifier();
        let r = cls.classify(
            "Differential expression analysis. Use kallisto for quantification. \
             Mass spec is mentioned only in passing as future work.",
        );
        // Whatever the primary winner is, additional_modalities
        // must stay empty because no conjunction phrase appeared.
        assert!(
            r.additional_modalities.is_empty(),
            "incidental keyword spillover without conjunction must NOT trigger cross-omics, \
             got primary={} additional={:?}",
            r.modality,
            r.additional_modalities
        );
    }

    /// Back-compat: a `ClassificationResult` JSON persisted without
    /// the `additional_modalities` field must deserialize cleanly
    /// with the field defaulting to an empty Vec.
    #[test]
    fn back_compat_deserialize_pre_amendment_session_json() {
        let pre_amendment_json = serde_json::json!({
            "modality": "bulk_rnaseq",
            "taxonomy_path": "config/stage-taxonomies/rnaseq-de.yaml",
            "domain": "",
            "workflow_description": "",
            "edam_topic": "topic:3308",
            "edam_operation": "operation:3223",
            "confidence": 0.5,
            "confidence_label": "medium",
            "organisms": [],
            "methods_specified": [],
            "data_sources": [],
            "intake_text": "stale session"
        });
        let r: ClassificationResult = serde_json::from_value(pre_amendment_json)
            .expect("pre-amendment session JSON must deserialize");
        assert_eq!(r.modality, "bulk_rnaseq");
        assert!(r.additional_modalities.is_empty());
        assert!(r.goal.is_none());
        assert!(r.archetype_id.is_none());
    }

    /// When only one modality clears threshold, no additional
    /// candidates surface even if the prose contains conjunction
    /// phrases.
    #[test]
    fn conjunction_alone_without_threshold_clearing_is_noop() {
        let cls = load_classifier();
        // "Transcriptomics and proteomics" conjunction is present, but
        // the intake doesn't carry enough of either modality's
        // keywords to clear the threshold (≥2 hits, ≥0.30 conf).
        let r = cls.classify("Quick exploratory analysis of transcriptomics and proteomics ideas.");
        // Either modality won; companions should be empty since
        // neither cleared the threshold gate.
        assert!(
            r.additional_modalities.is_empty(),
            "no candidate should clear threshold from sparse prose, got {:?}",
            r.additional_modalities
        );
    }

    /// Explicit synonym-level conjunction between two canonical
    /// modalities must lift the SUPPRESSED_PAIRS heuristic even when
    /// the SME doesn't write the academic terms "cross omics" /
    /// "multi omics" / "multiomics" anywhere in the prompt — otherwise
    /// a methylation-primary prose with a bulk-RNA-seq companion (a
    /// SUPPRESSED_PAIRS entry due to Bismark/BWA tooling overlap) would
    /// collapse to a single-modality DAG even when the matching
    /// cross_omics_rnaseq_methylation archetype is registered.
    ///
    /// Helper directly: `pair_explicitly_conjoined` returns true on
    /// synonym-pair phrasings like
    /// `"rna seq and whole-genome bisulfite sequencing"` (synonym +
    /// conjunction + modifier-laden second synonym).
    #[test]
    fn pair_explicitly_conjoined_matches_synonym_phrasing() {
        let text = normalize_for_match(
            "Matched bulk RNA-seq and whole-genome bisulfite sequencing \
             for the same cohort.",
        );
        assert!(
            super::pair_explicitly_conjoined(&text, "methylation", "bulk_rnaseq"),
            "rna seq + bisulfite conjunction should lift the suppression \
             (methylation primary, bulk_rnaseq companion)",
        );
        // Reverse direction: same phrase, suppressed pair reversed.
        assert!(
            super::pair_explicitly_conjoined(&text, "bulk_rnaseq", "methylation"),
            "lift is symmetric — bulk_rnaseq primary with methylation \
             companion gets the same explicit conjunction signal",
        );
    }

    /// Companion regression: an explicit synonym conjunction between
    /// primary and companion lifts SUPPRESSED_PAIRS even when no
    /// marker word ("cross omics", "multi omics", "diablo", etc.)
    /// appears anywhere in the prompt. Without this lift the
    /// rnaseq-methyl-* blinded scenarios collapse to single-modality
    /// methylation packages.
    #[test]
    fn rnaseq_methylation_conjunction_surfaces_companion_without_marker_word() {
        let cls = load_classifier();
        let prompt = "Matched bulk RNA-seq and whole-genome bisulfite sequencing for the same \
                      cohort. Two groups of donors. Each donor has paired RNA-seq + WGBS \
                      libraries. We want per-condition gene-level differential expression \
                      between the two groups from the RNA-seq, per-CpG methylation values \
                      and per-region differentially methylated regions from the WGBS, then \
                      a cross-modality scatter of expression-log2FC vs methylation-delta \
                      coloured by significance in both modalities.";
        let r = cls.classify(prompt);
        let canonical_ids: Vec<&str> = std::iter::once(r.modality.as_str())
            .chain(r.additional_modalities.iter().map(|c| c.modality.as_str()))
            .collect();
        assert!(
            canonical_ids.contains(&"methylation"),
            "methylation must be classified (primary or companion); got {canonical_ids:?}",
        );
        assert!(
            canonical_ids.contains(&"bulk_rnaseq"),
            "bulk_rnaseq must be classified (primary or companion) so the \
             cross_omics_rnaseq_methylation archetype dispatches; got {canonical_ids:?}",
        );
    }

    /// RCA F3 regression: multiple synonyms of the *same* canonical
    /// modality distributed across comma segments must NOT inflate the
    /// n-way count. Counting surface tokens (the old behavior) made
    /// "RRBS bisulfite methylation methylome" look like 4 modalities.
    /// Counting canonical IDs collapses them to 1 (methylation).
    #[test]
    fn is_n_way_intent_canonical_methylation_synonyms_collapse_to_one() {
        let n = normalize_for_match("RRBS bisulfite methylation methylome");
        assert_eq!(
            count_distinct_canonical_modalities_in_comma_list(&n, SYNONYM_TO_MODALITY),
            1,
            "all four tokens map to the methylation modality"
        );
        let n = normalize_for_match("methylation, methylome, dna methylation");
        assert_eq!(
            count_distinct_canonical_modalities_in_comma_list(&n, SYNONYM_TO_MODALITY),
            1,
            "three comma segments, all methylation synonyms → 1 canonical id"
        );
    }

    /// Three genuinely distinct modalities (WGS = variant_calling,
    /// RNA-seq = bulk_rnaseq, ChIP-seq = chip_seq) must count as 3.
    #[test]
    fn is_n_way_intent_canonical_three_distinct_modalities_count_three() {
        let n = normalize_for_match("WGS, RNA-seq, and ChIP-seq");
        assert_eq!(
            count_distinct_canonical_modalities_in_comma_list(&n, SYNONYM_TO_MODALITY),
            3,
        );
    }

    /// scRNA-seq and snATAC-seq are different canonical modalities
    /// (single_cell_rnaseq + scatac_seq) so they count as 2 — and the
    /// longer-synonym ordering must make sure "scrna" wins over the
    /// bulk-RNA synonyms in its segment.
    #[test]
    fn is_n_way_intent_canonical_scrna_snatac_count_two() {
        let n = normalize_for_match("scRNA-seq paired with snATAC-seq");
        assert_eq!(
            count_distinct_canonical_modalities_in_comma_list(&n, SYNONYM_TO_MODALITY),
            2,
        );
    }

    /// End-to-end: prose distributing 3 methylation synonyms across
    /// comma segments must NOT trip the public is_n_way_intent gate.
    #[test]
    fn is_n_way_intent_does_not_false_fire_on_methylation_synonyms() {
        assert!(
            !is_n_way_intent("RRBS, bisulfite, and methylation in mouse liver"),
            "three methylation synonyms must collapse to 1 modality (RCA F3)"
        );
    }

    /// D6 regression — the Roadmap Epigenome blinded paper-recreation
    /// prompt names three distinct modalities ("matched bulk RNA-seq,
    /// ATAC-seq ..., and ChIP-seq"), but ChIP-seq contributes only a
    /// single keyword hit ("chip seq") in the prose. Under the 2-hit
    /// floor for bare 2-way conjunction intent, chip_seq was dropped
    /// and the composer routed to the bulk_rnaseq archetype which
    /// can't reach the data:1255 / format:3003 peak-calling goal,
    /// surfacing `GoalUnreachable`. The fix relaxes min_hits to 1
    /// when `is_n_way_intent` fires (≥3 canonical modalities in the
    /// comma list), so all three modalities surface for the
    /// cross_omics_rnaseq_atac_chip archetype matcher.
    #[test]
    fn tri_omics_roadmap_epigenome_surfaces_atac_and_chip() {
        let cls = load_classifier();
        let prompt = "I have matched bulk RNA-seq, ATAC-seq (or DNase-seq — they're \
                      equivalent for this analysis), and ChIP-seq for an active \
                      histone mark across roughly thirty primary human tissues / cell \
                      populations. Per-sample FASTQs and per-modality processing. We \
                      want each modality run independently first — expression \
                      quantification with a DE contrast between two named tissue \
                      groups, ATAC peak calling and a union peak set, ChIP peak \
                      calling on the active mark — and then a cross-modality \
                      concordance report that highlights regions where the three \
                      modalities agree on a tissue-specific signal. Sanity-check that \
                      the sample IDs intersect across the three input tables.";
        let result = cls.classify(prompt);
        let modalities: std::collections::HashSet<&str> = std::iter::once(result.modality.as_str())
            .chain(
                result
                    .additional_modalities
                    .iter()
                    .map(|m| m.modality.as_str()),
            )
            .collect();
        assert!(
            modalities.contains("bulk_rnaseq"),
            "bulk_rnaseq must appear (primary or additional), got {:?}",
            modalities
        );
        assert!(
            modalities.contains("atac_seq"),
            "atac_seq must surface for tri-omics intent, got {:?}",
            modalities
        );
        assert!(
            modalities.contains("chip_seq"),
            "chip_seq must surface for tri-omics intent (D6 regression) — only 1 \
             keyword hit ('chip seq'), but n_way_intent unlocks min_hits=1; got {:?}",
            modalities
        );
    }
}
