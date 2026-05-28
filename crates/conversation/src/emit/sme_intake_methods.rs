//! Post-emit CONTEXT.md guarantor that reads `session.intake_methods`
//! directly and appends a `## SME discovery decisions` section when
//! the core renderer's output didn't carry one.
//!
//! Renamed from `sme_fallback` because the behavior is
//! a required safety net, not an optional fallback. The function
//! always runs at emit time and is idempotent (no-op when the marker
//! is already present), but the SME-decision surface is load-bearing
//! for the agent's auto-approve path so missing it would silently
//! contradict an explicit SME directive.
//!
//! Why this lives outside `core`: the observed regression was that
//! session.intake_methods was non-empty but the core renderer walked
//! session.dag and found no tasks in `Completed{resolved_by=sme}`
//! state, so the section was skipped. That pointed at a session.dag
//! population issue the unit tests don't reproduce (the same tool-
//! call sequence in isolation works). Rather than refactor the
//! emit → DAG → render pipeline, this module guarantees the SME's
//! recorded method choices ALWAYS land in CONTEXT.md by consulting
//! the durable session field directly.
//!
//! Idempotent: does nothing when CONTEXT.md already has the
//! `## SME discovery decisions` header, so sessions where the core
//! renderer worked correctly stay byte-identical.

use crate::session::Session;
use anyhow::{Context, Result};
use std::path::Path;
use tokio::fs;

const MARKER: &str = "## SME discovery decisions";

pub(super) async fn append_sme_intake_methods_if_missing(
    session: &Session,
    output_dir: &Path,
) -> Result<()> {
    if session.intake_methods.0.is_empty() {
        return Ok(());
    }

    let ctx_path = output_dir.join("CONTEXT.md");
    let current = fs::read_to_string(&ctx_path)
        .await
        .with_context(|| format!("reading {}", ctx_path.display()))?;
    if current.contains(MARKER) {
        // Core renderer already emitted the section — leave it alone.
        return Ok(());
    }

    // Filter to stages that map to a `discover_<stage>` task in the
    // built DAG. A stage without a matching discover task would not
    // have been resolvable even if the core path had worked.
    type Entry = (String, String, Vec<(String, String)>);
    let mut rendered_ids: Vec<Entry> = Vec::new();
    if let Some(dag) = &session.dag {
        for (stage, resolution) in &session.intake_methods.0 {
            let discover_id = format!("discover_{}", stage);
            if !dag.tasks.contains_key(discover_id.as_str()) {
                continue;
            }
            let fields: Vec<(String, String)> = resolution
                .fields
                .iter()
                .map(|(k, v)| {
                    let rendered = match v {
                        serde_json::Value::String(s) => s.clone(),
                        _ => v.to_string(),
                    };
                    (k.clone(), rendered)
                })
                .collect();
            rendered_ids.push((discover_id, resolution.method.clone(), fields));
        }
    }
    if rendered_ids.is_empty() {
        return Ok(());
    }

    let mut append = String::new();
    append.push('\n');
    append.push_str(MARKER);
    append.push_str("\n\n");
    append.push_str("_Fallback-appended from session.intake_methods at emit time. Authoritative: these resolutions override any heuristic method scan._\n\n");
    for (task_id, method, fields) in &rendered_ids {
        append.push_str(&format!("### `{}`\n", task_id));
        if !method.is_empty() {
            append.push_str(&format!("**Method:** {}\n\n", method));
        }
        if !fields.is_empty() {
            append.push_str("**Structured fields:**\n");
            for (k, v) in fields {
                append.push_str(&format!("- `{}` = `{}`\n", k, v));
            }
            append.push('\n');
        }
    }

    let new_ctx = format!("{}{}", current, append);
    fs::write(&ctx_path, new_ctx)
        .await
        .with_context(|| format!("writing {}", ctx_path.display()))?;
    Ok(())
}
