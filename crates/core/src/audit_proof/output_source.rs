//! Single source of truth for "what counts as an analytical output."
//!
//! # Why this module exists
//!
//! The Evidence (V) sub-graph and Invariant 3 (`evidence_coverage`) both range
//! over the analysis's *outputs*. For the reader (Inv 3) and the writer (the V
//! projection) to AGREE, they must derive that output set the same way. This
//! module is that shared derivation.
//!
//! # The output source
//!
//! Outputs are the RO-Crate `@graph` **output entities** — the
//! `schema:Image`/`ImageObject` figure entities (declared figure obligations at
//! emit; produced figure files post-execution) plus any `Dataset`/`File` entity
//! rooted under `runtime/outputs/`. The spec (`v0.2.md` §5.4) names exactly
//! these as the RO-Crate carriers of V evidence:
//!
//! > The RO-Crate `@graph` ALSO carries V entities as `dcat:Dataset` /
//! > `bioschemas:Dataset` / `schema:Image` types for ecosystem
//! > interoperability.
//!
//! Both emit paths agree on these entities (the CLI path and the production
//! conversation path emit the same `ImageObject` figure obligations), so V and
//! Inv 3 read the same source whether or not the agent has executed yet.
//!
//! # Why NOT `proofs.jsonl`
//!
//! `proofs.jsonl` is the Execution (E) sub-graph: producer→consumer
//! `EdgeContract` rows. The CLI emit path additionally tags each with a
//! `computed_from: "workflow:<dep>"` field — but that names a DAG dependency
//! NODE, not a produced file, so it is not an analytical output. The production
//! conversation path emits BARE `EdgeContract`s with no `computed_from` at all,
//! so keying outputs on `proofs.jsonl` left V empty and Inv 3 with nothing to
//! range over on real packages (the D.5.1 key-mismatch).
//!
//! A `proofs.jsonl` row whose `computed_from`/`produces` names a *real* output
//! path (e.g. a hand-built fixture's V `computed_from` row, or a future writer
//! that records produced files there) is still honored as a complementary
//! source — only the `workflow:*` dependency-node form is rejected.

use serde_json::Value;

/// Root prefix under which produced analytical artifacts live.
const OUTPUTS_ROOT: &str = "runtime/outputs/";

/// One analytical output, derived from the real-output source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalyticalOutput {
    /// The output's path / identifier (the `@id` of the RO-Crate entity, or the
    /// `computed_from`/`produces` path of a real-path proofs row), with any
    /// `#fragment` already stripped.
    pub path: String,
    /// The spec V node kind this output maps to (`Figure`/`Table`/`File`).
    pub kind: OutputKind,
    /// The producing task id, when derivable from a `runtime/outputs/<task>/…`
    /// path. Used to draw the V `computed_from` edge to the E step.
    pub producer_task: Option<String>,
}

/// The spec V node-type (`v0.2.md` §5.4 closed set member) an output maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    /// `schema:Image` / `ImageObject` → V `Figure`.
    Figure,
    /// Tabular result artifact → V `Table`.
    Table,
    /// Any other produced file → V `File`.
    File,
}

/// True when `@type` (a string or array) names `ImageObject`/`schema:Image`.
fn type_is_image(ty: &Value) -> bool {
    let is = |s: &str| s == "ImageObject" || s == "schema:Image";
    match ty {
        Value::String(s) => is(s),
        Value::Array(arr) => arr.iter().filter_map(Value::as_str).any(is),
        _ => false,
    }
}

/// True when `@type` (a string or array) names `Dataset`/`File`.
fn type_is_dataset_or_file(ty: &Value) -> bool {
    let is = |s: &str| matches!(s, "Dataset" | "File" | "dcat:Dataset");
    match ty {
        Value::String(s) => is(s),
        Value::Array(arr) => arr.iter().filter_map(Value::as_str).any(is),
        _ => false,
    }
}

/// `runtime/outputs/<task>/…` → `Some("<task>")`; otherwise `None`.
fn producer_task_from_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix(OUTPUTS_ROOT)?;
    let task = rest.split('/').next()?;
    if task.is_empty() {
        None
    } else {
        Some(task.to_string())
    }
}

/// True iff `a` and `b` name the same basename UNDER THE SAME
/// `runtime/outputs/<task>/` subtree. This is the ONLY case in which a basename
/// fallback may bridge the direct-child-vs-nested-table reconstruction gap
/// (a claim's reconstructed `…/<task>/de.tsv` vs the registered
/// `…/<task>/tables/de.tsv`). It deliberately does NOT match a cross-task,
/// wrong-directory reference (`…/<taskA>/de.tsv` vs `…/<taskB>/de.tsv`), which
/// stays a violation. Shared by Inv 3 (`evidence_coverage`) and Inv 5
/// (`cross_graph_integrity`) so the two never disagree about the same C→V link.
/// A `#fragment` on either side is ignored.
pub fn same_task_basename_match(a: &str, b: &str) -> bool {
    fn strip(p: &str) -> &str {
        p.split('#').next().unwrap_or(p)
    }
    let (a, b) = (strip(a), strip(b));
    match (producer_task_from_path(a), producer_task_from_path(b)) {
        (Some(ta), Some(tb)) if ta == tb => a.rsplit('/').next() == b.rsplit('/').next(),
        _ => false,
    }
}

/// Classify an output path into a V node kind by its extension / location.
fn kind_for_path(path: &str, was_image_entity: bool) -> OutputKind {
    if was_image_entity
        || path.ends_with(".png")
        || path.ends_with(".pdf")
        || path.ends_with(".svg")
    {
        OutputKind::Figure
    } else if path.ends_with(".csv") || path.ends_with(".tsv") || path.ends_with(".parquet") {
        OutputKind::Table
    } else {
        OutputKind::File
    }
}

/// True when `path` is a bogus DAG-dependency-node identifier (the CLI emit
/// path's `computed_from: "workflow:<dep>"` form) rather than a produced file.
fn is_dependency_node_ref(path: &str) -> bool {
    path.starts_with("workflow:")
}

/// Strip any `#fragment` so references resolve against the bare output id.
fn strip_fragment(s: &str) -> &str {
    s.split('#').next().unwrap_or(s)
}

/// Derive the analytical outputs for a package from the single shared source:
/// the RO-Crate `@graph` output entities, plus any real-path
/// `computed_from`/`produces` proofs rows (excluding `workflow:*` dependency
/// edges). Deterministically ordered by path; de-duplicated by path (an entity
/// also named in proofs is counted once, RO-Crate kind winning).
pub fn analytical_outputs(output_entities: &[Value], proofs: &[Value]) -> Vec<AnalyticalOutput> {
    use std::collections::BTreeMap;
    let mut by_path: BTreeMap<String, AnalyticalOutput> = BTreeMap::new();

    // 1. RO-Crate `@graph` output entities (the primary, both-paths-agree
    //    source). ImageObject everywhere; Dataset/File only under
    //    `runtime/outputs/` so config dirs like `policies/` are excluded.
    for e in output_entities {
        let Some(id) = e.get("@id").and_then(Value::as_str) else {
            continue;
        };
        let path = strip_fragment(id).to_string();
        if path.is_empty() {
            continue;
        }
        let ty = e.get("@type").unwrap_or(&Value::Null);
        let is_image = type_is_image(ty);
        let is_output_path = path.starts_with(OUTPUTS_ROOT);
        let keep = is_image || (type_is_dataset_or_file(ty) && is_output_path);
        if !keep {
            continue;
        }
        let kind = kind_for_path(&path, is_image);
        let producer_task = producer_task_from_path(&path);
        by_path.entry(path.clone()).or_insert(AnalyticalOutput {
            path,
            kind,
            producer_task,
        });
    }

    // 2. Real-path proofs `computed_from`/`produces` rows. Honored as a
    //    complementary source (hand-built fixtures' V rows, or a future
    //    produced-file writer); `workflow:*` dependency-node refs rejected.
    for p in proofs {
        let Some(raw) = p
            .get("computed_from")
            .or_else(|| p.get("produces"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if is_dependency_node_ref(raw) {
            continue;
        }
        let path = strip_fragment(raw).to_string();
        if path.is_empty() {
            continue;
        }
        let kind = kind_for_path(&path, false);
        let producer_task = producer_task_from_path(&path);
        by_path.entry(path.clone()).or_insert(AnalyticalOutput {
            path,
            kind,
            producer_task,
        });
    }

    by_path.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_task_basename_match_bridges_intra_task_only() {
        // Nested-table reconstruction gap WITHIN one task → matches.
        assert!(same_task_basename_match(
            "runtime/outputs/de/de.tsv",
            "runtime/outputs/de/tables/de.tsv"
        ));
        // A `#fragment` on either side is ignored.
        assert!(same_task_basename_match(
            "runtime/outputs/de/de.tsv#row-3",
            "runtime/outputs/de/de.tsv"
        ));
        // Cross-task, wrong-directory ref → does NOT match (the finding #2 masking).
        assert!(!same_task_basename_match(
            "runtime/outputs/final_reporting/de_results.tsv",
            "runtime/outputs/differential_expression/de_results.tsv"
        ));
        // Same task, different basename → no match.
        assert!(!same_task_basename_match(
            "runtime/outputs/de/a.tsv",
            "runtime/outputs/de/b.tsv"
        ));
        // Non-`runtime/outputs/` paths never resolve by basename fallback.
        assert!(!same_task_basename_match("results/tables/de.csv", "results/other/de.csv"));
    }

    #[test]
    fn image_entity_anywhere_is_a_figure() {
        let outs = analytical_outputs(
            &[
                json!({"@id":"runtime/outputs/de/figures/volcano.png","@type":["File","ImageObject"]}),
            ],
            &[],
        );
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].kind, OutputKind::Figure);
        assert_eq!(outs[0].producer_task.as_deref(), Some("de"));
    }

    #[test]
    fn dataset_only_counts_under_outputs_root() {
        let outs = analytical_outputs(
            &[
                json!({"@id":"policies/","@type":"Dataset"}),
                json!({"@id":"runtime/outputs/de/de_results.csv","@type":["File","Dataset"]}),
            ],
            &[],
        );
        assert_eq!(outs.len(), 1, "policies/ config dir is not an output");
        assert_eq!(outs[0].kind, OutputKind::Table);
    }

    #[test]
    fn workflow_dependency_edges_are_rejected() {
        let outs = analytical_outputs(
            &[],
            &[json!({"id":"workflow:de","computed_from":"workflow:data_acquisition"})],
        );
        assert!(outs.is_empty(), "workflow:* is a dep node, not an output");
    }

    #[test]
    fn real_path_proofs_row_is_honored() {
        let outs = analytical_outputs(
            &[],
            &[json!({"id":"fig_qc","computed_from":"data/figures/fig_qc.png"})],
        );
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].path, "data/figures/fig_qc.png");
        assert_eq!(outs[0].kind, OutputKind::Figure);
    }

    #[test]
    fn dedup_by_path_rocrate_wins() {
        let outs = analytical_outputs(
            &[json!({"@id":"runtime/outputs/de/x.png","@type":["ImageObject"]})],
            &[json!({"computed_from":"runtime/outputs/de/x.png"})],
        );
        assert_eq!(outs.len(), 1);
        assert_eq!(outs[0].kind, OutputKind::Figure);
    }

    #[test]
    fn deterministic_ordering_by_path() {
        let outs = analytical_outputs(
            &[
                json!({"@id":"runtime/outputs/b/fig.png","@type":["ImageObject"]}),
                json!({"@id":"runtime/outputs/a/fig.png","@type":["ImageObject"]}),
            ],
            &[],
        );
        assert_eq!(outs[0].path, "runtime/outputs/a/fig.png");
        assert_eq!(outs[1].path, "runtime/outputs/b/fig.png");
    }
}
