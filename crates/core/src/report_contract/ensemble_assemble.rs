//! Deterministic cross-method robustness aggregator for the multi-analyst
//! ensemble. Given the K statistical-method variants of one terminal
//! analytical stage (e.g. `differential_expression__v_deseq2` /
//! `..edger` / `..limma`), it reads each variant's declared result
//! artifact, resolves the per-entity effect + significance BY THE SHARED
//! [`ResultSchema`] (never positionally), and classifies every entity's
//! cross-method robustness into one of four buckets. It also emits a
//! pooled consensus single-artifact view (`report-data.json`) so the
//! existing reporting-invariants recompute machinery still has something
//! to read for the ensemble.
//!
//! Pure over its on-disk inputs and never touches the wall clock (threads
//! [`Clock`] per the emit-path determinism contract, though it is
//! reserved/unused today); iterates inputs in sorted / `BTreeMap` order so
//! the emitted JSON is byte-reproducible. Outputs live under
//! `runtime/outputs/assemble_statistical_distribution/` and are runtime
//! products (excluded from the emitted-package byte-repro baseline).
//!
//! Lives inside the `report_contract` module so it can reach the
//! `pub(crate)` [`read_table`](super::assemble::read_table) parser and the
//! `pub(crate)` [`build_entity_rows`](super::report_data::build_entity_rows)
//! per-entity resolver without duplicating either.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::{Context, Result};

use crate::clock::Clock;
use crate::reexecution_bounds::ModalityBounds;

use super::assemble::read_table;
use super::report_data::build_entity_rows;
use super::{
    DirectionSplit, DistBin, EntityRow, LiteratureStatus, ReportData, ResultArtifactSummary,
    ResultSchema, load_policy_column_synonyms, should_spill, summarize_artifact,
    write_supplementary,
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
}
