//! Archetype catalog loader.
//!
//! Walks `config/archetypes/<id>.yaml` files, validates each against
//! `_archetype.schema.json`, deserializes into `ArchetypeDefinition`,
//! and exposes `find_match` for the composer's fast-path matcher.
//!
//! Mirrors the AtomRegistry shape (S4.2) so the composer's two reads
//! — atom catalog + archetype catalog — share validation discipline.

use crate::archetype::{ArchetypeDefinition, CURRENT_ARCHETYPE_SCHEMA_VERSION};
use crate::blocker::BlockerKind;
use crate::edam::is_subtype_of;
use anyhow::{anyhow, Context, Result};
use jsonschema::JSONSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use ts_rs::TS;

/// Embedded schema (compile-time include). Mirrors the file at
/// `config/archetypes/_archetype.schema.json` so validation works
/// without runtime path resolution.
const ARCHETYPE_SCHEMA_JSON: &str =
    include_str!("../../../config/archetypes/_archetype.schema.json");

/// In-memory archetype catalog. Keyed by id; BTreeMap for deterministic
/// iteration order across runs.
#[derive(Debug, Clone, Default)]
pub struct ArchetypeRegistry {
    archetypes: BTreeMap<String, ArchetypeDefinition>,
}

impl ArchetypeRegistry {
    /// Walk `dir`, load every `*.yaml` file (excluding `_*.yaml`
    /// schema sidecars). Returns an empty registry when the directory
    /// is missing — mirrors AtomRegistry's permissive shape so the
    /// composer's legacy fallback continues to work pre-Stage-6.
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let schema = Self::compiled_schema()?;
        let mut archetypes = BTreeMap::new();
        if !dir.exists() {
            return Ok(Self { archetypes });
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .with_context(|| format!("reading archetype dir {}", dir.display()))?
            .filter_map(|r| r.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension().and_then(|s| s.to_str()) == Some("yaml")
                    && !p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with('_') || n.ends_with(".slots.yaml"))
                        .unwrap_or(false)
            })
            .collect();
        entries.sort();
        for path in entries {
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("reading archetype file {}", path.display()))?;
            let yaml_val: serde_yml::Value = serde_yml::from_str(&raw)
                .with_context(|| format!("parsing archetype YAML {}", path.display()))?;
            let parsed: Value = serde_json::to_value(&yaml_val)
                .with_context(|| format!("yaml→json reshape for {}", path.display()))?;
            // C23 — surface a typed schema_version_mismatch error
            // BEFORE the JSON Schema validator's generic `const`
            // failure. Caller (runtime classification, replay) can
            // promote to `BlockerKind::SchemaVersionMismatch` via
            // [`schema_version_mismatch_blocker`]; startup loaders log
            // and continue when the registry is permissive.
            if let Some(found) = parsed.get("schema_version").and_then(|v| v.as_str()) {
                if found != CURRENT_ARCHETYPE_SCHEMA_VERSION {
                    return Err(anyhow!(
                        "archetype {} schema_version_mismatch: \
                         expected {}, found {}",
                        path.display(),
                        CURRENT_ARCHETYPE_SCHEMA_VERSION,
                        found,
                    ));
                }
            }
            if let Err(errors) = schema.validate(&parsed) {
                let msgs: Vec<String> = errors
                    .map(|e| format!("{} at {}", e, e.instance_path))
                    .collect();
                return Err(anyhow!(
                    "archetype {} failed schema validation:\n  - {}",
                    path.display(),
                    msgs.join("\n  - ")
                ));
            }
            let archetype: ArchetypeDefinition = serde_json::from_value(parsed)
                .with_context(|| format!("deserializing archetype {}", path.display()))?;
            // Optional `<id>.slots.yaml` sidecar attaches a SlotManifest
            // for closed-enum slot-filling. The sidecar lives next to
            // the primary archetype file; the filter above excludes it
            // from the primary scan so it never deserializes as its
            // own archetype.
            let slots_path = path.with_extension("slots.yaml");
            let archetype = if slots_path.exists() {
                let slots_raw = std::fs::read_to_string(&slots_path)
                    .with_context(|| format!("reading slot sidecar {}", slots_path.display()))?;
                let slots: crate::archetype_slots::SlotManifest = serde_yml::from_str(&slots_raw)
                    .with_context(|| {
                    format!("parsing slot sidecar {}", slots_path.display())
                })?;
                ArchetypeDefinition {
                    slots: Some(slots),
                    ..archetype
                }
            } else {
                archetype
            };
            // Filename stem must match the archetype id.
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or_else(|| anyhow!("archetype path {} has no stem", path.display()))?;
            if stem != archetype.id {
                return Err(anyhow!(
                    "archetype file {} has stem {} but declares id {}",
                    path.display(),
                    stem,
                    archetype.id
                ));
            }
            if archetypes
                .insert(archetype.id.clone(), archetype.clone())
                .is_some()
            {
                return Err(anyhow!(
                    "duplicate archetype id {} (second file: {})",
                    archetype.id,
                    path.display()
                ));
            }
        }
        Ok(Self { archetypes })
    }

    /// Iterate archetypes in id-sorted order.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &ArchetypeDefinition)> {
        self.archetypes.iter()
    }

    /// Test-only constructor. Builds a registry directly
    /// from a list of archetypes without round-tripping through the
    /// schema validator. Used by inheritance tests that need
    /// synthetic archetypes the catalog doesn't ship.
    pub fn test_from_archetypes(archetypes: Vec<ArchetypeDefinition>) -> Self {
        let mut map = BTreeMap::new();
        for a in archetypes {
            map.insert(a.id.clone(), a);
        }
        Self { archetypes: map }
    }

    /// Get.
    pub fn get(&self, id: &str) -> Option<&ArchetypeDefinition> {
        self.archetypes.get(id)
    }

    /// Len.
    pub fn len(&self) -> usize {
        self.archetypes.len()
    }

    /// Is empty.
    pub fn is_empty(&self) -> bool {
        self.archetypes.is_empty()
    }

    /// Score-based archetype matching. Given a goal's
    /// `(edam_data, edam_format, project_class)` triple, return the
    /// archetypes ranked by match score.
    ///
    /// Scoring rules (curated EDAM subtype graph aware):
    /// +3 if archetype.goal_data == target.edam_data (exact)
    /// +2 if archetype.goal_data is a subtype of target.edam_data,
    /// or target.edam_data is a subtype of archetype.goal_data
    /// (partial credit via `crate::edam::is_subtype_of`)
    /// +2 if archetype.goal_format matches target.edam_format
    /// +1 if archetype.project_class matches
    /// 0 otherwise
    ///
    /// The 5% tie-surfacing rule per [DEC Q2.4] is honored by the
    /// composer's caller: when the top two candidates' scores are
    /// within 5% of the maximum, the SME-facing tie-breaking card
    /// renders. See `composer::compose` for the
    /// `CompositionError::TieRequiresSmeDecision` path.
    pub fn find_match(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
    ) -> Vec<(&ArchetypeDefinition, u32)> {
        self.find_match_with_evidence(target_data, target_format, target_class)
            .into_iter()
            .map(|m| (m.archetype, m.evidence.total))
            .collect()
    }

    /// Tie-fix variant of `find_match` that also
    /// consumes the classifier's modality (e.g. `bulk_rnaseq`,
    /// `long_read_rnaseq`). When the classifier produces a goal
    /// triple that's shared by multiple archetypes (the
    /// `data:0951 / format:3475` DE case where bulk + long-read +
    /// metagenomics all tie at score 6), the modality hint adds
    /// +2 to the matching archetype, lifting it outside the 5%-
    /// tie window so the composer fast-path commits cleanly.
    /// `target_modality = None` falls through to the
    /// modality-blind path (legacy callers stay byte-stable).
    pub fn find_match_with_modality(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
        target_modality: Option<&str>,
    ) -> Vec<(&ArchetypeDefinition, u32)> {
        self.find_match_with_evidence_and_modality(
            target_data,
            target_format,
            target_class,
            target_modality,
        )
        .into_iter()
        .map(|m| (m.archetype, m.evidence.total))
        .collect()
    }

    /// Modality-aware `find_match_with_evidence`
    /// that adds the +2 modality-match component to the score.
    pub fn find_match_with_evidence_and_modality(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
        target_modality: Option<&str>,
    ) -> Vec<ArchetypeMatch<'_>> {
        self.find_match_with_evidence_modality_kind(
            target_data,
            target_format,
            target_class,
            target_modality,
            None,
        )
    }

    /// Full disambiguation surface.
    /// Combines modality_hint (+2) and goal_kind_hint (+2)
    /// scoring components. Resolves proteomics DDA-vs-DIA where
    /// both archetypes share `modality_hint: proteomics` + same
    /// goal_data/format/class but distinct goal_kind_hint values
    /// (`proteomics_dda` vs `proteomics_dia`). The classifier
    /// surfaces `goal.modifiers.kind` as the target_kind.
    pub fn find_match_with_evidence_modality_kind(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
        target_modality: Option<&str>,
        target_kind: Option<&str>,
    ) -> Vec<ArchetypeMatch<'_>> {
        let mut scored: Vec<ArchetypeMatch<'_>> = self
            .archetypes
            .values()
            // Single-modality matcher must not consider cross-omics
            // archetypes. They're gated by `find_match_cross_omics`
            // instead. Otherwise a single-modality bulk_rnaseq request
            // would tie against `cross_omics_rnaseq_proteomics` purely
            // on goal_data / project_class shape.
            .filter(|a| a.cross_omics_modalities.is_empty())
            .map(|a| {
                let evidence = score_archetype_full(
                    a,
                    target_data,
                    target_format,
                    target_class,
                    target_modality,
                    target_kind,
                );
                ArchetypeMatch {
                    archetype: a,
                    evidence,
                }
            })
            .filter(|m| m.evidence.total > 0)
            .collect();
        // Modality_match is the primary sort key. A raw-total sort
        // would let a generic goal-data match (e.g. bulk_rnaseq_de
        // scoring 6 on data:0951 + format:3475 + project_class for
        // an ATAC intake whose classifier inferred goal=DE from
        // "DESeq2") outrank the modality-specific archetype
        // (atac_seq_peaks scoring 3 on project_class +
        // modality_match). The 5% tie-window would then see multiple
        // goal-tied archetypes, skip the seed entirely, and fall to
        // the polluted search-only path.
        // Sorting by modality_match first guarantees a modality-
        // specific match outranks any pure goal-triple match.
        scored.sort_by(|a, b| {
            b.evidence
                .modality_match
                .cmp(&a.evidence.modality_match)
                .then(b.evidence.total.cmp(&a.evidence.total))
                .then(a.archetype.id.cmp(&b.archetype.id))
        });
        scored
    }

    /// Find_match-shaped wrapper for the full (modality + kind)
    /// scoring surface. Returns `(archetype, score)` tuples in
    /// score-descending order.
    pub fn find_match_with_modality_and_kind(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
        target_modality: Option<&str>,
        target_kind: Option<&str>,
    ) -> Vec<(&ArchetypeDefinition, u32)> {
        self.find_match_with_evidence_modality_kind(
            target_data,
            target_format,
            target_class,
            target_modality,
            target_kind,
        )
        .into_iter()
        .map(|m| (m.archetype, m.evidence.total))
        .collect()
    }

    /// Same scoring as `find_match` but exposes the per-component
    /// breakdown so the UI rationale card (`AtomSelectionRationale`)
    /// can render *why* each candidate scored where it did. Returned
    /// candidates are filtered to those with score > 0 and sorted
    /// descending by total score; ties are broken on archetype id
    /// (stable + deterministic).
    pub fn find_match_with_evidence(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
    ) -> Vec<ArchetypeMatch<'_>> {
        let mut scored: Vec<ArchetypeMatch<'_>> = self
            .archetypes
            .values()
            // Exclude cross-omics archetypes (gated by
            // `find_match_cross_omics` instead). See sibling
            // `find_match_with_evidence_modality_kind` for details.
            .filter(|a| a.cross_omics_modalities.is_empty())
            .map(|a| {
                let evidence = score_archetype(a, target_data, target_format, target_class);
                ArchetypeMatch {
                    archetype: a,
                    evidence,
                }
            })
            .filter(|m| m.evidence.total > 0)
            .collect();
        scored.sort_by(|a, b| {
            b.evidence
                .total
                .cmp(&a.evidence.total)
                .then(a.archetype.id.cmp(&b.archetype.id))
        });
        scored
    }

    /// Match cross-omics archetypes whose
    /// `cross_omics_modalities` set equals (order-insensitive) the
    /// SME's requested modality set.
    ///
    /// Returns ranked `(archetype, score)` tuples for archetypes that
    /// (a) declare a non-empty `cross_omics_modalities` field and (b)
    /// declare exactly the same set of modalities the SME asked for.
    /// Single-modality archetypes are excluded from this matcher even
    /// when the SME asked for one of their modalities — the caller
    /// (`compose_with_version_and_modalities`) is expected to fall
    /// through to single-modality matching when no cross-omics
    /// archetype set-matches.
    ///
    /// Set-equality (rather than subset-match) is the conservative
    /// choice. A 3-way request `[rnaseq, proteomics, atac]` does NOT
    /// match a 2-way archetype `[rnaseq, proteomics]` — the caller
    /// must explicitly choose between (a) authoring a 3-way archetype,
    /// (b) running the 2-way + dropping atac, or (c) running the
    /// backward-chain safety net. Subset-matching would silently drop
    /// the third modality, which is the bug the amendment is fixing
    /// in the first place.
    #[allow(clippy::too_many_arguments)]
    pub fn find_match_cross_omics(
        &self,
        target_data: &str,
        target_format: Option<&str>,
        target_class: &str,
        target_modalities: &[&str],
        target_kind: Option<&str>,
        n_way_intent: bool,
        intake_prose: &str,
    ) -> Vec<(&ArchetypeDefinition, u32)> {
        if target_modalities.len() < 2 {
            // Single-modality fallback: when only ONE modality
            // surfaced from the classifier but we KNOW the SME wanted
            // multi-modal (caller invokes this because the intent
            // gate fired), try the superset fallback below — there
            // may be a multi-omics archetype that covers the lone
            // requested modality plus one or two others. This rescues
            // multiome / share-seq prompts when the classifier under-
            // counted modalities (e.g., produced [single_cell_rnaseq]
            // only when the prose mentioned single-nucleus ATAC too).
            // Below we still gate the superset fallback on share_with
            // >= 1.
            if target_modalities.is_empty() {
                return Vec::new();
            }
        }
        let want: std::collections::BTreeSet<&str> = target_modalities.iter().copied().collect();
        let mut scored: Vec<(&ArchetypeDefinition, u32)> = self
            .archetypes
            .values()
            .filter(|a| !a.cross_omics_modalities.is_empty())
            .filter(|a| {
                let have: std::collections::BTreeSet<&str> = a
                    .cross_omics_modalities
                    .iter()
                    .map(String::as_str)
                    .collect();
                have == want
            })
            .map(|a| {
                // The set-equality filter above already validated
                // that the archetype's modality coverage matches the
                // SME's request, so there's no separate +2 modality
                // bonus to award here. Pass `None` to suppress the
                // single-modality `modality_hint` bonus path.
                let evidence = score_archetype_full(
                    a,
                    target_data,
                    target_format,
                    target_class,
                    None,
                    target_kind,
                );
                (a, evidence.total)
            })
            .filter(|(_, score)| *score > 0)
            .collect();
        if !scored.is_empty() {
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
            return scored;
        }

        // N-way false-positive-companion fallback. In explicit 3-way
        // prompts the classifier can surface an extra weak companion
        // (for example `gwas` from "association" vocabulary in a
        // WGS+RNA+ChIP regulatory-variant request). Strict set
        // equality then misses the registered 3-branch archetype even
        // though every modality it covers is named in prose. When the
        // SME explicitly signaled n-way intent, prefer an archetype
        // whose modality set is a proper subset of the classifier set
        // if the prose names every modality in that archetype.
        if n_way_intent && target_modalities.len() > 2 {
            let prose_normalized = if intake_prose.is_empty() {
                String::new()
            } else {
                crate::classify::normalize_for_match(intake_prose)
            };
            let prose_gate_active = !prose_normalized.is_empty();
            let mut subset_scored: Vec<(&ArchetypeDefinition, usize, usize, u32)> = self
                .archetypes
                .values()
                .filter(|a| !a.cross_omics_modalities.is_empty())
                .filter_map(|a| {
                    let have: std::collections::BTreeSet<&str> = a
                        .cross_omics_modalities
                        .iter()
                        .map(String::as_str)
                        .collect();
                    if have.len() < 2 || have.len() >= want.len() || !have.is_subset(&want) {
                        return None;
                    }
                    if prose_gate_active {
                        let omitted: std::collections::BTreeSet<&str> =
                            want.difference(&have).copied().collect();
                        if count_prose_modality_hits(&omitted, &prose_normalized) > 0 {
                            return None;
                        }
                    }
                    let prose_hits = if prose_gate_active {
                        count_prose_modality_hits(&have, &prose_normalized)
                    } else {
                        0
                    };
                    if prose_gate_active && prose_hits < have.len() {
                        return None;
                    }
                    let evidence = score_archetype_full(
                        a,
                        target_data,
                        target_format,
                        target_class,
                        None,
                        target_kind,
                    );
                    Some((a, prose_hits, have.len(), evidence.total))
                })
                .collect();
            subset_scored.sort_by(|a, b| {
                b.1.cmp(&a.1)
                    .then(b.2.cmp(&a.2))
                    .then(b.3.cmp(&a.3))
                    .then(a.0.id.cmp(&b.0.id))
            });
            if !subset_scored.is_empty() {
                return subset_scored
                    .into_iter()
                    .map(|(a, _, _, total)| (a, total))
                    .collect();
            }
        }

        // Superset fallback applies when the classifier produced exactly
        // one modality (and the SME named cross-omics intent) OR when
        // the caller signals explicit n-way intent (e.g., "tri-omics",
        // "three-way analysis", "n-way integration"). For 2-modality
        // inputs without n_way_intent, falling through to
        // `cross_omics_generic_multi_modal` (the generic multi-branch
        // synthesizer) is still the correct conservative behavior —
        // injecting orphan branches for unrequested modalities is the
        // bug class this gate prevents.
        if target_modalities.len() != 1 && !n_way_intent {
            return Vec::new();
        }

        // Superset fallback. When the strict set-equality returned
        // nothing — classifier under-counted modalities to a single
        // entry — pick archetypes whose `cross_omics_modalities` is a
        // strict SUPERSET of the requested set (i.e., `want ⊂ have`).
        // The archetype declares more modalities than the SME
        // explicitly named, but since the SME named cross-omics intent
        // and the lone classified modality matches, the archetype is
        // the closest catalog entry.
        //
        // Selection priority (prose-aware):
        // 1. `prose_modality_hits` — how many of the archetype's
        //    `cross_omics_modalities` actually appear in the SME's
        //    intake prose (via canonical-token containment on a
        //    normalized buffer). Requires `>= 2` to be eligible (one
        //    is the lone classified modality, at least one more must
        //    be evident in prose). Eliminates the modality-blind
        //    picks where an archetype is chosen purely because it
        //    contains the lone classified modality (the
        //    `variant-rnaseq-chip-encode-celllines` misroute into
        //    `cross_omics_rnaseq_methylation` for prose that names
        //    WGS + ChIP-seq).
        // 2. Maximum overlap (most of the SME's requested modalities
        //    matched by the archetype).
        // 3. Smallest archetype modality set (least extra coverage —
        //    the SME asked for X+Y, prefer X+Y+Z archetype over
        //    X+Y+Z+W if both exist).
        // 4. Score, then archetype id, for determinism.
        //
        // Back-compat: an empty `intake_prose` skips the prose gate
        // and reverts to the legacy (overlap, size) sort so callers
        // that don't have prose handy still get a deterministic pick.
        let prose_normalized = if intake_prose.is_empty() {
            String::new()
        } else {
            crate::classify::normalize_for_match(intake_prose)
        };
        let prose_gate_active = !prose_normalized.is_empty();
        let mut superset_scored: Vec<(&ArchetypeDefinition, usize, usize, usize, u32)> = self
            .archetypes
            .values()
            .filter(|a| !a.cross_omics_modalities.is_empty())
            .filter_map(|a| {
                let have: std::collections::BTreeSet<&str> = a
                    .cross_omics_modalities
                    .iter()
                    .map(String::as_str)
                    .collect();
                let overlap = want.intersection(&have).count();
                // Require at least 1 shared modality, and the archetype
                // must cover STRICTLY more than the SME's request.
                if overlap == 0 || have.len() <= want.len() {
                    return None;
                }
                let prose_hits = if prose_gate_active {
                    count_prose_modality_hits(&have, &prose_normalized)
                } else {
                    0
                };
                if prose_gate_active && prose_hits < 2 {
                    return None;
                }
                let evidence = score_archetype_full(
                    a,
                    target_data,
                    target_format,
                    target_class,
                    None,
                    target_kind,
                );
                Some((a, prose_hits, overlap, have.len(), evidence.total))
            })
            .collect();
        // Sort: prose-hits desc, overlap desc, archetype-size asc, score desc, id asc.
        superset_scored.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(b.2.cmp(&a.2))
                .then(a.3.cmp(&b.3))
                .then(b.4.cmp(&a.4))
                .then(a.0.id.cmp(&b.0.id))
        });
        superset_scored
            .into_iter()
            .map(|(a, _, _, _, total)| (a, total))
            .collect()
    }

    /// Pick the canonical "primary" archetype for a
    /// modality + project_class pair when no goal phrase is available.
    ///
    /// This is the bare-modality fallback path: SME prose like
    /// "single cell scRNA-seq from human IVD samples with 10x Chromium"
    /// classifies to `modality = single_cell_rnaseq, project_class =
    /// research`, but [`crate::classify::ClassificationResult::goal`]
    /// stays `None` because no `goal_pattern` matched. The standard
    /// goal-based archetype matcher
    /// (`find_match_with_evidence_modality_kind`) then returns nothing,
    /// and v4 dispatch fails. This helper closes that gap.
    ///
    /// Selection rules (deterministic):
    ///
    /// 1. **Modality + project_class match.** Filter to archetypes
    ///    whose `modality_hint == modality` AND `project_class ==
    /// project_class` AND `cross_omics_modalities.is_empty()`
    ///    (single-modality only; cross-omics has its own selector).
    ///    Among the filtered set, prefer the archetype with the
    ///    smallest atom count — the canonical "do the modality"
    ///    archetype is typically a thin scaffold, while specialized
    ///    variants accrete atoms. Empirically, this heuristic picks
    ///    the right primary for every modality in today's catalog
    ///    (`bulk_rnaseq → bulk_rnaseq_de`,
    ///    `single_cell_rnaseq → single_cell_de`,
    ///    `chip_seq → chip_seq_peaks`,
    ///    `atac_seq → atac_seq_peaks`,
    ///    `variant_calling → variant_calling_germline`,
    ///    `metagenomics → metagenomics_taxonomic`,
    ///    `proteomics → proteomics_dda`).
    /// 2. **Project-class fallback.** When no modality match, fall
    ///    through to project-class-routed archetypes whose
    ///    `modality_hint` is unset and whose `project_class` matches.
    ///    This handles project-class-routed analyses
    ///    (`clinical_trial → clinical_trial_analysis`,
    ///    `time_series_forecast → time_series_forecast`) where the
    ///    classifier may set `modality = generic_omics` even though
    ///    the workflow is project-class-driven.
    /// 3. Lexical id ordering breaks ties when atom counts match.
    ///
    /// Returns `None` when no archetype matches (e.g., a modality with
    /// no archetype defined yet AND no matching project-class fallback).
    pub fn find_primary_for_modality(
        &self,
        modality: &str,
        project_class: &str,
    ) -> Option<&ArchetypeDefinition> {
        // Pass 1: prefer archetypes whose `modality_hint` matches.
        let modality_match = self
            .archetypes
            .values()
            .filter(|a| a.cross_omics_modalities.is_empty())
            .filter(|a| a.project_class == project_class)
            .filter(|a| a.modality_hint.as_deref() == Some(modality))
            .min_by(|a, b| {
                a.atoms
                    .len()
                    .cmp(&b.atoms.len())
                    .then_with(|| a.id.cmp(&b.id))
            });
        if modality_match.is_some() {
            return modality_match;
        }

        // Pass 2: project-class-only fallback. Project-class-routed
        // archetypes (clinical_trial_analysis, time_series_forecast)
        // typically leave `modality_hint` unset because the workflow
        // is project-class-driven rather than modality-driven.
        self.archetypes
            .values()
            .filter(|a| a.cross_omics_modalities.is_empty())
            .filter(|a| a.project_class == project_class)
            .filter(|a| a.modality_hint.is_none())
            .min_by(|a, b| {
                a.atoms
                    .len()
                    .cmp(&b.atoms.len())
                    .then_with(|| a.id.cmp(&b.id))
            })
    }

    /// Return the smallest single-modality archetype whose
    /// `modality_hint` exactly matches, regardless of project class.
    ///
    /// This is deliberately narrower than [`find_primary_for_modality`]:
    /// it never falls back to project-class-routed archetypes. Dispatch
    /// uses it when the modality classifier is high-confidence but the
    /// softer project-class classifier was fooled by domain prose such as
    /// "Phase 3 EUR" in GWAS/LD-reference descriptions.
    pub fn find_primary_for_modality_hint_any_project(
        &self,
        modality: &str,
    ) -> Option<&ArchetypeDefinition> {
        self.archetypes
            .values()
            .filter(|a| a.cross_omics_modalities.is_empty())
            .filter(|a| a.modality_hint.as_deref() == Some(modality))
            .min_by(|a, b| {
                a.atoms
                    .len()
                    .cmp(&b.atoms.len())
                    .then_with(|| a.id.cmp(&b.id))
            })
    }

    /// Tie-surfacing helper. Returns the candidates whose score is
    /// within `tie_window` (default 5%) of the top score. Used by the
    /// composer to decide whether to commit to a single archetype or
    /// surface a confirmation card.
    ///
    /// `tie_window = 0.05` gives a 5% window. Caller may pass a
    /// different value to widen / narrow the window for testing.
    pub fn candidates_within_tie_window<'a>(
        matches: &'a [ArchetypeMatch<'a>],
        tie_window: f32,
    ) -> Vec<&'a ArchetypeDefinition> {
        if matches.is_empty() {
            return Vec::new();
        }
        let top = matches[0].evidence.total as f32;
        let cutoff = (top * (1.0 - tie_window)).floor() as u32;
        matches
            .iter()
            .filter(|m| m.evidence.total >= cutoff)
            .map(|m| m.archetype)
            .collect()
    }

    fn compiled_schema() -> Result<&'static JSONSchema> {
        crate::schema_helpers::compile_schema_cached("archetype", ARCHETYPE_SCHEMA_JSON)
    }

    /// Process-wide cached load. See `AtomRegistry::load_cached`.
    pub fn load_cached(dir: &Path) -> Result<Arc<Self>> {
        use std::collections::HashMap;
        use std::path::PathBuf;
        use std::sync::OnceLock;
        static CACHE: OnceLock<std::sync::Mutex<HashMap<PathBuf, Arc<ArchetypeRegistry>>>> =
            OnceLock::new();
        let cache = CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        if let Ok(guard) = cache.lock() {
            if let Some(reg) = guard.get(&key) {
                return Ok(reg.clone());
            }
        }
        let reg = Arc::new(Self::load_from_dir(dir)?);
        if let Ok(mut guard) = cache.lock() {
            guard.insert(key, reg.clone());
        }
        Ok(reg)
    }
}

/// Build a typed [`BlockerKind::SchemaVersionMismatch`] for an
/// archetype-config schema-version mismatch. C23 sibling of
/// `modality_registry::schema_version_mismatch_blocker`.
pub fn schema_version_mismatch_blocker(found: impl Into<String>) -> BlockerKind {
    BlockerKind::SchemaVersionMismatch {
        config_kind: "archetype_config".to_string(),
        expected: CURRENT_ARCHETYPE_SCHEMA_VERSION.to_string(),
        found: found.into(),
    }
}

/// Per-component scoring breakdown. Surfaces *why* an
/// archetype matched (or didn't) so the UI rationale card (S6.15) can
/// render the evidence inline.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS, schemars::JsonSchema,
)]
#[ts(export)]
pub struct ScoreEvidence {
    /// +3 when `archetype.goal_data` exactly matches target.
    pub goal_data_exact: u32,
    /// +2 when one is a subtype of the other (partial credit).
    pub goal_data_subtype: u32,
    /// +2 when `archetype.goal_format` matches target format.
    pub goal_format_match: u32,
    /// +1 when `archetype.project_class` matches target class.
    pub project_class_match: u32,
    /// +2 when `archetype.modality_hint` exactly matches the
    /// classifier's modality (e.g. `bulk_rnaseq`, `long_read_rnaseq`).
    /// Disambiguates DE-shaped goals where multiple archetypes produce
    /// the same goal_data/goal_format pair.
    #[serde(default)]
    pub modality_match: u32,
    /// +2 when `archetype.goal_kind_hint` matches the classifier's
    /// `goal.modifiers.kind`. Sub-modality disambiguator that resolves
    /// the proteomics DDA-vs-DIA tie.
    #[serde(default)]
    pub goal_kind_match: u32,
    /// Sum of all components.
    pub total: u32,
}

/// `(archetype, evidence)` tuple returned by
/// `find_match_with_evidence`. Borrowed reference to the archetype +
/// owned evidence struct.
#[derive(Debug)]
pub struct ArchetypeMatch<'a> {
    /// Archetype.
    pub archetype: &'a ArchetypeDefinition,
    /// Evidence.
    pub evidence: ScoreEvidence,
}

/// Score one archetype against a target triple. Free function so the
/// scoring rules are testable in isolation from the registry.
fn score_archetype(
    a: &ArchetypeDefinition,
    target_data: &str,
    target_format: Option<&str>,
    target_class: &str,
) -> ScoreEvidence {
    score_archetype_with_modality(a, target_data, target_format, target_class, None)
}

/// Modality-aware scorer. Same as `score_archetype`
/// plus a +2 component when `archetype.modality_hint == target_modality`.
/// Resolves the DE-shaped tie (3 archetypes producing
/// `data:0951 / format:3475` all scoring 6) by lifting the
/// modality-matching archetype to score 8 (outside the 5%-tie
/// window) so the composer commits cleanly.
fn score_archetype_with_modality(
    a: &ArchetypeDefinition,
    target_data: &str,
    target_format: Option<&str>,
    target_class: &str,
    target_modality: Option<&str>,
) -> ScoreEvidence {
    score_archetype_full(
        a,
        target_data,
        target_format,
        target_class,
        target_modality,
        None,
    )
}

/// Full scorer with goal_kind disambiguator.
/// Adds +2 when `archetype.goal_kind_hint == target_kind` (the
/// classifier's `goal.modifiers.kind`). Resolves the proteomics
/// DDA-vs-DIA tie where both archetypes share modality_hint +
/// goal_data + goal_format and would otherwise tie at score 8.
///
/// `project_class` is a hard partition, not a
/// tie-breaker. When the archetype's `project_class` differs from the
/// target's, scoring returns `total = 0` so the candidate is filtered
/// out by the upstream `> 0` filter. Otherwise project-class-routed
/// archetypes (`clinical_trial_analysis`, `time_series_forecast`)
/// silently lose to bioinformatics archetypes that share the same
/// `(goal_data, goal_format)` triple: the +1 they earn for matching
/// the target's project_class gets eaten by the 5% tie-window cutoff
/// (`floor(top_score * 0.95)`), so the matcher reports a 5-way tie
/// and the composer either soft-fails to the legacy taxonomy build
/// (v2) or skips the archetype seed entirely (v4). Filtering on
/// project_class up front makes the partition explicit: a
/// clinical-trial scenario will never match a bulk-rnaseq archetype,
/// and vice versa, regardless of how well the goal triple aligns.
fn score_archetype_full(
    a: &ArchetypeDefinition,
    target_data: &str,
    target_format: Option<&str>,
    target_class: &str,
    target_modality: Option<&str>,
    target_kind: Option<&str>,
) -> ScoreEvidence {
    let mut e = ScoreEvidence::default();
    // Hard project_class partition. Filter out
    // archetypes whose project_class doesn't match the target.
    if a.project_class != target_class {
        return e;
    }
    if a.goal_data == target_data {
        e.goal_data_exact = 3;
    } else if is_subtype_of(&a.goal_data, target_data) || is_subtype_of(target_data, &a.goal_data) {
        e.goal_data_subtype = 2;
    }
    if let (Some(want), Some(got)) = (target_format, a.goal_format.as_deref()) {
        if want == got {
            e.goal_format_match = 2;
        }
    }
    // project_class always matches here (filtered above); record the
    // +1 component so the breakdown stays self-explanatory.
    e.project_class_match = 1;
    if let (Some(want), Some(got)) = (target_modality, a.modality_hint.as_deref()) {
        if want == got {
            e.modality_match = 2;
        }
    }
    if let (Some(want), Some(got)) = (target_kind, a.goal_kind_hint.as_deref()) {
        if want == got {
            e.goal_kind_match = 2;
        }
    }
    e.total = e.goal_data_exact
        + e.goal_data_subtype
        + e.goal_format_match
        + e.project_class_match
        + e.modality_match
        + e.goal_kind_match;
    e
}

/// Canonical-token table mapping each cross-omics-registered modality
/// id to substrings whose presence (in a `normalize_for_match`-ed
/// prose buffer — lowercased + hyphens/underscores collapsed to
/// spaces) implies the SME named that modality in their intake. Used
/// by `find_match_cross_omics`'s superset fallback to rank candidate
/// archetypes by prose-mentioned modality count, so a 2-of-3 partial
/// classifier result rescues the 3-way archetype that actually
/// matches the SME's prose rather than the alphabetically-earliest
/// archetype that contains the lone classified modality.
const PROSE_MODALITY_TOKENS: &[(&str, &[&str])] = &[
    (
        "bulk_rnaseq",
        &["rna seq", "rnaseq", "transcriptomics", "transcriptome"],
    ),
    (
        "single_cell_rnaseq",
        &[
            "single cell rna",
            "scrna",
            "sc rna",
            "snrna",
            "sn rna",
            "single nucleus rna",
            "single cell transcriptomics",
        ],
    ),
    (
        "proteomics",
        &["proteomics", "proteome", "mass spec", "lc ms", "tmt"],
    ),
    (
        "atac_seq",
        &[
            "atac seq",
            "atacseq",
            "chromatin accessibility",
            "open chromatin",
        ],
    ),
    (
        "chip_seq",
        &["chip seq", "chipseq", "chromatin immunoprecipitation"],
    ),
    (
        "methylation",
        &[
            "methylation",
            "bisulfite",
            "rrbs",
            "wgbs",
            "methylome",
            "5mc",
            "dna methylation",
        ],
    ),
    (
        "variant_calling",
        &[
            "wgs",
            "wes",
            "exome",
            "variant call",
            "germline variant",
            "somatic variant",
            "snv",
        ],
    ),
];

/// Count how many of `have` (an archetype's `cross_omics_modalities`
/// set) have at least one canonical token present in
/// `prose_normalized` (assumed already passed through
/// `classify::normalize_for_match`). Modalities without an entry in
/// `PROSE_MODALITY_TOKENS` are treated as "not in prose" — they
/// can't contribute to the hit count, which is the conservative
/// choice (failing closed for novel modalities until they're
/// registered here).
fn count_prose_modality_hits(
    have: &std::collections::BTreeSet<&str>,
    prose_normalized: &str,
) -> usize {
    let mut hits = 0usize;
    for modality in have {
        if let Some((_, tokens)) = PROSE_MODALITY_TOKENS
            .iter()
            .find(|(id, _)| *id == *modality)
        {
            if tokens.iter().any(|t| prose_normalized.contains(t)) {
                hits += 1;
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_archetype(dir: &Path, name: &str, body: &str) {
        let p = dir.join(format!("{}.yaml", name));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn empty_dir_yields_empty_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let reg = ArchetypeRegistry::load_from_dir(tmp.path()).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn nonexistent_dir_yields_empty_registry() {
        let reg = ArchetypeRegistry::load_from_dir(Path::new("/nonexistent")).unwrap();
        assert!(reg.is_empty());
    }

    #[test]
    fn loads_real_archetype_catalog() {
        // Smoke-test the on-disk catalog via include_str! schema +
        // real config/ files. Validates that the archetype YAMLs
        // conform to the schema; a regression here would mean the
        // catalog drifted from the schema.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/archetypes");
        if !dir.exists() {
            return;
        }
        let reg = ArchetypeRegistry::load_from_dir(&dir).unwrap();
        assert!(
            reg.len() >= 10,
            "expected ≥ 10 archetypes, got {}",
            reg.len()
        );
        // Spot-check that the 13 catalog archetypes load.
        for required in [
            "bulk_rnaseq_de",
            "single_cell_de",
            "chip_seq_peaks",
            "atac_seq_peaks",
            "variant_calling_germline",
            "gwas_coloc",
        ] {
            assert!(
                reg.get(required).is_some(),
                "expected archetype {} in registry",
                required
            );
        }
    }

    #[test]
    fn find_match_scores_and_orders() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/archetypes");
        if !dir.exists() {
            return;
        }
        let reg = ArchetypeRegistry::load_from_dir(&dir).unwrap();
        // bulk_rnaseq_de produces DE results: data:0951, format:3475,
        // project_class bioinformatics. Should rank top against that
        // exact triple.
        let matches = reg.find_match("data:0951", Some("format:3475"), "bioinformatics");
        assert!(!matches.is_empty());
        assert!(matches[0].1 >= 3, "top score should be ≥ 3");
    }

    #[test]
    fn find_match_with_evidence_exposes_breakdown() {
        // Hand-rolled tiny registry so the test is independent of the
        // on-disk catalog drift.
        let mut archetypes = BTreeMap::new();
        archetypes.insert(
            "exact_match".into(),
            ArchetypeDefinition {
                schema_version: crate::archetype::CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
                id: "exact_match".into(),
                version: "1.0.0".into(),
                description: "exact".into(),
                sme_summary: "x".into(),
                goal_data: "data:0951".into(),
                goal_format: Some("format:3475".into()),
                atoms: vec![],
                slot_mappings: BTreeMap::new(),
                compose: vec![],
                slots: None,
                cross_dependencies: vec![],
                claim_boundary: None,
                project_class: "bioinformatics".into(),
                modality_hint: None,
                goal_kind_hint: None,
                preferred_container: None,
                runtime_baseline: Default::default(),
                cross_omics_modalities: vec![],
            },
        );
        archetypes.insert(
            "class_only".into(),
            ArchetypeDefinition {
                schema_version: crate::archetype::CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
                id: "class_only".into(),
                version: "1.0.0".into(),
                description: "class only".into(),
                sme_summary: "y".into(),
                goal_data: "data:9999".into(),
                goal_format: None,
                atoms: vec![],
                slot_mappings: BTreeMap::new(),
                compose: vec![],
                slots: None,
                cross_dependencies: vec![],
                claim_boundary: None,
                project_class: "bioinformatics".into(),
                modality_hint: None,
                goal_kind_hint: None,
                preferred_container: None,
                runtime_baseline: Default::default(),
                cross_omics_modalities: vec![],
            },
        );
        let reg = ArchetypeRegistry { archetypes };
        let m = reg.find_match_with_evidence("data:0951", Some("format:3475"), "bioinformatics");
        assert_eq!(m.len(), 2, "exact + class-only both score > 0");
        assert_eq!(m[0].archetype.id, "exact_match", "exact match ranks first");
        assert_eq!(
            m[0].evidence,
            ScoreEvidence {
                goal_data_exact: 3,
                goal_data_subtype: 0,
                goal_format_match: 2,
                project_class_match: 1,
                modality_match: 0,
                goal_kind_match: 0,
                total: 6,
            },
            "evidence breakdown matches scoring rule"
        );
        assert_eq!(m[1].archetype.id, "class_only");
        assert_eq!(m[1].evidence.total, 1, "class_only only project_class hit");
    }

    #[test]
    fn candidates_within_tie_window_surfaces_close_calls() {
        let mut archetypes = BTreeMap::new();
        // Two archetypes scoring 6 and 5 respectively against the same
        // target: 5/6 = 0.833 — outside the 5% window (0.95).
        // 6 vs 6 is a true tie; 6 vs 5.7 (96%) is inside the window.
        for (id, gd, fmt) in [
            ("a_exact_data_format", "data:0951", Some("format:3475")),
            ("b_exact_data_format", "data:0951", Some("format:3475")),
        ] {
            archetypes.insert(
                id.into(),
                ArchetypeDefinition {
                    schema_version: crate::archetype::CURRENT_ARCHETYPE_SCHEMA_VERSION.into(),
                    id: id.into(),
                    version: "1.0.0".into(),
                    description: "x".into(),
                    sme_summary: "y".into(),
                    goal_data: gd.into(),
                    goal_format: fmt.map(|f| f.into()),
                    atoms: vec![],
                    slot_mappings: BTreeMap::new(),
                    compose: vec![],
                    slots: None,
                    cross_dependencies: vec![],
                    claim_boundary: None,
                    project_class: "bioinformatics".into(),
                    modality_hint: None,
                    goal_kind_hint: None,
                    preferred_container: None,
                    runtime_baseline: Default::default(),
                    cross_omics_modalities: vec![],
                },
            );
        }
        let reg = ArchetypeRegistry { archetypes };
        let m = reg.find_match_with_evidence("data:0951", Some("format:3475"), "bioinformatics");
        let close = ArchetypeRegistry::candidates_within_tie_window(&m, 0.05);
        assert_eq!(close.len(), 2, "exact tie surfaces both candidates");
        let close_narrow = ArchetypeRegistry::candidates_within_tie_window(&m, 0.0);
        assert_eq!(
            close_narrow.len(),
            2,
            "zero window still picks up exact ties"
        );
    }

    #[test]
    fn rejects_unknown_project_class() {
        let tmp = tempfile::tempdir().unwrap();
        write_archetype(
            tmp.path(),
            "bad_class",
            r#"schema_version: "0.1"
id: bad_class
version: "1.0.0"
description: "Bad project class"
sme_summary: "x"
goal_data: data:3917
atoms:
  - atom_id: data_acquisition
project_class: bogus_class
"#,
        );
        let err = ArchetypeRegistry::load_from_dir(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("schema") || err.contains("project_class"),
            "expected project_class enum error, got: {}",
            err
        );
    }

    /// C23 — an archetype YAML carrying a `schema_version` the loader
    /// doesn't recognize must surface as a typed
    /// `schema_version_mismatch:` error BEFORE the JSON Schema
    /// validator's generic `const` failure. Callers wanting a
    /// SME-facing card promote via
    /// [`schema_version_mismatch_blocker`].
    #[test]
    fn schema_version_mismatch_surfaces_typed_error() {
        let tmp = tempfile::tempdir().unwrap();
        write_archetype(
            tmp.path(),
            "futurized",
            r#"schema_version: "0.99"
id: futurized
version: "1.0.0"
description: "Future archetype layout"
sme_summary: "future"
goal_data: data:0951
atoms:
  - atom_id: data_acquisition
project_class: bioinformatics
"#,
        );
        let err = ArchetypeRegistry::load_from_dir(tmp.path())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("schema_version_mismatch"),
            "expected typed schema_version_mismatch error, got: {}",
            err
        );
        assert!(err.contains("0.99"), "error must echo the found version");
        assert!(
            err.contains(CURRENT_ARCHETYPE_SCHEMA_VERSION),
            "error must echo the expected version"
        );

        let blocker = schema_version_mismatch_blocker("0.99");
        match blocker {
            BlockerKind::SchemaVersionMismatch {
                config_kind,
                expected,
                found,
            } => {
                assert_eq!(config_kind, "archetype_config");
                assert_eq!(expected, CURRENT_ARCHETYPE_SCHEMA_VERSION);
                assert_eq!(found, "0.99");
            }
            other => panic!("expected SchemaVersionMismatch, got {other:?}"),
        }
    }
}
