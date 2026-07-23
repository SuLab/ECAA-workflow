//! Deterministic cross-method robustness aggregator for the multi-analyst
//! ensemble, hosting BOTH ensemble aggregators: the cross-method
//! statistical-distribution rollup and the cross-axis (method × lens)
//! ensemble-distribution rollup. Given the K statistical-method variants
//! of one terminal analytical stage (e.g.
//! `differential_expression__v_deseq2` / `..edger` / `..limma`), the
//! statistical aggregator reads each variant's declared result artifact,
//! resolves the per-entity effect + significance BY THE SHARED
//! [`ResultSchema`] (never positionally), and classifies every entity's
//! cross-method robustness into one of four buckets. It also emits a
//! pooled consensus single-artifact view (`report-data.json`) that is
//! SHAPE-COMPATIBLE with the normal (non-ensemble) `report-data.json` —
//! it deserializes to the same [`ReportData`] type. Consumption of that
//! pooled artifact by the reporting-invariants recompute machinery in
//! ensemble mode is DEFERRED to a follow-up: today
//! `reporting_invariants::read_report_data` reads only
//! `outputs/reporting/report-data.json`, which the ensemble path never
//! writes (`check_rc_count` re-derives `outputs/<stage_id>/<schema.artifact>`
//! directly instead), so the pooled artifact written here and under
//! `assemble_ensemble_distribution/` is inert for that validator until the
//! wiring lands.
//!
//! Pure over its on-disk inputs and never touches the wall clock (threads
//! [`Clock`] per the emit-path determinism contract, though it is
//! reserved/unused today); iterates inputs in sorted / `BTreeMap` order so
//! the emitted JSON is byte-reproducible. Outputs live under
//! `runtime/outputs/assemble_statistical_distribution/` and
//! `runtime/outputs/assemble_ensemble_distribution/` respectively, and are
//! runtime products (excluded from the emitted-package byte-repro
//! baseline).
//!
//! Lives inside the `report_contract` module so it can reach the
//! `pub(crate)` [`read_table`](super::assemble::read_table) parser and the
//! `pub(crate)` [`build_entity_rows`](super::report_data::build_entity_rows)
//! per-entity resolver without duplicating either.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::claim_extractor::ExtractorConfig;
use crate::claim_verifier::ClaimVerificationReport;
use crate::clock::Clock;
use crate::reexecution_bounds::ModalityBounds;

use super::assemble::read_table;
use super::report_data::build_entity_rows;
use super::{
    DirectionSplit, DistBin, EntityRow, LitFinding, LiteratureStatus, ReportData,
    ResultArtifactSummary, ResultSchema, load_policy_column_synonyms, should_spill,
    summarize_artifact, write_supplementary,
};

/// The stage_id (and output-dir name) of the statistical-distribution
/// aggregator. Shared with the composer/harness as the single source of
/// truth for where this aggregator's products live.
pub const STAT_DISTRIBUTION_STAGE_ID: &str = "assemble_statistical_distribution";

/// Cross-method robustness class for a single entity, computed over the K
/// statistical-method variants that reported it.
///
/// `#[non_exhaustive]`: wire-facing enum crossing the
/// `stat-distribution.json` boundary; new classes may be added without a
/// SemVer-breaking match on downstream consumers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum RobustnessClass {
    /// Significant AND same-signed in EVERY present method (≥2 methods) —
    /// the strongest cross-method corroboration.
    Robust,
    /// Directionally consistent (same sign, no flip) and all pairwise
    /// effects agree within the modality re-execution tolerance, but the
    /// finding does not reach significance in any method — corroborated in
    /// direction/magnitude, sub-significant.
    Concordant,
    /// Either significant in a strict subset of present methods (no sign
    /// flip), OR same-signed but the effects disperse beyond the modality
    /// tolerance while reaching significance in none — a fragile signal.
    Fragile,
    /// A sign flip across present methods (one method up, another down) —
    /// the methods disagree on direction.
    Discordant,
}

/// Per-entity cross-method rollup. `per_method_*` maps are keyed by the
/// short method name (the tail of the variant stage-id after `__v_`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntityMethodRow {
    pub entity: String,
    /// Signed effect per method (only methods whose artifact carried a
    /// finite effect value for this entity).
    pub per_method_effect: BTreeMap<String, f64>,
    /// Significance verdict per present method.
    pub per_method_significant: BTreeMap<String, bool>,
    /// Fraction of the effect-bearing present methods sharing the majority
    /// sign (1.0 when ≤1 method reported a finite effect).
    pub sign_agreement: f64,
    /// `max − min` of the per-method effects (0.0 when fewer than two
    /// finite effects were reported).
    pub effect_range: f64,
    pub n_methods_significant: u32,
    pub robustness: RobustnessClass,
    /// Median of the per-method finite effects (`None` when none reported).
    pub pooled_effect_median: Option<f64>,
}

/// Top-level `stat-distribution.json` payload: the method roster, the
/// per-entity robustness rollup, the class-count histogram, and a pooled
/// consensus single-artifact view shaped exactly like a `ReportData`
/// artifact.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StatDistribution {
    /// Short method names of the variants whose artifact was actually
    /// present on disk (sorted; absent variants are skipped).
    pub methods: Vec<String>,
    /// One row per entity present in any present method, sorted by entity.
    pub entities: Vec<EntityMethodRow>,
    pub n_robust: u64,
    pub n_concordant: u64,
    pub n_fragile: u64,
    pub n_discordant: u64,
    /// Consensus single-result view (per-entity median effect; significant
    /// set = entities significant in a strict majority of present methods).
    pub pooled: ResultArtifactSummary,
}

/// One method's reading of one entity.
#[derive(Debug, Clone)]
struct MethodDatum {
    effect: Option<f64>,
    significance: Option<f64>,
    significant: bool,
}

/// Derives the short method name from a variant stage-id: the tail after
/// the last `__v_` (e.g. `differential_expression__v_edger` → `edger`).
/// Falls back to the whole id when the marker is absent.
fn method_name(variant_stage_id: &str) -> String {
    variant_stage_id
        .rsplit_once("__v_")
        .map(|(_, tail)| tail)
        .unwrap_or(variant_stage_id)
        .to_string()
}

/// Median of a finite-value slice (`None` when empty). Even-length median
/// is the mean of the two central values.
fn median(vals: &[f64]) -> Option<f64> {
    if vals.is_empty() {
        return None;
    }
    let mut v = vals.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = v.len();
    Some(if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    })
}

/// Bins `|effect|` over `[0,0.5) [0.5,1) [1,2) [2,inf)` — mirrors the
/// report-data assembler's magnitude bins so the pooled distribution reads
/// identically to a single-artifact one.
fn magnitude_bin(effect: f64) -> usize {
    let a = effect.abs();
    if a < 0.5 {
        0
    } else if a < 1.0 {
        1
    } else if a < 2.0 {
        2
    } else {
        3
    }
}

const DIST_BIN_LABELS: [&str; 4] = ["0-0.5", "0.5-1", "1-2", "2+"];

/// Assembles the cross-method robustness distribution over
/// `variant_stage_ids` and writes both `stat-distribution.json` and a
/// pooled `report-data.json` under
/// `package_root/runtime/outputs/assemble_statistical_distribution/`.
///
/// A variant whose declared artifact is absent on disk is SKIPPED (not an
/// error), mirroring [`assemble_report_data`](super::assemble::assemble_report_data).
///
/// `clock` is threaded per the architecture rule that no emit-path
/// function reads the wall clock directly; the payload carries no
/// timestamp today, so it is otherwise unused.
///
/// ## Robustness precedence (first match wins)
/// Total classification, ordered **Discordant > Robust > Fragile > Concordant**:
/// 1. **Discordant** — a sign flip across present methods (∃ effect > 0 and
///    ∃ effect < 0). Directional disagreement is the dominant signal.
/// 2. **Robust** — ≥2 present methods AND significant in every one (same
///    sign is implied by the absence of a flip).
/// 3. **Fragile** — significant in a strict subset of present methods.
/// 4. else (no method significant, no sign flip):
///    - **Concordant** when all pairwise effects agree within `bounds`
///      (`ModalityBounds::within`) — directionally + quantitatively
///      consistent, sub-significant;
///    - **Fragile** otherwise — same-signed but dispersed beyond tolerance.
pub fn assemble_statistical_distribution(
    package_root: &Path,
    variant_stage_ids: &[String],
    schema: &ResultSchema,
    bounds: &ModalityBounds,
    clock: &dyn Clock,
) -> Result<StatDistribution> {
    let _ = clock;

    // Data-driven tolerant column resolution, exactly as the report-data
    // assembler does — the shared synonym source, never hardcoded names.
    let synonyms = load_policy_column_synonyms(package_root);
    let outputs_dir = package_root.join("runtime").join("outputs");

    // Deterministic input order.
    let mut vids: Vec<String> = variant_stage_ids.to_vec();
    vids.sort();
    vids.dedup();

    // entity -> (method -> datum). BTreeMaps keep both the entity roster and
    // the per-method maps sorted for byte-reproducible output.
    let mut by_entity: BTreeMap<String, BTreeMap<String, MethodDatum>> = BTreeMap::new();
    let mut methods: Vec<String> = Vec::new();

    for vid in &vids {
        let artifact_path = outputs_dir.join(vid).join(&schema.artifact);
        if !artifact_path.exists() {
            continue;
        }
        let (headers, rows) = read_table(&artifact_path)?;
        let stats = summarize_artifact(&rows, &headers, schema, &synonyms);
        let method = method_name(vid);
        if !methods.contains(&method) {
            methods.push(method.clone());
        }

        // Significant entity names for this method (empty when the
        // significance column was declared-but-unresolvable — never treated
        // as "all significant").
        let sig_names: BTreeSet<String> =
            build_entity_rows(&rows, &headers, schema, &synonyms, &stats.significant_row_indices)
                .into_iter()
                .map(|e| e.entity)
                .collect();

        // Effect + significance value per entity over every row.
        let all_indices: Vec<usize> = (0..rows.len()).collect();
        for er in build_entity_rows(&rows, &headers, schema, &synonyms, &all_indices) {
            let significant = sig_names.contains(&er.entity);
            by_entity
                .entry(er.entity.clone())
                .or_default()
                .entry(method.clone())
                .or_insert(MethodDatum {
                    effect: er.effect,
                    significance: er.significance,
                    significant,
                });
        }
    }
    methods.sort();

    let mut entities: Vec<EntityMethodRow> = Vec::new();
    let (mut n_robust, mut n_concordant, mut n_fragile, mut n_discordant) = (0u64, 0u64, 0u64, 0u64);

    // Pooled consensus rows: (entity, median_effect, median_significance,
    // majority_significant). Built in the same sorted pass.
    let mut pooled_rows: Vec<(String, Option<f64>, Option<f64>, bool)> = Vec::new();

    for (entity, method_map) in &by_entity {
        let n_present = method_map.len();

        let mut per_method_effect: BTreeMap<String, f64> = BTreeMap::new();
        let mut per_method_significant: BTreeMap<String, bool> = BTreeMap::new();
        let mut effects: Vec<f64> = Vec::new();
        let mut sig_values: Vec<f64> = Vec::new();
        let mut n_sig = 0u32;
        for (m, d) in method_map {
            per_method_significant.insert(m.clone(), d.significant);
            if d.significant {
                n_sig += 1;
            }
            if let Some(e) = d.effect {
                per_method_effect.insert(m.clone(), e);
                effects.push(e);
            }
            if let Some(s) = d.significance {
                sig_values.push(s);
            }
        }

        let has_pos = effects.iter().any(|&e| e > 0.0);
        let has_neg = effects.iter().any(|&e| e < 0.0);
        let sign_flip = has_pos && has_neg;

        let sign_agreement = if effects.is_empty() {
            1.0
        } else {
            let pos = effects.iter().filter(|&&e| e > 0.0).count();
            let neg = effects.iter().filter(|&&e| e < 0.0).count();
            let zero = effects.iter().filter(|&&e| e == 0.0).count();
            pos.max(neg).max(zero) as f64 / effects.len() as f64
        };

        let effect_range = if effects.len() < 2 {
            0.0
        } else {
            let max = effects.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let min = effects.iter().cloned().fold(f64::INFINITY, f64::min);
            max - min
        };

        let all_significant = n_present >= 2 && (n_sig as usize) == n_present;
        let any_significant = n_sig >= 1;
        // All pairwise effects agree within the modality tolerance
        // (vacuously true with fewer than two finite effects).
        let within_tol = effects.iter().enumerate().all(|(i, &a)| {
            effects.iter().skip(i + 1).all(|&b| bounds.within(a, b))
        });

        // Precedence: Discordant > Robust > Fragile > Concordant (see doc).
        let robustness = if sign_flip {
            RobustnessClass::Discordant
        } else if all_significant {
            RobustnessClass::Robust
        } else if any_significant {
            RobustnessClass::Fragile
        } else if within_tol {
            RobustnessClass::Concordant
        } else {
            RobustnessClass::Fragile
        };
        match robustness {
            RobustnessClass::Robust => n_robust += 1,
            RobustnessClass::Concordant => n_concordant += 1,
            RobustnessClass::Fragile => n_fragile += 1,
            RobustnessClass::Discordant => n_discordant += 1,
        }

        let pooled_effect_median = median(&effects);
        // Strict-majority significance across present methods.
        let majority_significant = (n_sig as usize) * 2 > n_present;
        pooled_rows.push((
            entity.clone(),
            pooled_effect_median,
            median(&sig_values),
            majority_significant,
        ));

        entities.push(EntityMethodRow {
            entity: entity.clone(),
            per_method_effect,
            per_method_significant,
            sign_agreement,
            effect_range,
            n_methods_significant: n_sig,
            robustness,
            pooled_effect_median,
        });
    }

    let pooled = build_pooled_summary(&outputs_dir, schema, &pooled_rows)?;

    let dist = StatDistribution {
        methods,
        entities,
        n_robust,
        n_concordant,
        n_fragile,
        n_discordant,
        pooled: pooled.clone(),
    };

    let agg_dir = outputs_dir.join(STAT_DISTRIBUTION_STAGE_ID);
    std::fs::create_dir_all(&agg_dir)
        .with_context(|| format!("creating {}", agg_dir.display()))?;

    let dist_json =
        serde_json::to_string_pretty(&dist).context("serializing stat-distribution.json")?;
    let dist_path = agg_dir.join("stat-distribution.json");
    std::fs::write(&dist_path, dist_json)
        .with_context(|| format!("writing {}", dist_path.display()))?;

    // Pooled report-data.json so the existing reporting-invariants recompute
    // still has a single-artifact ReportData to read for the ensemble.
    let report = ReportData {
        artifacts: vec![pooled],
        literature: None,
    };
    let report_json =
        serde_json::to_string_pretty(&report).context("serializing pooled report-data.json")?;
    let report_path = agg_dir.join("report-data.json");
    std::fs::write(&report_path, report_json)
        .with_context(|| format!("writing {}", report_path.display()))?;

    Ok(dist)
}

/// Builds the pooled consensus [`ResultArtifactSummary`] — the per-entity
/// median-effect single-result view whose significant set is the entities
/// significant in a strict majority of methods. Writes matching pooled
/// supplementary tables so the `significant_table_path`/`full_table_path`
/// point at real files (consistent with the report-data assembler).
fn build_pooled_summary(
    outputs_dir: &Path,
    schema: &ResultSchema,
    pooled_rows: &[(String, Option<f64>, Option<f64>, bool)],
) -> Result<ResultArtifactSummary> {
    let agg_dir = outputs_dir.join(STAT_DISTRIBUTION_STAGE_ID);

    let effect_col = schema.signed_effect_column.as_deref().unwrap_or("effect");
    let sig_col = schema
        .significance
        .as_ref()
        .map(|s| s.column.as_str())
        .unwrap_or("significance");

    // Synthesize a pooled table (header + one row per entity, all methods
    // collapsed to their median) so the supplementary attachments are a
    // faithful subset/superset of the pooled view.
    let mut headers = csv::StringRecord::new();
    headers.push_field(&schema.entity_column);
    headers.push_field(effect_col);
    headers.push_field(sig_col);

    let fmt = |v: Option<f64>| v.map(|x| x.to_string()).unwrap_or_default();
    let mut rows: Vec<csv::StringRecord> = Vec::new();
    let mut sig_indices: Vec<usize> = Vec::new();
    let mut significant_entities: Vec<EntityRow> = Vec::new();
    let mut up = 0u64;
    let mut down = 0u64;
    let mut bins = [0u64; 4];
    for (i, (entity, eff, sig, majority)) in pooled_rows.iter().enumerate() {
        let mut rec = csv::StringRecord::new();
        rec.push_field(entity);
        rec.push_field(&fmt(*eff));
        rec.push_field(&fmt(*sig));
        rows.push(rec);
        if *majority {
            sig_indices.push(i);
            significant_entities.push(EntityRow {
                entity: entity.clone(),
                effect: *eff,
                significance: *sig,
                literature: LiteratureStatus::NotAssessed,
            });
            if let Some(e) = eff {
                if *e > 0.0 {
                    up += 1;
                } else if *e < 0.0 {
                    down += 1;
                }
                bins[magnitude_bin(*e)] += 1;
            }
        }
    }

    let stem = Path::new(&schema.artifact)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(schema.artifact.as_str());
    let (sig_rel, full_rel) = write_supplementary(&agg_dir, stem, &headers, &rows, &sig_indices)
        .context("writing pooled supplementary tables")?;

    let n_significant = sig_indices.len() as u64;
    let has_effect = schema.signed_effect_column.is_some();
    let direction_split = has_effect.then_some(DirectionSplit { up, down });
    let effect_distribution = has_effect.then(|| {
        DIST_BIN_LABELS
            .iter()
            .zip(bins)
            .map(|(label, count)| DistBin {
                label: (*label).to_string(),
                count,
            })
            .collect::<Vec<_>>()
    });

    let spilled = should_spill(significant_entities.len());
    Ok(ResultArtifactSummary {
        stage_id: STAT_DISTRIBUTION_STAGE_ID.to_string(),
        artifact: schema.artifact.clone(),
        n_total: pooled_rows.len() as u64,
        n_significant: Some(n_significant),
        direction_split,
        effect_distribution,
        grouped_significant: None,
        significant_entities: if spilled { Vec::new() } else { significant_entities },
        significant_table_path: format!(
            "runtime/outputs/{STAT_DISTRIBUTION_STAGE_ID}/{sig_rel}"
        ),
        full_table_path: format!("runtime/outputs/{STAT_DISTRIBUTION_STAGE_ID}/{full_rel}"),
        spilled_to_attachment_only: spilled,
    })
}

// ---------------------------------------------------------------------------
// Cross-axis (method × interpretive-lens) ensemble distribution.
// ---------------------------------------------------------------------------

/// The stage_id (and output-dir name) of the cross-axis ensemble
/// aggregator. Shared with the composer/harness as the single source of
/// truth for where this aggregator's products live.
pub const ENSEMBLE_DISTRIBUTION_STAGE_ID: &str = "assemble_ensemble_distribution";

/// The result.json keys read for a cell's hypothesis-support verdict, in
/// precedence order: `hypothesis_supported` (canonical) then `support`
/// (fixture/legacy alias). The first present-and-coercible value wins.
const SUPPORT_KEYS: [&str; 2] = ["hypothesis_supported", "support"];

/// Per-cell rollup of one method × interpretive-lens interpretation cell.
///
/// One cell corresponds to `runtime/outputs/<cell_id>/result.json`, where
/// `cell_id == biological_interpretation__m_<method>__lens_<lens>`. The
/// [`verification`](Self::verification) slot carries the serialized
/// [`ClaimVerificationReport`](crate::claim_verifier::ClaimVerificationReport)
/// when the cell's narrative was cross-checked against its method-variant
/// result table (see [`assemble_ensemble_distribution`]); it is `None` when
/// there was nothing to verify (no embedded interpretation policy, no
/// narrative, or the method-variant table dir was absent).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CellRollup {
    pub cell_id: String,
    /// Statistical-method axis parsed from the cell id (empty when the id
    /// does not match the canonical `..__m_<method>__lens_<lens>` shape).
    pub method: String,
    /// Interpretive-lens axis parsed from the cell id (empty on a
    /// non-matching id).
    pub lens: String,
    /// Hypothesis-support verdict read from the cell's result.json (see
    /// [`SUPPORT_KEYS`]); `None` when absent or not coercible to a bool.
    pub support: Option<bool>,
    /// The raw parsed result.json value, retained verbatim for per-cell
    /// claim verification.
    pub claims_json: serde_json::Value,
    /// Serialized per-cell [`ClaimVerificationReport`](crate::claim_verifier::ClaimVerificationReport):
    /// `Some` when the cell's narrative was checked for consistency with its
    /// method-variant result table, `None` when nothing was verifiable. A
    /// report with `n_mismatch > 0` (`has_mismatch()`) tags the cell as
    /// pruned — it is RETAINED here with its verdict but excluded from the
    /// ensemble consensus/marginals.
    pub verification: Option<serde_json::Value>,
}

/// Factorial decomposition of the support signal across the two ensemble
/// axes plus the compounding-fragility (interaction) signal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FactorialAttribution {
    /// Per-method support rate (mean of support as 0/1 over that method's
    /// support-bearing cells). Methods with no support-bearing cell are
    /// omitted.
    pub by_method: BTreeMap<String, f64>,
    /// Per-lens support rate, computed the same way as `by_method`.
    pub by_lens: BTreeMap<String, f64>,
    /// Cell ids whose support verdict differs from BOTH its method-marginal
    /// AND its lens-marginal majority — the compounding-fragility signal
    /// (a finding that only one method×lens combination disagrees on).
    pub interaction_hotspots: Vec<String>,
}

/// Top-level `ensemble-distribution.json` payload: the per-cell rollups,
/// the ensemble-wide agreement fraction, the factorial attribution, the
/// deduplicated literature union across the cells, its coverage, and the
/// fixed consensus label.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnsembleDistribution {
    /// One rollup per cell whose result.json was present on disk (sorted by
    /// cell id; absent cells are skipped).
    pub cells: Vec<CellRollup>,
    /// Fraction of the SURVIVING (non-pruned) support-bearing cells that
    /// agree with the majority support verdict (1.0 when ≤1 such cell; 0.0
    /// when none).
    pub agreement: f64,
    /// Count of cells excluded from the consensus because their per-cell
    /// verification carried a `Mismatch` (a narrative inconsistent with its
    /// method-variant result table). Pruned cells are RETAINED in `cells`
    /// with their verdict attached; they simply do not vote.
    pub n_pruned: u64,
    pub attribution: FactorialAttribution,
    /// PMID-deduplicated union of any [`LitFinding`]s surfaced across the
    /// cells' result.jsons (sorted-cell, then in-cell order).
    pub literature_union: Vec<LitFinding>,
    /// Unique PMID count across `literature_union`.
    pub coverage: u64,
    /// Always `"model agreement across N cells — not verified truth"`, where
    /// N is the number of cells carrying a support verdict.
    pub consensus_label: String,
}

/// Builds the per-cell claim-verifier [`ExtractorConfig`] from the package's
/// OWN embedded `policies/interpretation-policy.json`, when it declares an
/// enabled `verifiableEntities` block. Returns `None` (verification skipped)
/// when no usable policy is embedded — the honest "nothing to verify"
/// degrade, mirroring `verify_task_with_context`. Reads only package-relative
/// files so the aggregator stays deterministic. `ProjectClass::default()`
/// (`Bioinformatics`) selects the base policy with no class overlay.
fn load_cell_verifier_cfg(package_root: &Path) -> Option<ExtractorConfig> {
    let policy_dir = package_root.join("policies");
    let policy_path =
        crate::claim_extractor::resolve_policy_file(&policy_dir, "interpretation-policy.json")?;
    let raw = std::fs::read_to_string(&policy_path).ok()?;
    let policy: serde_json::Value = serde_json::from_str(&raw).ok()?;
    ExtractorConfig::from_policy_for_class(
        &policy,
        &policy_dir,
        crate::project_class::ProjectClass::default(),
    )
    .ok()
}

/// Verifies one cell's narrative for CONSISTENCY-WITH-ITS-NUMBERS against its
/// METHOD-VARIANT result table. The table dir is
/// `runtime/outputs/<primary_stage_id>__v_<method>/` — the statistical
/// variant the cell reports over, NOT the cell's own dir. The cell's
/// narrative (`report`/`interpretation`/`summary` `.md`/`.txt`) lives in the
/// cell dir. Returns `None` (nothing to verify) when the primary/method is
/// unknown, the method-variant table dir is absent, or the cell wrote no
/// narrative. This checks numeric consistency, never biological truth.
fn verify_cell(
    package_root: &Path,
    outputs_dir: &Path,
    cell_id: &str,
    primary_stage_id: &str,
    method: &str,
    cfg: &ExtractorConfig,
) -> Option<ClaimVerificationReport> {
    if primary_stage_id.is_empty() || method.is_empty() {
        return None;
    }
    let table_dir = outputs_dir.join(format!("{primary_stage_id}__v_{method}"));
    if !table_dir.is_dir() {
        return None;
    }
    let narrative_path = crate::claim_verifier::find_narrative_artifact(package_root, cell_id)?;
    let narrative = std::fs::read_to_string(&narrative_path).ok()?;
    let claims = crate::claim_extractor::extract_claims(&narrative, cfg);
    Some(crate::claim_verifier::verify_claims(&claims, &table_dir, cfg))
}

/// Parses the `(method, lens)` axes from a canonical interpretation cell id
/// `biological_interpretation__m_<method>__lens_<lens>`. Returns
/// `("", "")` for a non-matching id so a malformed id still surfaces a
/// rollup rather than being silently dropped.
fn parse_cell_axes(cell_id: &str) -> (String, String) {
    cell_id
        .strip_prefix("biological_interpretation__m_")
        .and_then(|rest| rest.split_once("__lens_"))
        .map(|(m, l)| (m.to_string(), l.to_string()))
        .unwrap_or_default()
}

/// Coerces a JSON value to a boolean support verdict. Accepts native
/// booleans and a small set of case-insensitive string forms; anything
/// else (numbers, null, objects, unknown strings) yields `None`.
fn coerce_support(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Bool(b) => Some(*b),
        serde_json::Value::String(s) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "supported" | "yes" | "support" => Some(true),
            "false" | "unsupported" | "not_supported" | "refuted" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

/// Reads the support verdict from a cell result.json value using the
/// [`SUPPORT_KEYS`] precedence.
fn read_support(v: &serde_json::Value) -> Option<bool> {
    for key in SUPPORT_KEYS {
        if let Some(found) = v.get(key).and_then(coerce_support) {
            return Some(found);
        }
    }
    None
}

/// Best-effort parse of a cell's `literature` field as `Vec<LitFinding>`
/// (empty when the field is absent or does not deserialize).
fn read_literature(v: &serde_json::Value) -> Vec<LitFinding> {
    v.get("literature")
        .and_then(|lit| serde_json::from_value::<Vec<LitFinding>>(lit.clone()).ok())
        .unwrap_or_default()
}

/// Strict majority of a boolean slice: `Some(true)`/`Some(false)` when one
/// verdict strictly outnumbers the other, `None` on an empty slice or a tie
/// (a tie has no defined majority to differ from).
fn strict_majority(votes: &[bool]) -> Option<bool> {
    if votes.is_empty() {
        return None;
    }
    let t = votes.iter().filter(|&&b| b).count();
    let f = votes.len() - t;
    match t.cmp(&f) {
        Ordering::Greater => Some(true),
        Ordering::Less => Some(false),
        Ordering::Equal => None,
    }
}

/// Support rate (mean of support as 0/1) over a non-empty boolean slice.
fn support_rate(votes: &[bool]) -> f64 {
    let t = votes.iter().filter(|&&b| b).count();
    t as f64 / votes.len() as f64
}

/// Assembles the cross-axis (method × interpretive-lens) ensemble
/// distribution over `cell_ids` and writes `ensemble-distribution.json`
/// under `package_root/runtime/outputs/assemble_ensemble_distribution/`.
///
/// Each cell id is expected to be
/// `biological_interpretation__m_<method>__lens_<lens>`; its
/// `runtime/outputs/<cell_id>/result.json` is read for the hypothesis
/// support verdict (see [`SUPPORT_KEYS`]). A cell whose result.json is
/// absent is SKIPPED (not an error), mirroring
/// [`assemble_statistical_distribution`].
///
/// Deterministic over its on-disk inputs: cell ids are sorted+deduped, all
/// per-axis maps are `BTreeMap`s, and the literature union is built in
/// sorted-cell order. `clock` is threaded per the emit-path determinism
/// contract but unused (the payload carries no timestamp).
///
/// ## Per-cell verification (pruning)
/// Each cell's narrative is cross-checked against its METHOD-VARIANT result
/// table — the table lives at `runtime/outputs/<primary_stage_id>__v_<method>/`
/// (the statistical variant the cell reports over), NOT the cell's own dir.
/// The verifier checks the prose for CONSISTENCY-WITH-ITS-NUMBERS (does the
/// asserted direction/value/threshold match the table?), NOT biological
/// truth. Only a `Mismatch` prunes a cell from the consensus (mirroring
/// [`ClaimVerificationReport::has_mismatch`](crate::claim_verifier::ClaimVerificationReport::has_mismatch));
/// a `Suspicious` verdict is review-only and never prunes. Verification runs
/// only when the package embeds an enabled `verifiableEntities`
/// interpretation policy; otherwise it is SKIPPED (every cell's
/// `verification` stays `None`, nothing is pruned) — the honest
/// "nothing to verify" degrade. Pruned cells are RETAINED in `cells` (tagged
/// with their verdict); they are excluded from `agreement`, the factorial
/// marginals, the interaction hotspots, and the literature union.
pub fn assemble_ensemble_distribution(
    package_root: &Path,
    cell_ids: &[String],
    primary_stage_id: &str,
    clock: &dyn Clock,
) -> Result<EnsembleDistribution> {
    let _ = clock;

    let outputs_dir = package_root.join("runtime").join("outputs");

    let mut ids: Vec<String> = cell_ids.to_vec();
    ids.sort();
    ids.dedup();

    // Per-cell verifier config from the package's OWN embedded interpretation
    // policy. `None` = no enabled `verifiableEntities` block → per-cell
    // verification is skipped entirely (every `verification` stays None,
    // nothing pruned). The verifier checks consistency-with-numbers, NOT
    // biological truth; only a `Mismatch` prunes (see `has_mismatch`).
    let cfg = load_cell_verifier_cfg(package_root);

    // Per-cell rollups (only cells whose result.json is present on disk).
    let mut cells: Vec<CellRollup> = Vec::new();
    // Marginal support votes per axis + the ensemble-wide vote set — built
    // over the SURVIVING (non-pruned) cells only.
    let mut method_votes: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    let mut lens_votes: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    let mut all_votes: Vec<bool> = Vec::new();
    // PMID-deduplicated literature union (surviving cells only), first-seen order.
    let mut literature_union: Vec<LitFinding> = Vec::new();
    let mut seen_pmids: BTreeSet<String> = BTreeSet::new();
    // Cell ids excluded from the consensus because their narrative was
    // inconsistent with its method-variant table (`Mismatch`).
    let mut pruned_ids: BTreeSet<String> = BTreeSet::new();

    for cell_id in &ids {
        let result_path = outputs_dir.join(cell_id).join("result.json");
        if !result_path.exists() {
            continue;
        }
        let raw = std::fs::read_to_string(&result_path)
            .with_context(|| format!("reading {}", result_path.display()))?;
        let claims_json: serde_json::Value = serde_json::from_str(&raw)
            .with_context(|| format!("parsing {}", result_path.display()))?;

        let (method, lens) = parse_cell_axes(cell_id);
        let support = read_support(&claims_json);

        // Per-cell verification against the method-variant table. A cell is
        // PRUNED iff it produced a report AND that report `has_mismatch()`
        // (Mismatch only — `Suspicious` is review-only and never prunes).
        let report = cfg.as_ref().and_then(|cfg| {
            verify_cell(
                package_root,
                &outputs_dir,
                cell_id,
                primary_stage_id,
                &method,
                cfg,
            )
        });
        let pruned = report.as_ref().map(|r| r.has_mismatch()).unwrap_or(false);
        let verification = match report.as_ref() {
            Some(r) => Some(
                serde_json::to_value(r).context("serializing per-cell verification report")?,
            ),
            None => None,
        };

        // Only surviving cells vote / contribute literature; pruned cells are
        // retained in `cells` (tagged) but excluded from the consensus.
        if pruned {
            pruned_ids.insert(cell_id.clone());
        } else {
            if let Some(s) = support {
                method_votes.entry(method.clone()).or_default().push(s);
                lens_votes.entry(lens.clone()).or_default().push(s);
                all_votes.push(s);
            }
            for finding in read_literature(&claims_json) {
                if seen_pmids.insert(finding.pmid.clone()) {
                    literature_union.push(finding);
                }
            }
        }

        cells.push(CellRollup {
            cell_id: cell_id.clone(),
            method,
            lens,
            support,
            claims_json,
            verification,
        });
    }
    let n_pruned = pruned_ids.len() as u64;

    // Agreement = fraction of support-bearing cells sharing the majority
    // verdict. 1.0 when ≤1 such cell, 0.0 when none.
    let agreement = match all_votes.len() {
        0 => 0.0,
        1 => 1.0,
        n => {
            let t = all_votes.iter().filter(|&&b| b).count();
            let f = n - t;
            t.max(f) as f64 / n as f64
        }
    };

    let by_method: BTreeMap<String, f64> = method_votes
        .iter()
        .map(|(m, v)| (m.clone(), support_rate(v)))
        .collect();
    let by_lens: BTreeMap<String, f64> = lens_votes
        .iter()
        .map(|(l, v)| (l.clone(), support_rate(v)))
        .collect();

    // Interaction hotspots: cells that buck BOTH marginal majorities.
    let method_majority: BTreeMap<&String, Option<bool>> = method_votes
        .iter()
        .map(|(m, v)| (m, strict_majority(v)))
        .collect();
    let lens_majority: BTreeMap<&String, Option<bool>> = lens_votes
        .iter()
        .map(|(l, v)| (l, strict_majority(v)))
        .collect();

    let differs = |maj: Option<Option<bool>>, s: bool| matches!(maj, Some(Some(m)) if m != s);
    let mut interaction_hotspots: Vec<String> = Vec::new();
    for cell in &cells {
        // Pruned cells did not vote, so they can't be a compounding-fragility
        // hotspot against marginals they never contributed to.
        if pruned_ids.contains(&cell.cell_id) {
            continue;
        }
        if let Some(s) = cell.support {
            let mm = method_majority.get(&cell.method).copied();
            let lm = lens_majority.get(&cell.lens).copied();
            if differs(mm, s) && differs(lm, s) {
                interaction_hotspots.push(cell.cell_id.clone());
            }
        }
    }

    let n_supported = all_votes.len();
    let dist = EnsembleDistribution {
        cells,
        agreement,
        n_pruned,
        attribution: FactorialAttribution {
            by_method,
            by_lens,
            interaction_hotspots,
        },
        coverage: literature_union.len() as u64,
        literature_union,
        consensus_label: format!(
            "model agreement across {n_supported} cells — not verified truth"
        ),
    };

    let agg_dir = outputs_dir.join(ENSEMBLE_DISTRIBUTION_STAGE_ID);
    std::fs::create_dir_all(&agg_dir)
        .with_context(|| format!("creating {}", agg_dir.display()))?;
    let dist_json =
        serde_json::to_string_pretty(&dist).context("serializing ensemble-distribution.json")?;
    let dist_path = agg_dir.join("ensemble-distribution.json");
    std::fs::write(&dist_path, dist_json)
        .with_context(|| format!("writing {}", dist_path.display()))?;

    Ok(dist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FrozenClock;
    use crate::report_contract::{Comparator, Significance};

    fn de_schema() -> ResultSchema {
        ResultSchema {
            artifact: "de_results.tsv".into(),
            entity_column: "gene".into(),
            entity_column_aliases: Vec::new(),
            significance: Some(Significance {
                column: "padj".into(),
                threshold: 0.05,
                comparator: Comparator::Lt,
            }),
            signed_effect_column: Some("log2FoldChange".into()),
            signed_effect_aliases: Vec::new(),
            grouping_column: None,
        }
    }

    /// bulk-rnaseq-ish bounds: 5% relative slack is generous enough that the
    /// CONCORD fixture's near-identical effects (1.00 / 1.01 / 1.00) agree.
    fn bounds() -> ModalityBounds {
        ModalityBounds {
            relative_tolerance: 0.05,
            absolute_tolerance: 0.001,
        }
    }

    fn write_variant(root: &Path, vid: &str, body: &str) {
        let dir = root.join("runtime").join("outputs").join(vid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("de_results.tsv"), body).unwrap();
    }

    const DESEQ2: &str = "differential_expression__v_deseq2";
    const EDGER: &str = "differential_expression__v_edger";
    const LIMMA: &str = "differential_expression__v_limma";

    /// Three method tables over four genes engineered to hit each class:
    /// ROBUST (sig+same-sign×3), CONCORD (same sign, within tolerance, 0/3
    /// significant), FRAGILE (sig in 1/3, same sign), DISCORD (sign flip).
    fn write_all_three(root: &Path) {
        write_variant(
            root,
            DESEQ2,
            "gene\tlog2FoldChange\tpadj\n\
             ROBUST\t2.0\t0.001\n\
             CONCORD\t1.00\t0.5\n\
             FRAGILE\t3.0\t0.001\n\
             DISCORD\t4.0\t0.01\n",
        );
        write_variant(
            root,
            EDGER,
            "gene\tlog2FoldChange\tpadj\n\
             ROBUST\t2.1\t0.002\n\
             CONCORD\t1.01\t0.5\n\
             FRAGILE\t3.1\t0.5\n\
             DISCORD\t-4.0\t0.01\n",
        );
        write_variant(
            root,
            LIMMA,
            "gene\tlog2FoldChange\tpadj\n\
             ROBUST\t1.9\t0.001\n\
             CONCORD\t1.00\t0.5\n\
             FRAGILE\t2.9\t0.5\n\
             DISCORD\t4.1\t0.01\n",
        );
    }

    fn all_vids() -> Vec<String> {
        vec![DESEQ2.into(), EDGER.into(), LIMMA.into()]
    }

    #[test]
    fn stat_distribution_classifies_robustness() {
        let tmp = tempfile::tempdir().unwrap();
        write_all_three(tmp.path());

        let clock = FrozenClock::default();
        let dist = assemble_statistical_distribution(
            tmp.path(),
            &all_vids(),
            &de_schema(),
            &bounds(),
            &clock,
        )
        .unwrap();

        assert_eq!(dist.methods, vec!["deseq2", "edger", "limma"]);

        let by_entity: BTreeMap<_, _> = dist
            .entities
            .iter()
            .map(|e| (e.entity.clone(), e.robustness.clone()))
            .collect();
        assert_eq!(by_entity["ROBUST"], RobustnessClass::Robust);
        assert_eq!(by_entity["CONCORD"], RobustnessClass::Concordant);
        assert_eq!(by_entity["FRAGILE"], RobustnessClass::Fragile);
        assert_eq!(by_entity["DISCORD"], RobustnessClass::Discordant);

        assert_eq!(dist.n_robust, 1);
        assert_eq!(dist.n_concordant, 1);
        assert_eq!(dist.n_fragile, 1);
        assert_eq!(dist.n_discordant, 1);

        // Per-entity detail on the ROBUST row.
        let robust = dist.entities.iter().find(|e| e.entity == "ROBUST").unwrap();
        assert_eq!(robust.n_methods_significant, 3);
        assert_eq!(robust.per_method_significant.len(), 3);
        assert!((robust.sign_agreement - 1.0).abs() < 1e-9);
        assert_eq!(robust.pooled_effect_median, Some(2.0));

        // Both products written.
        let agg = tmp
            .path()
            .join("runtime")
            .join("outputs")
            .join("assemble_statistical_distribution");
        assert!(agg.join("stat-distribution.json").exists());
        assert!(agg.join("report-data.json").exists());

        // The pooled report-data.json deserializes and carries one artifact
        // whose significant set is the strict-majority-significant entities.
        // Pooled significance is independent of robustness class: ROBUST
        // (sig 3/3) AND DISCORD (also sig 3/3, but sign-flipped) both clear
        // the >50%-of-methods bar; CONCORD (0/3) and FRAGILE (1/3) do not.
        let rd: ReportData =
            serde_json::from_str(&std::fs::read_to_string(agg.join("report-data.json")).unwrap())
                .unwrap();
        assert_eq!(rd.artifacts.len(), 1);
        assert_eq!(rd.artifacts[0].n_significant, Some(2));
        assert_eq!(rd.artifacts[0], dist.pooled);
    }

    #[test]
    fn stat_distribution_skips_absent_variant() {
        let tmp = tempfile::tempdir().unwrap();
        // Only 2 of the 3 declared variants exist on disk.
        write_variant(
            tmp.path(),
            DESEQ2,
            "gene\tlog2FoldChange\tpadj\nROBUST\t2.0\t0.001\nDISCORD\t4.0\t0.01\n",
        );
        write_variant(
            tmp.path(),
            EDGER,
            "gene\tlog2FoldChange\tpadj\nROBUST\t2.1\t0.002\nDISCORD\t-4.0\t0.01\n",
        );

        let clock = FrozenClock::default();
        let dist = assemble_statistical_distribution(
            tmp.path(),
            &all_vids(),
            &de_schema(),
            &bounds(),
            &clock,
        )
        .unwrap();

        // methods reflect only the present variants.
        assert_eq!(dist.methods, vec!["deseq2", "edger"]);
        // ROBUST is significant + same sign in both present methods (≥2).
        let by_entity: BTreeMap<_, _> = dist
            .entities
            .iter()
            .map(|e| (e.entity.clone(), e.robustness.clone()))
            .collect();
        assert_eq!(by_entity["ROBUST"], RobustnessClass::Robust);
        assert_eq!(by_entity["DISCORD"], RobustnessClass::Discordant);
    }

    #[test]
    fn stat_distribution_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        write_all_three(tmp.path());
        let clock = FrozenClock::default();

        let a = assemble_statistical_distribution(
            tmp.path(),
            &all_vids(),
            &de_schema(),
            &bounds(),
            &clock,
        )
        .unwrap();
        let a_json = serde_json::to_string_pretty(&a).unwrap();
        let a_disk = std::fs::read_to_string(
            tmp.path()
                .join("runtime/outputs/assemble_statistical_distribution/stat-distribution.json"),
        )
        .unwrap();

        let b = assemble_statistical_distribution(
            tmp.path(),
            &all_vids(),
            &de_schema(),
            &bounds(),
            &clock,
        )
        .unwrap();
        let b_json = serde_json::to_string_pretty(&b).unwrap();
        let b_disk = std::fs::read_to_string(
            tmp.path()
                .join("runtime/outputs/assemble_statistical_distribution/stat-distribution.json"),
        )
        .unwrap();

        assert_eq!(a_json, b_json, "serialized result identical across runs");
        assert_eq!(a_disk, b_disk, "on-disk stat-distribution.json byte-identical");
    }

    // -- ensemble (method × lens) distribution tests ---------------------

    fn cell_id(method: &str, lens: &str) -> String {
        format!("biological_interpretation__m_{method}__lens_{lens}")
    }

    /// Writes a cell's `result.json` with a `hypothesis_supported` verdict
    /// and an optional literature array.
    fn write_cell(root: &Path, method: &str, lens: &str, support: bool, lit: serde_json::Value) {
        let id = cell_id(method, lens);
        let dir = root.join("runtime").join("outputs").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({
            "hypothesis_supported": support,
            "literature": lit,
        });
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
    }

    const METHODS: [&str; 3] = ["deseq2", "edger", "limma"];
    const LENSES: [&str; 3] = ["molecular_mechanism", "clinical_translation", "pathway_context"];

    /// A 3×3 grid where every cell supports the hypothesis EXCEPT
    /// `(deseq2, molecular_mechanism)`, which is the sole interaction
    /// hotspot: it bucks its method marginal (deseq2 = 2 true / 1 false) AND
    /// its lens marginal (molecular_mechanism = 2 true / 1 false).
    fn write_grid(root: &Path) -> Vec<String> {
        let mut ids = Vec::new();
        for m in METHODS {
            for l in LENSES {
                let support = !(m == "deseq2" && l == "molecular_mechanism");
                // molecular_mechanism cells surface a shared PMID so the
                // literature union exercises PMID dedup.
                let lit = if l == "molecular_mechanism" {
                    serde_json::json!([{
                        "entity": "CRISPLD2",
                        "pmid": "12345678",
                        "evidence_quote": "prior CRISPLD2 association",
                        "effect": 2.61
                    }])
                } else {
                    serde_json::json!([])
                };
                write_cell(root, m, l, support, lit);
                ids.push(cell_id(m, l));
            }
        }
        ids
    }

    #[test]
    fn ensemble_distribution_rolls_up_cells() {
        let tmp = tempfile::tempdir().unwrap();
        let ids = write_grid(tmp.path());
        let clock = FrozenClock::default();

        let dist = assemble_ensemble_distribution(tmp.path(), &ids, "", &clock).unwrap();

        // 9 cells, all support-bearing; 8 true / 1 false → majority true.
        assert_eq!(dist.cells.len(), 9);
        assert!((dist.agreement - 8.0 / 9.0).abs() < 1e-9);

        // Method marginals: deseq2 = 2/3; edger, limma = 1.0.
        assert!((dist.attribution.by_method["deseq2"] - 2.0 / 3.0).abs() < 1e-9);
        assert!((dist.attribution.by_method["edger"] - 1.0).abs() < 1e-9);
        assert!((dist.attribution.by_method["limma"] - 1.0).abs() < 1e-9);

        // Lens marginals: molecular_mechanism = 2/3; others = 1.0.
        assert!(
            (dist.attribution.by_lens["molecular_mechanism"] - 2.0 / 3.0).abs() < 1e-9
        );
        assert!((dist.attribution.by_lens["clinical_translation"] - 1.0).abs() < 1e-9);
        assert!((dist.attribution.by_lens["pathway_context"] - 1.0).abs() < 1e-9);

        // Exactly the compounding-fragility cell is a hotspot.
        assert_eq!(
            dist.attribution.interaction_hotspots,
            vec![cell_id("deseq2", "molecular_mechanism")]
        );

        // Literature union deduped by PMID across the 3 molecular_mechanism
        // cells that carried the same PMID.
        assert_eq!(dist.literature_union.len(), 1);
        assert_eq!(dist.literature_union[0].pmid, "12345678");
        assert_eq!(dist.coverage, 1);

        // Exact consensus label with N = support-bearing cell count.
        assert_eq!(
            dist.consensus_label,
            "model agreement across 9 cells — not verified truth"
        );

        // Product written to disk and round-trips.
        let path = tmp
            .path()
            .join("runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json");
        assert!(path.exists());
        let round: EnsembleDistribution =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(round, dist);
    }

    #[test]
    fn ensemble_distribution_skips_absent_cells() {
        let tmp = tempfile::tempdir().unwrap();
        // Only one of the two declared cells exists on disk.
        write_cell(
            tmp.path(),
            "deseq2",
            "molecular_mechanism",
            true,
            serde_json::json!([]),
        );
        let ids = vec![
            cell_id("deseq2", "molecular_mechanism"),
            cell_id("edger", "pathway_context"), // no result.json → skipped
        ];
        let clock = FrozenClock::default();

        let dist = assemble_ensemble_distribution(tmp.path(), &ids, "", &clock).unwrap();

        assert_eq!(dist.cells.len(), 1);
        assert_eq!(dist.cells[0].method, "deseq2");
        assert_eq!(dist.cells[0].lens, "molecular_mechanism");
        // Single support-bearing cell → agreement 1.0, N = 1.
        assert!((dist.agreement - 1.0).abs() < 1e-9);
        assert_eq!(
            dist.consensus_label,
            "model agreement across 1 cells — not verified truth"
        );
    }

    #[test]
    fn ensemble_distribution_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let ids = write_grid(tmp.path());
        let clock = FrozenClock::default();

        let a = assemble_ensemble_distribution(tmp.path(), &ids, "", &clock).unwrap();
        let a_json = serde_json::to_string_pretty(&a).unwrap();
        let a_disk = std::fs::read_to_string(
            tmp.path()
                .join("runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json"),
        )
        .unwrap();

        let b = assemble_ensemble_distribution(tmp.path(), &ids, "", &clock).unwrap();
        let b_json = serde_json::to_string_pretty(&b).unwrap();
        let b_disk = std::fs::read_to_string(
            tmp.path()
                .join("runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json"),
        )
        .unwrap();

        assert_eq!(a_json, b_json, "serialized result identical across runs");
        assert_eq!(
            a_disk, b_disk,
            "on-disk ensemble-distribution.json byte-identical"
        );
    }

    #[test]
    fn verification_skipped_without_policy() {
        // No embedded interpretation policy → per-cell verification is the
        // honest "nothing to verify" degrade: every slot stays None and no
        // cell is pruned.
        let tmp = tempfile::tempdir().unwrap();
        let ids = write_grid(tmp.path());
        let clock = FrozenClock::default();

        let dist = assemble_ensemble_distribution(tmp.path(), &ids, "", &clock).unwrap();
        assert!(!dist.cells.is_empty());
        assert_eq!(dist.n_pruned, 0, "nothing to prune without a policy");
        for cell in &dist.cells {
            assert!(
                cell.verification.is_none(),
                "verification must be None without a policy for {}",
                cell.cell_id
            );
        }
    }

    // -- per-cell verifier pruning ---------------------------------------

    /// Writes the package-embedded `policies/interpretation-policy.json` with
    /// an enabled `verifiableEntities` block (the same shape the claim
    /// verifier's own tests use) so the aggregator builds a real
    /// `ExtractorConfig`.
    fn write_interpretation_policy(root: &Path) {
        let dir = root.join("policies");
        std::fs::create_dir_all(&dir).unwrap();
        let policy = serde_json::json!({
            "verifiableEntities": {
                "enabled": true,
                "entityNamePatterns": ["[A-Z][A-Z0-9]{1,}"],
                "directionVocab": {
                    "up": ["upregulated", "increased", "elevated"],
                    "down": ["downregulated", "decreased", "reduced"]
                },
                "effectSizeColumns": ["log2FC", "logFC"],
                "entityColumns": ["gene", "symbol"],
                "pvalueColumns": ["padj", "pvalue"]
            }
        });
        std::fs::write(
            dir.join("interpretation-policy.json"),
            serde_json::to_string_pretty(&policy).unwrap(),
        )
        .unwrap();
    }

    /// Writes a cell's METHOD-VARIANT result table under
    /// `runtime/outputs/<primary>__v_<method>/de_summary_s1.tsv` — the dir the
    /// verifier resolves the cell's cited table against (NOT the cell dir).
    fn write_method_variant_table(root: &Path, primary: &str, method: &str, body: &str) {
        let dir = root
            .join("runtime")
            .join("outputs")
            .join(format!("{primary}__v_{method}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("de_summary_s1.tsv"), body).unwrap();
    }

    /// Writes a cell dir carrying a `result.json` (support verdict) and a
    /// prose `report.md` narrative the verifier cross-checks.
    fn write_cell_with_narrative(root: &Path, method: &str, lens: &str, support: bool, narrative: &str) {
        let id = cell_id(method, lens);
        let dir = root.join("runtime").join("outputs").join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        let body = serde_json::json!({ "hypothesis_supported": support, "literature": [] });
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_string_pretty(&body).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("report.md"), narrative).unwrap();
    }

    /// `verification` reads `n_mismatch` off a cell's serialized report.
    fn n_mismatch(cell: &CellRollup) -> Option<u64> {
        cell.verification
            .as_ref()
            .and_then(|v| v.get("n_mismatch"))
            .and_then(|v| v.as_u64())
    }

    fn n_checked(cell: &CellRollup) -> Option<u64> {
        cell.verification
            .as_ref()
            .and_then(|v| v.get("n_checked"))
            .and_then(|v| v.as_u64())
    }

    const PRIMARY: &str = "differential_expression";
    // ACAN is DOWN-regulated (log2FC = -1.2) in the method-variant table.
    const DOWN_TABLE: &str = "gene\tlog2FC\tpadj\nACAN\t-1.2\t0.001\n";
    // Narrative CONTRADICTS the table (claims UP with a positive effect).
    const CONTRADICTING_NARRATIVE: &str = "ACAN was upregulated (log2FC=2.1, Table S1).";
    // Narrative CONSISTENT with the table (down, matching magnitude).
    const CONSISTENT_NARRATIVE: &str = "ACAN was downregulated (log2FC=-1.2, Table S1).";

    #[test]
    fn cell_with_contradicting_narrative_is_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_interpretation_policy(root);

        // Two cells over the same lens; both tables say ACAN is DOWN.
        write_method_variant_table(root, PRIMARY, "deseq2", DOWN_TABLE);
        write_method_variant_table(root, PRIMARY, "edger", DOWN_TABLE);
        // deseq2 cell's narrative contradicts its table → Mismatch → pruned.
        write_cell_with_narrative(root, "deseq2", "molecular_mechanism", true, CONTRADICTING_NARRATIVE);
        // edger cell's narrative is consistent → verified → counts.
        write_cell_with_narrative(root, "edger", "molecular_mechanism", true, CONSISTENT_NARRATIVE);

        let ids = vec![
            cell_id("deseq2", "molecular_mechanism"),
            cell_id("edger", "molecular_mechanism"),
        ];
        let clock = FrozenClock::default();
        let dist = assemble_ensemble_distribution(root, &ids, PRIMARY, &clock).unwrap();

        // Both cells retained.
        assert_eq!(dist.cells.len(), 2, "pruned cells are retained, not dropped");
        let bad = dist
            .cells
            .iter()
            .find(|c| c.method == "deseq2")
            .expect("deseq2 cell retained");
        let good = dist
            .cells
            .iter()
            .find(|c| c.method == "edger")
            .expect("edger cell retained");

        // The contradicting cell carries a Mismatch verdict.
        assert!(
            n_mismatch(bad).unwrap_or(0) > 0,
            "contradicting cell must show n_mismatch>0; verification={:?}",
            bad.verification
        );
        // The consistent cell verified cleanly (and actually ran).
        assert_eq!(n_mismatch(good), Some(0), "consistent cell has no mismatch");
        assert!(
            n_checked(good).unwrap_or(0) > 0,
            "verification must have actually run on the consistent cell"
        );

        // Exactly one cell pruned.
        assert_eq!(dist.n_pruned, 1);

        // Consensus/marginals computed over the SURVIVING cell only: the
        // pruned deseq2 cell does not vote, so `edger` is the only method
        // marginal and agreement is over the single survivor.
        assert!(
            !dist.attribution.by_method.contains_key("deseq2"),
            "pruned cell must not appear in the method marginals"
        );
        assert!((dist.attribution.by_method["edger"] - 1.0).abs() < 1e-9);
        assert!((dist.agreement - 1.0).abs() < 1e-9, "one surviving vote → 1.0");
        assert_eq!(
            dist.consensus_label,
            "model agreement across 1 cells — not verified truth"
        );
    }

    #[test]
    fn clean_cell_counts_toward_consensus() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_interpretation_policy(root);

        // Two consistent cells → both verified, none pruned, both vote.
        write_method_variant_table(root, PRIMARY, "deseq2", DOWN_TABLE);
        write_method_variant_table(root, PRIMARY, "edger", DOWN_TABLE);
        write_cell_with_narrative(root, "deseq2", "molecular_mechanism", true, CONSISTENT_NARRATIVE);
        write_cell_with_narrative(root, "edger", "molecular_mechanism", true, CONSISTENT_NARRATIVE);

        let ids = vec![
            cell_id("deseq2", "molecular_mechanism"),
            cell_id("edger", "molecular_mechanism"),
        ];
        let clock = FrozenClock::default();
        let dist = assemble_ensemble_distribution(root, &ids, PRIMARY, &clock).unwrap();

        assert_eq!(dist.n_pruned, 0, "no contradictions → nothing pruned");
        for cell in &dist.cells {
            assert_eq!(n_mismatch(cell), Some(0), "clean cell has no mismatch");
            assert!(n_checked(cell).unwrap_or(0) > 0, "verification ran");
        }
        // Both surviving support-bearing cells agree → agreement 1.0, N=2.
        assert!((dist.agreement - 1.0).abs() < 1e-9);
        assert!((dist.attribution.by_method["deseq2"] - 1.0).abs() < 1e-9);
        assert!((dist.attribution.by_method["edger"] - 1.0).abs() < 1e-9);
        assert_eq!(
            dist.consensus_label,
            "model agreement across 2 cells — not verified truth"
        );
    }

    #[test]
    fn verification_pruning_is_deterministic() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write_interpretation_policy(root);
        write_method_variant_table(root, PRIMARY, "deseq2", DOWN_TABLE);
        write_method_variant_table(root, PRIMARY, "edger", DOWN_TABLE);
        write_cell_with_narrative(root, "deseq2", "molecular_mechanism", true, CONTRADICTING_NARRATIVE);
        write_cell_with_narrative(root, "edger", "molecular_mechanism", true, CONSISTENT_NARRATIVE);
        let ids = vec![
            cell_id("deseq2", "molecular_mechanism"),
            cell_id("edger", "molecular_mechanism"),
        ];
        let clock = FrozenClock::default();

        let a = assemble_ensemble_distribution(root, &ids, PRIMARY, &clock).unwrap();
        let a_json = serde_json::to_string_pretty(&a).unwrap();
        let a_disk = std::fs::read_to_string(
            root.join("runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json"),
        )
        .unwrap();
        let b = assemble_ensemble_distribution(root, &ids, PRIMARY, &clock).unwrap();
        let b_json = serde_json::to_string_pretty(&b).unwrap();
        let b_disk = std::fs::read_to_string(
            root.join("runtime/outputs/assemble_ensemble_distribution/ensemble-distribution.json"),
        )
        .unwrap();

        assert_eq!(a, b, "verified EnsembleDistribution identical across runs");
        assert_eq!(a_json, b_json, "serialized result byte-identical across runs");
        assert_eq!(a_disk, b_disk, "on-disk product byte-identical across runs");
    }
}
