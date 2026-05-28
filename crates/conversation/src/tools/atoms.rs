//! `list_atoms` — read-only inspection of the atom catalog.
//!
//! Returns a filtered summary of `config/stage-atoms/*.yaml` so the LLM
//! can:
//! 1. Check before proposing — avoid `propose_hypothesized_node`
//!    duplicates of atoms that already exist.
//! 2. Pick a real `candidate_tool` when the SME asks "what aligners
//!    do you support?" instead of relying on memorized knowledge.
//! 3. Explain coverage — answer "which modalities have dedicated
//!    DTU atoms?" by filtering on `role == operation` +
//!    `has_method_choice == true`.
//!
//! Tool spec lives in `tools/mod.rs::SPEC_LIST_ATOMS`, schema in
//! `tool_schemas.rs`, and the variant in `Tool::ListAtoms`. Sub-
//! millisecond on a 73-atom catalog; the registry is reloaded per
//! call (matches `get_taxonomy_info`'s discipline — no global cache).

use crate::errors::{ToolError, ToolResult};
use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom::AtomRole;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::Path;
// R6-U7: ts-rs derives removed across this module — these structs are
// LLM-tool argument/return shapes consumed only by the internal tool
// dispatcher; UI never reads them.

/// Default cap on returned rows. The full catalog is ~73 atoms; a
/// caller can override via `max_results` up to `MAX_RESULTS_HARD_CAP`.
const DEFAULT_MAX_RESULTS: usize = 100;
const MAX_RESULTS_HARD_CAP: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AtomRoleFilter {
    Operation,
    Discovery,
    Validation,
    Aggregator,
    Sizing,
    Selection,
    Calibration,
    Pilot,
    Adversarial,
    Monitor,
}

impl AtomRoleFilter {
    fn matches(&self, role: &AtomRole) -> bool {
        matches!(
            (self, role),
            (AtomRoleFilter::Operation, AtomRole::Operation)
                | (AtomRoleFilter::Discovery, AtomRole::Discovery)
                | (AtomRoleFilter::Validation, AtomRole::Validation)
                | (AtomRoleFilter::Aggregator, AtomRole::Aggregator)
                | (AtomRoleFilter::Sizing, AtomRole::Sizing)
                | (AtomRoleFilter::Selection, AtomRole::Selection)
                | (AtomRoleFilter::Calibration, AtomRole::Calibration)
                | (AtomRoleFilter::Pilot, AtomRole::Pilot)
                | (AtomRoleFilter::Adversarial, AtomRole::Adversarial)
                | (AtomRoleFilter::Monitor, AtomRole::Monitor)
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ListAtomsArgs {
    /// Filter to atoms referenced by the canonical archetype for the
    /// given modality (e.g. `"long_read_rnaseq"`). Atoms themselves
    /// don't carry a `modality_hint` field — the archetype does. The
    /// handler resolves the archetype whose `modality_hint` matches
    /// and intersects its atom-id list with the atom registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modality: Option<String>,
    /// Filter to atoms with the given role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AtomRoleFilter>,
    /// Filter to atoms that defer their tool choice to a discovery
    /// atom (i.e. `method_choice.deferred_to` is set).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_method_choice: Option<bool>,
    /// Filter to atoms whose primary output EDAM data IRI matches.
    /// e.g. `"data:0951"` (effect-size + adjusted p-value).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produces_edam_data: Option<String>,
    /// Cap on returned rows. Default `DEFAULT_MAX_RESULTS` (100);
    /// hard-capped at `MAX_RESULTS_HARD_CAP` (500). Set high to
    /// dump the full catalog; lower it when the LLM only needs a
    /// few representative atoms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub(super) struct AtomSummary {
    pub id: String,
    /// String form of `AtomRole` — lowercase snake_case.
    pub role: String,
    pub edam_operation: String,
    /// Optional on the underlying `AtomDefinition`; surfaced as an
    /// empty string when absent so the JSON shape stays uniform.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub edam_data: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub edam_format: String,
    /// Upstream atoms this atom consumes (by id). Ordering matches
    /// the YAML's `depends_on:` list.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Candidate tool ids from `attributes.candidate_tools`. Empty
    /// when the atom doesn't expose runtime tool selection.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub candidate_tools: Vec<String>,
    /// Set when `method_choice.deferred_to` points at a discovery
    /// atom (e.g. `discover_dtu_method`). Surfaces the discovery
    /// wrapper so the LLM can chain proposals correctly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method_choice_deferred_to: Option<String>,
}

pub(super) fn list_atoms(args: &ListAtomsArgs, config_dir: &Path) -> ToolResult {
    let atom_dir = config_dir.join("stage-atoms");
    let registry = match AtomRegistry::load_cached(&atom_dir) {
        Ok(r) => r,
        Err(e) => {
            return ToolResult::err(ToolError::ValidationFailure {
                reason: format!("could not load atom registry: {}", e),
                valid_alternatives: vec![],
                hint: "Verify config/stage-atoms/ is reachable.".into(),
            });
        }
    };

    let total_in_registry = registry.iter().count();

    // Resolve modality → atom-id allowlist via the archetype registry,
    // since atoms themselves don't carry a `modality_hint`. None means
    // no modality filter (every atom passes); Some(set) restricts to
    // the canonical archetype's atom list for the requested modality.
    let modality_atom_ids: Option<BTreeSet<String>> = match args.modality.as_deref() {
        None => None,
        Some(want_modality) => {
            let arch_dir = config_dir.join("archetypes");
            match ArchetypeRegistry::load_cached(&arch_dir) {
                Ok(arch_reg) => {
                    let matched = arch_reg
                        .iter()
                        .find(|(_, a)| a.modality_hint.as_deref() == Some(want_modality));
                    match matched {
                        Some((_, archetype)) => Some(
                            archetype
                                .atoms
                                .iter()
                                .map(|a| a.atom_id.to_string())
                                .collect(),
                        ),
                        None => {
                            // Unknown modality → empty allowlist (zero
                            // matches). The LLM will see matched=0 and
                            // total_in_registry > 0, signalling "no
                            // archetype found for that modality".
                            Some(BTreeSet::new())
                        }
                    }
                }
                Err(e) => {
                    return ToolResult::err(ToolError::ValidationFailure {
                        reason: format!("could not load archetype registry: {}", e),
                        valid_alternatives: vec![],
                        hint: "Modality filter requires config/archetypes/ to be reachable.".into(),
                    });
                }
            }
        }
    };

    // A `max_results: Some(0)` from a faulty caller previously returned
    // an empty list, which masks the result truncation as "no atoms
    // matched". Treat 0 the same as an absent value so the SME-facing
    // tool always returns at least the default page.
    let cap = args
        .max_results
        .map(|n| n as usize)
        .filter(|n| *n > 0)
        .map(|n| n.min(MAX_RESULTS_HARD_CAP))
        .unwrap_or(DEFAULT_MAX_RESULTS);

    let mut all_matched: Vec<AtomSummary> = registry
        .iter()
        .filter(|(_, atom)| {
            if let Some(allow) = &modality_atom_ids {
                if !allow.contains(&atom.id) {
                    return false;
                }
            }
            if let Some(want_role) = &args.role {
                if !want_role.matches(&atom.role) {
                    return false;
                }
            }
            if let Some(want_mc) = args.has_method_choice {
                let has = atom.method_choice.is_some();
                if has != want_mc {
                    return false;
                }
            }
            if let Some(want_edam) = args.produces_edam_data.as_deref() {
                if atom.edam_data.as_deref() != Some(want_edam) {
                    return false;
                }
            }
            true
        })
        .map(|(_, atom)| AtomSummary {
            id: atom.id.clone(),
            role: role_to_snake(&atom.role).into(),
            edam_operation: atom.edam_operation.clone(),
            edam_data: atom.edam_data.clone().unwrap_or_default(),
            edam_format: atom.edam_format.clone().unwrap_or_default(),
            depends_on: atom.depends_on.clone(),
            candidate_tools: candidate_tools_from_attributes(&atom.attributes),
            method_choice_deferred_to: atom.method_choice.as_ref().map(|m| m.deferred_to.clone()),
        })
        .collect();

    let matched = all_matched.len();
    let truncated = matched > cap;
    if truncated {
        all_matched.truncate(cap);
    }

    let body = serde_json::json!({
        "atoms": all_matched,
        "total_in_registry": total_in_registry,
        "matched": matched,
        "truncated": truncated,
    });
    ToolResult::ok(body)
}

fn role_to_snake(r: &AtomRole) -> &'static str {
    match r {
        AtomRole::Operation => "operation",
        AtomRole::Discovery => "discovery",
        AtomRole::Validation => "validation",
        AtomRole::Aggregator => "aggregator",
        AtomRole::Sizing => "sizing",
        AtomRole::Selection => "selection",
        AtomRole::Calibration => "calibration",
        AtomRole::Pilot => "pilot",
        AtomRole::Adversarial => "adversarial",
        AtomRole::Monitor => "monitor",
    }
}

/// Extract the `candidate_tools` array from an atom's typed-but-loose
/// `attributes` map. The schema is `Vec<String>` by convention; if a
/// future atom encodes the list differently the helper returns an
/// empty vector rather than panicking (the LLM can fall back to
/// reading raw YAML if it needs richer data).
fn candidate_tools_from_attributes(
    attrs: &std::collections::BTreeMap<String, serde_json::Value>,
) -> Vec<String> {
    let Some(value) = attrs.get("candidate_tools") else {
        return Vec::new();
    };
    let Some(arr) = value.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect()
}
