//! taxonomy inspection.
//!
//! `get_taxonomy_info` summarizes the stage list for a given modality.
//! The legacy `config/stage-taxonomies/*.yaml` is retired
//! loader; this tool now reads from the archetype registry instead.
//! Future refactor may rename the tool to `get_archetype_info` (out
//! of scope for B4 — that's a plan amendment to the closed Tool enum).

use crate::errors::{ToolError, ToolResult};
use std::path::Path;

pub(super) fn get_taxonomy_info(modality_id: &str, config_dir: &Path) -> ToolResult {
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    let archetype_dir = config_dir.join("archetypes");
    let registry = match ArchetypeRegistry::load_cached(&archetype_dir) {
        Ok(r) => r,
        Err(e) => {
            return ToolResult::err(ToolError::ValidationFailure {
                reason: format!("could not load archetype registry: {}", e),
                valid_alternatives: vec![],
                hint: "Verify config/archetypes/ is reachable.".into(),
            });
        }
    };
    // Search for an archetype whose primary modality matches the
    // requested modality id. Multi-modality archetypes (cross_omics_*)
    // may carry the modality as a secondary; we don't surface those
    // unprompted to the LLM since the SME's "modality" here is the
    // primary.
    let matched = registry
        .iter()
        .find(|(_id, a)| a.modality_hint.as_deref() == Some(modality_id));
    let Some((_id, archetype)) = matched else {
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!(
                "no archetype matches modality '{}' in config/archetypes/",
                modality_id
            ),
            valid_alternatives: vec![],
            hint: "Check that the modality id matches one in modality-keywords.yaml \
                   and that an archetype declares modality: <id>."
                .into(),
        });
    };
    let stages: Vec<serde_json::Value> = archetype
        .atoms
        .iter()
        .map(|a| {
            let id = a.alias.clone().unwrap_or_else(|| a.atom_id.to_string());
            serde_json::json!({
                "id": id,
                "class": "operation",
                "description": format!("Atom {} from archetype {}", a.atom_id, archetype.id),
                "discovery": "none",
                "depends_on": a.depends_on.clone(),
            })
        })
        .collect();
    let body = serde_json::json!({
        "id": archetype.id,
        "domain": "computational biology",
        "description": archetype.description,
        "claim_boundary": serde_json::Value::Null,
        "policies": serde_json::Value::Null,
        "stages": stages,
    });
    ToolResult::ok(body)
}
