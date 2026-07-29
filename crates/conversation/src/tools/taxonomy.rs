//! taxonomy inspection.
//!
//! `get_taxonomy_info` summarizes the stage list for a given modality.
//! The legacy `config/stage-taxonomies/*.yaml` is retired
//! loader; this tool now reads from the archetype registry instead.
//! Future refactor may rename the tool to `get_archetype_info` (out
//! of scope for B4 — that's a plan amendment to the closed Tool enum).

use crate::errors::{ToolError, ToolResult};
use std::collections::BTreeSet;
use std::path::Path;

pub(super) fn get_taxonomy_info(modality_id: &str, config_dir: &Path) -> ToolResult {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
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
    // Accept the exact archetype id as well as its primary modality.
    // Both identifiers are exposed to the model during intake, and the
    // tool reads the archetype registry rather than a modality catalog.
    // Exact ids take precedence when several archetypes share a
    // modality; modality lookup retains deterministic id order.
    let matched = registry
        .get(modality_id)
        .map(|archetype| (modality_id, archetype))
        .or_else(|| {
            registry
                .iter()
                .find(|(_id, archetype)| archetype.modality_hint.as_deref() == Some(modality_id))
                .map(|(id, archetype)| (id.as_str(), archetype))
        });
    let Some((_id, archetype)) = matched else {
        let valid_alternatives = registry
            .iter()
            .flat_map(|(id, archetype)| {
                [
                    id.as_str(),
                    archetype.modality_hint.as_deref().unwrap_or_default(),
                ]
            })
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        return ToolResult::err(ToolError::ValidationFailure {
            reason: format!(
                "no archetype or primary modality matches '{}' in config/archetypes/",
                modality_id
            ),
            valid_alternatives,
            hint: "Use an archetype id or a primary modality id returned by the intake tools."
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
