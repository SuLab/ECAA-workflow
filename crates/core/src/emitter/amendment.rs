//! Amendment lineage emission: links a derived package back to its
//! parent via `policies/amendment-lineage.json`, a PROV-O
//! `wasDerivedFrom` predicate on the root Dataset, and an
//! `UpdateAction` entity in the RO-Crate graph.
//!
//! Caller (`emit_package`) decides when to invoke each helper; this
//! module is purely the amendment-policy renderer + RO-Crate patcher.

use super::write_policy;
use super::AmendContext;
use anyhow::Result;
use std::path::Path;

/// Identifiers pulled from the parent package so the amendment's
/// RO-Crate + lineage policy can point at it unambiguously.
#[derive(Debug, Clone)]
pub(super) struct ParentLink {
    /// The parent's root `Dataset.@id` (typically `./` for local packages).
    pub(super) root_id: String,
    /// The parent's workflow UUID from `WORKFLOW.json`. Used as the
    /// `parent_package_id` in the lineage policy so distinct packages
    /// don't collide on the literal `./` root id.
    pub(super) workflow_id: String,
    /// Absolute path to the parent package dir — surfaced in the policy
    /// so operators can find the prior emission on disk.
    pub(super) path: std::path::PathBuf,
}

pub(super) fn read_parent_link(parent_dir: &Path) -> Option<ParentLink> {
    let meta_path = parent_dir.join("ro-crate-metadata.json");
    let workflow_path = parent_dir.join("WORKFLOW.json");

    // Parent's root @id. Soft-fail if metadata is missing / malformed.
    let root_id = match std::fs::read(&meta_path) {
        Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
            Ok(value) => value
                .get("@graph")
                .and_then(|g| g.as_array())
                .and_then(|graph| {
                    graph
                        .iter()
                        .find(|e| e.get("@type").and_then(|t| t.as_str()) == Some("Dataset"))
                        .and_then(|e| e.get("@id").and_then(|v| v.as_str()))
                        .map(|s| s.to_string())
                })
                .unwrap_or_else(|| "./".to_string()),
            Err(e) => {
                eprintln!(
                    "warning: amend_from parent metadata at {} is unparseable ({}); emitting without lineage",
                    meta_path.display(),
                    e
                );
                return None;
            }
        },
        Err(e) => {
            eprintln!(
                "warning: amend_from parent metadata at {} unreadable ({}); emitting without lineage",
                meta_path.display(),
                e
            );
            return None;
        }
    };

    let workflow_id = std::fs::read(&workflow_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|v| {
            v.get("workflow_id")
                .and_then(|s| s.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default();

    Some(ParentLink {
        root_id,
        workflow_id,
        path: parent_dir.to_path_buf(),
    })
}

pub(super) fn emit_amendment_lineage_policy(
    package_dir: &Path,
    ctx: &AmendContext,
    parent: Option<&ParentLink>,
    clock: &dyn crate::clock::Clock,
) -> Result<()> {
    let parent_obj = parent.map(|p| {
        serde_json::json!({
            "parent_package_id": p.workflow_id,
            "parent_root_id": p.root_id,
            "parent_path": p.path.to_string_lossy(),
        })
    });
    // `created_at` enters the BagIt manifest, so emit-pipeline callers
    // must thread a `FrozenClock` derived from the intake hash so two
    // emits of the same intake produce byte-identical
    // `amendment-lineage.json`.
    let payload = serde_json::json!({
        "parent": parent_obj,
        "amended_stage": ctx.amended_stage,
        "invalidated_tasks": ctx.invalidated_tasks,
        "amendment_reason": ctx.reason,
        "created_at": clock.now_rfc3339(),
    });
    write_policy(package_dir, "amendment-lineage", &payload)
}

/// Patch the RO-Crate with branch lineage only: a single
/// `prov:wasDerivedFrom` edge on the root Dataset + a parent Dataset
/// entry. No UpdateAction — branches are copies of the parent at a
/// point in time, not edits to it. Symmetric to
/// `patch_ro_crate_with_amendment` for the wasDerivedFrom + parent
/// entity pair; intentionally omits the UpdateAction that's specific
/// to amendments.
pub(super) fn patch_ro_crate_with_branch(metadata: &mut serde_json::Value, parent: &ParentLink) {
    let Some(graph) = metadata.get_mut("@graph").and_then(|g| g.as_array_mut()) else {
        return;
    };

    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
    {
        let parent_ref =
            serde_json::json!({"@id": format!("branch-parent:{}", parent.workflow_id)});
        if let Some(obj) = root.as_object_mut() {
            obj.insert("prov:wasDerivedFrom".to_string(), parent_ref);
        }
    }

    let parent_id = format!("branch-parent:{}", parent.workflow_id);
    let already_parent = graph
        .iter()
        .any(|e| e.get("@id").and_then(|v| v.as_str()) == Some(parent_id.as_str()));
    if !already_parent {
        graph.push(serde_json::json!({
            "@id": parent_id,
            "@type": "Dataset",
            "name": format!("Branched-from package {}", parent.workflow_id),
            "identifier": parent.workflow_id,
            "url": parent.path.to_string_lossy(),
        }));
    }
}

pub(super) fn patch_ro_crate_with_amendment(
    metadata: &mut serde_json::Value,
    ctx: &AmendContext,
    parent: &ParentLink,
) {
    let Some(graph) = metadata.get_mut("@graph").and_then(|g| g.as_array_mut()) else {
        return;
    };

    // Attach prov:wasDerivedFrom to the root Dataset (./) — this is the
    // standard W3C PROV-O predicate for amendment chains.
    if let Some(root) = graph
        .iter_mut()
        .find(|e| e.get("@id").and_then(|v| v.as_str()) == Some("./"))
    {
        let parent_ref =
            serde_json::json!({"@id": format!("amendment-parent:{}", parent.workflow_id)});
        if let Some(obj) = root.as_object_mut() {
            obj.insert("prov:wasDerivedFrom".to_string(), parent_ref);
        }
    }

    // Register the parent as a referenced Dataset entity + an UpdateAction
    // that records the amendment itself.
    let parent_id = format!("amendment-parent:{}", parent.workflow_id);
    let already_parent = graph
        .iter()
        .any(|e| e.get("@id").and_then(|v| v.as_str()) == Some(parent_id.as_str()));
    if !already_parent {
        graph.push(serde_json::json!({
            "@id": parent_id,
            "@type": "Dataset",
            "name": format!("Parent package {}", parent.workflow_id),
            "identifier": parent.workflow_id,
            "url": parent.path.to_string_lossy(),
        }));
    }

    // Include the parent workflow_id in the UpdateAction's `@id` so a
    // multi-amend chain (parent → child amends `qc` → grandchild also
    // amends `qc`) doesn't collide on `#amendment-action-qc`. The id
    // becomes a stable composite of (stage, parent-version); each
    // child's RO-Crate carries its own UpdateAction without
    // overwriting a sibling's. Falls back to the bare stage when the
    // parent has no workflow_id (pre-stamping era / empty manifest).
    let action_id = if parent.workflow_id.is_empty() {
        format!("#amendment-action-{}", ctx.amended_stage)
    } else {
        format!(
            "#amendment-action-{}-from-{}",
            ctx.amended_stage, parent.workflow_id
        )
    };
    let action = serde_json::json!({
        "@id": action_id,
        "@type": "UpdateAction",
        "name": format!("Amendment: swap method for {}", ctx.amended_stage),
        "object": {"@id": parent_id},
        "result": {"@id": "./"},
        "description": ctx.reason.clone().unwrap_or_else(|| format!(
            "SME amended the method for stage '{}'. Downstream tasks were invalidated.",
            ctx.amended_stage
        )),
        "actionStatus": "https://schema.org/CompletedActionStatus",
        "targetCollection": ctx
            .invalidated_tasks
            .iter()
            .map(|id| serde_json::json!({"@id": format!("#step-{}", id)}))
            .collect::<Vec<_>>(),
    });
    graph.push(action);
}
