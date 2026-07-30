//! Observed-read reconciliation (design §5.2 C5).
//!
//! Reconciliation compares what a task *actually read* against what
//! the composed DAG *declared* it would read (the `EdgeContract`s
//! whose `to_node` is the task). Only typed or adapter-mediated data-flow
//! edges authorize reads; ordering edges do not. A read whose path lives
//! under a declared producer's output directory
//! (`runtime/outputs/<from_node>/`) and, when named, whose
//! `declared_port` matches that same edge resolves the producer as the
//! authoritative source. This disambiguates a mutually-exclusive one-of
//! input group (e.g. differential-expression `raw_counts` /
//! `normalized_counts`) down to the single edge the task consumed. A read
//! that matches no declared producer-and-port edge is a divergence: either
//! the task read outside its declared input contract, or the graph is wrong.
//!
//! This module is pure and synchronous — no I/O, no clock, no
//! `HashMap`. Capturing the observed reads (harness-side) and folding
//! the reconciled, observed graph back into the emitted RO-Crate are
//! later tasks; this module only supplies the types and the decision
//! function they share.

use super::super::workflow_contracts::edge::EdgeContract;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Root prefix under which produced analytical artifacts live.
///
/// Mirrors `crate::audit_proof::output_source`'s `runtime/outputs/`
/// convention — the two modules independently range over the same
/// on-disk layout (produced outputs vs. observed reads of them) and
/// must agree on where a task's outputs live.
const OUTPUTS_ROOT: &str = "runtime/outputs/";

/// One file a task was observed to read, captured at the harness
/// dispatch site (later task). `declared_port` is the input port the
/// task's own read manifest attributes the read to, when known. Reconciliation
/// requires the claimed port and producer path to resolve the same edge.
///
/// `Serialize`/`Deserialize` so the harness can carry a task's observed
/// reads on its `InvocationRecord` (`runtime/invocations.jsonl`,
/// design §5.2 C5) and `crates/conversation/src/emit/ro_crate.rs` can
/// read them back without depending on the harness crate (conversation
/// never links harness — see CLAUDE.md crate layering).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservedRead {
    /// The task that performed the read.
    pub task_id: String,
    /// The input port the read is claimed to satisfy, if the read
    /// manifest names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_port: Option<String>,
    /// The file path that was read.
    pub path: String,
}

/// Outcome of reconciling one [`ObservedRead`] against a task's
/// declared producer edges.
///
/// `#[non_exhaustive]` per the workspace's public wire-enum contract
/// (`CLAUDE.md` — adding a variant is minor, not breaking).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReconVerdict {
    /// The read path and any claimed port resolve the same declared data-flow
    /// edge. `authoritative_edge` is `(from_node, to_node)` of that edge; for
    /// a mutually-exclusive one-of group, it is the consumed member.
    Match {
        authoritative_edge: (String, String),
    },
    /// The read path does not live under any declared producer's
    /// output directory for this task. `declared_producer` names the
    /// producer the declared graph attributes to the read's own
    /// `declared_port` (when the read named one and that port is
    /// declared) — i.e. what the graph *said* should have produced
    /// this data, even though the observed read came from somewhere
    /// else. `None` when the read named no port, or named a port with
    /// no declared producer.
    Divergent {
        read_path: String,
        declared_producer: Option<String>,
    },
    /// The task has no declared producer edges at all, so there is
    /// nothing to reconcile this read against — not a violation
    /// (design §5.2 "systemic scope": some tasks legitimately read
    /// beyond a modeled port set today), just an unmodeled read.
    Untracked,
}

/// One divergent read discovered while reconciling a *package's* observed
/// reads against its declared per-edge graph
/// (`crate::ro_crate::reconcile_ro_crate_edges`). Unlike [`ReconVerdict::Divergent`]
/// — which is scoped to the single task passed to [`reconcile`] and so never
/// carries a task id — a package-level reconciliation pass folds every
/// task's verdicts together, so the owning `task_id` has to travel with the
/// record once it leaves that per-task scope. Consumed by
/// `crates/conversation/src/emit/ro_crate.rs::patch_ro_crate_metadata`'s
/// caller to transition the offending task to
/// `BlockerKind::ProvenanceDivergence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DivergenceRecord {
    /// The task whose observed read diverged from its declared producers.
    pub task_id: String,
    /// The file path that was read.
    pub read_path: String,
    /// The producer the declared graph attributed to the read's own
    /// claimed input port, when the read named one and that port is
    /// declared. Mirrors [`ReconVerdict::Divergent`]'s field of the same
    /// name.
    pub declared_producer: Option<String>,
}

/// True when `path` lives under the given producer task's declared
/// output directory (`runtime/outputs/<producer_task>/…`).
fn path_under_producer_output(path: &str, producer_task: &str) -> bool {
    let mut prefix = String::with_capacity(OUTPUTS_ROOT.len() + producer_task.len() + 1);
    prefix.push_str(OUTPUTS_ROOT);
    prefix.push_str(producer_task);
    prefix.push('/');
    path.starts_with(&prefix)
}

/// Reconcile `task_id`'s observed reads against its declared typed or
/// adapter-mediated producer edges.
///
/// Reads not addressed to `task_id` are ignored (callers may pass the
/// full declared-edge / observed-read sets for a package; this
/// function scopes itself to one task). One [`ReconVerdict`] is
/// returned per matching read, in the input reads' order.
pub fn reconcile(
    declared: &[EdgeContract],
    reads: &[ObservedRead],
    task_id: &str,
) -> Vec<ReconVerdict> {
    let declared_for_task: Vec<&EdgeContract> = declared
        .iter()
        .filter(|e| {
            e.to_node == task_id
                && matches!(
                    e.kind,
                    crate::workflow_contracts::edge::EdgeKind::TypedDataFlow
                        | crate::workflow_contracts::edge::EdgeKind::AdapterMediated
                )
        })
        .collect();

    reads
        .iter()
        .filter(|r| r.task_id == task_id)
        .map(|read| {
            if declared_for_task.is_empty() {
                return ReconVerdict::Untracked;
            }

            // When the agent names the port this read satisfied, both the
            // producer path AND the consumer port must identify the same
            // declared data-flow edge. Matching only the producer directory
            // lets a file for one port masquerade as another whenever a task
            // consumes two products from the same producer, or when an
            // ordering-only edge happens to name the file's ancestor.
            let path_match = declared_for_task.iter().find(|e| {
                path_under_producer_output(&read.path, &e.from_node)
                    && read
                        .declared_port
                        .as_ref()
                        .map(|port| &e.to_port == port)
                        .unwrap_or(true)
            });

            if let Some(edge) = path_match {
                return ReconVerdict::Match {
                    authoritative_edge: (edge.from_node.clone(), edge.to_node.clone()),
                };
            }

            let declared_producer = read.declared_port.as_ref().and_then(|port| {
                declared_for_task
                    .iter()
                    .find(|e| &e.to_port == port)
                    .map(|e| e.from_node.clone())
            });

            ReconVerdict::Divergent {
                read_path: read.path.clone(),
                declared_producer,
            }
        })
        .collect()
}

/// How a declared edge must appear in the reconciled **standard**
/// structural graph after observed-provenance resolves which member of a
/// mutually-exclusive one-of input group was actually read (§G-B1).
///
/// This is the decision only — [`crate::ro_crate::reconcile_ro_crate_edges`]
/// applies it to the RO-Crate `@graph` (dropping / stamping the
/// `ParameterConnection` nodes). Pure and deterministic.
///
/// `#[non_exhaustive]` per the workspace public-enum convention.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EdgeDisposition {
    /// The task's observed reads resolved THIS edge as the authoritative
    /// source. Keep it in the standard graph, marked `authoritative`.
    Authoritative,
    /// This edge is one member of a mutually-exclusive one-of group whose
    /// authoritative member was resolved to a DIFFERENT sibling — i.e. this
    /// candidate was NOT read. It must be **dropped from the standard
    /// structural graph** (so a generic RO-Crate / WRROC / runcrate consumer
    /// never reads it as an authoritative data flow) and recorded ONLY in an
    /// `ecaax:` side channel. `superseded_by` names the authoritative
    /// sibling's producer node.
    UnusedCandidate { superseded_by: String },
    /// Member of a one-of group that reconciliation could NOT disambiguate:
    /// no sibling resolved authoritative (e.g. the actual read diverged from
    /// every declared producer, or no read landed for the group this pass).
    /// Both members are kept, marked `candidate_unused` — we cannot know
    /// which was read, so neither may be dropped.
    UnresolvedCandidate,
    /// Ordinary (non-grouped) edge with no read evidence this pass. Left in
    /// the standard graph, unstamped — silence is not evidence either way.
    Unobserved,
}

/// Decide, for a single task, how each of its declared edges must appear in
/// the reconciled **standard** structural graph, given the authoritative
/// `(from_node, to_node)` edges its observed reads resolved (the
/// [`ReconVerdict::Match`] set).
///
/// Only a mutually-exclusive one-of group whose authoritative member was
/// resolved yields [`EdgeDisposition::UnusedCandidate`] for its *other*
/// members; every other case keeps the edge (see [`EdgeDisposition`]). One
/// disposition is returned per edge, in `declared_for_task`'s order.
///
/// Pure + deterministic — no I/O, no clock, no `HashMap`.
pub fn classify_reconciled_edges(
    declared_for_task: &[EdgeContract],
    authoritative: &BTreeSet<(String, String)>,
) -> Vec<EdgeDisposition> {
    // A one-of group is "resolved" once one of its members is authoritative.
    let mut resolved_groups: BTreeSet<&str> = BTreeSet::new();
    for e in declared_for_task {
        if let Some(g) = e.mutually_exclusive_group.as_deref() {
            if authoritative.contains(&(e.from_node.clone(), e.to_node.clone())) {
                resolved_groups.insert(g);
            }
        }
    }

    declared_for_task
        .iter()
        .map(|e| {
            if authoritative.contains(&(e.from_node.clone(), e.to_node.clone())) {
                return EdgeDisposition::Authoritative;
            }
            match e.mutually_exclusive_group.as_deref() {
                Some(g) if resolved_groups.contains(g) => {
                    // Some sibling in this group was read; THIS one was not.
                    let superseded_by = declared_for_task
                        .iter()
                        .find(|s| {
                            s.mutually_exclusive_group.as_deref() == Some(g)
                                && authoritative.contains(&(s.from_node.clone(), s.to_node.clone()))
                        })
                        .map(|s| s.from_node.clone())
                        .unwrap_or_default();
                    EdgeDisposition::UnusedCandidate { superseded_by }
                }
                Some(_) => EdgeDisposition::UnresolvedCandidate,
                None => EdgeDisposition::Unobserved,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::{CompatibilityProof, EdgeKind};

    fn edge(from_node: &str, from_port: &str, to_node: &str, to_port: &str) -> EdgeContract {
        EdgeContract {
            from_node: from_node.into(),
            from_port: from_port.into(),
            to_node: to_node.into(),
            to_port: to_port.into(),
            proof: CompatibilityProof::default(),
            kind: EdgeKind::TypedDataFlow,
            chain_of_custody: None,
            mutually_exclusive_group: None,
        }
    }

    #[test]
    fn read_matching_declared_producer_output_is_a_match() {
        let edges = vec![edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        assert_eq!(v.len(), 1);
        assert!(matches!(v[0], ReconVerdict::Match { .. }));
        match &v[0] {
            ReconVerdict::Match { authoritative_edge } => {
                assert_eq!(
                    authoritative_edge,
                    &(
                        "quantification".to_string(),
                        "differential_expression".to_string()
                    )
                );
            }
            other => panic!("expected Match, got {other:?}"),
        }
    }

    #[test]
    fn read_of_undeclared_source_is_divergent() {
        let edges = vec![edge(
            "normalisation",
            "normalized_counts",
            "differential_expression",
            "normalized_counts",
        )];
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: None,
            path: "runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        assert!(v
            .iter()
            .any(|x| matches!(x, ReconVerdict::Divergent { .. })));
        match &v[0] {
            ReconVerdict::Divergent {
                read_path,
                declared_producer,
            } => {
                assert_eq!(
                    read_path,
                    "runtime/outputs/data_acquisition/data/himes-inputs/counts.tsv"
                );
                assert_eq!(declared_producer, &None);
            }
            other => panic!("expected Divergent, got {other:?}"),
        }
    }

    #[test]
    fn divergent_read_names_the_declared_producer_for_its_claimed_port() {
        // The read claims to satisfy `normalized_counts`, whose declared
        // producer is `normalisation` — but the actual path came from
        // `data_acquisition`, so the graph's belief about the producer is
        // surfaced even though the read itself diverges.
        let edges = vec![edge(
            "normalisation",
            "normalized_counts",
            "differential_expression",
            "normalized_counts",
        )];
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("normalized_counts".into()),
            path: "runtime/outputs/data_acquisition/counts.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        match &v[0] {
            ReconVerdict::Divergent {
                declared_producer, ..
            } => assert_eq!(declared_producer.as_deref(), Some("normalisation")),
            other => panic!("expected Divergent, got {other:?}"),
        }
    }

    #[test]
    fn producer_path_for_a_different_port_is_divergent() {
        let edges = vec![
            edge(
                "qc_preprocessing",
                "filtered_count_matrix",
                "differential_expression",
                "raw_counts",
            ),
            edge(
                "data_acquisition",
                "cohort_manifest",
                "differential_expression",
                "experimental_design",
            ),
        ];
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/data_acquisition/data/counts.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        match &v[0] {
            ReconVerdict::Divergent {
                declared_producer, ..
            } => assert_eq!(declared_producer.as_deref(), Some("qc_preprocessing")),
            other => panic!("expected Divergent, got {other:?}"),
        }
    }

    #[test]
    fn ordering_only_edge_does_not_authorize_data_reads() {
        let mut ordering = edge(
            "data_acquisition",
            "splice",
            "differential_expression",
            "splice",
        );
        ordering.kind = EdgeKind::OrderingOnly;
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/data_acquisition/data/counts.tsv".into(),
        }];
        assert_eq!(
            reconcile(&[ordering], &reads, "differential_expression"),
            vec![ReconVerdict::Untracked],
            "an ordering edge is not a declared data producer"
        );
    }

    #[test]
    fn task_with_no_declared_edges_is_untracked() {
        let edges: Vec<EdgeContract> = vec![];
        let reads = vec![ObservedRead {
            task_id: "final_reporting".into(),
            declared_port: None,
            path: "runtime/outputs/differential_expression/de.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "final_reporting");
        assert_eq!(v, vec![ReconVerdict::Untracked]);
    }

    #[test]
    fn reads_addressed_to_a_different_task_are_ignored() {
        let edges = vec![edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let reads = vec![ObservedRead {
            task_id: "normalisation".into(),
            declared_port: None,
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        assert!(v.is_empty());
    }

    #[test]
    fn one_of_group_resolves_to_the_member_actually_read() {
        // Mirrors the DE one-of group: both raw_counts and
        // normalized_counts are declared candidates, but only one was
        // actually read — reconcile must pick that member as
        // authoritative and say nothing about the unread sibling.
        let edges = vec![
            edge(
                "quantification",
                "count_matrix",
                "differential_expression",
                "raw_counts",
            ),
            edge(
                "normalisation",
                "normalized_counts",
                "differential_expression",
                "normalized_counts",
            ),
        ];
        let reads = vec![ObservedRead {
            task_id: "differential_expression".into(),
            declared_port: Some("raw_counts".into()),
            path: "runtime/outputs/quantification/count_matrix.tsv".into(),
        }];
        let v = reconcile(&edges, &reads, "differential_expression");
        assert_eq!(v.len(), 1);
        assert_eq!(
            v[0],
            ReconVerdict::Match {
                authoritative_edge: (
                    "quantification".to_string(),
                    "differential_expression".to_string()
                )
            }
        );
    }

    // ── §G-B1 — reconciled-edge disposition (which one-of candidate the
    // standard graph must drop) ───────────────────────────────────────────

    fn grouped_edge(
        from_node: &str,
        from_port: &str,
        to_node: &str,
        to_port: &str,
        group: &str,
    ) -> EdgeContract {
        let mut e = edge(from_node, from_port, to_node, to_port);
        e.mutually_exclusive_group = Some(group.into());
        e
    }

    fn de_one_of() -> Vec<EdgeContract> {
        vec![
            grouped_edge(
                "quantification",
                "count_matrix",
                "differential_expression",
                "raw_counts",
                "counts",
            ),
            grouped_edge(
                "normalisation",
                "normalized_counts",
                "differential_expression",
                "normalized_counts",
                "counts",
            ),
        ]
    }

    #[test]
    fn resolved_one_of_drops_the_unread_sibling_and_keeps_the_read_member() {
        // raw_counts (quantification) was read; normalized_counts
        // (normalisation) is the unread candidate of the same group.
        let edges = de_one_of();
        let mut authoritative: BTreeSet<(String, String)> = BTreeSet::new();
        authoritative.insert((
            "quantification".to_string(),
            "differential_expression".to_string(),
        ));

        let d = classify_reconciled_edges(&edges, &authoritative);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0], EdgeDisposition::Authoritative);
        assert_eq!(
            d[1],
            EdgeDisposition::UnusedCandidate {
                superseded_by: "quantification".to_string()
            },
            "the unread normalized candidate must be dropped, superseded by the read raw member"
        );
    }

    #[test]
    fn unresolved_one_of_keeps_both_members_as_candidates() {
        // Reconciliation resolved NEITHER member authoritative (e.g. the
        // actual read diverged from both producers) — both stay candidates,
        // neither is dropped, so we never fabricate a resolution.
        let edges = de_one_of();
        let authoritative: BTreeSet<(String, String)> = BTreeSet::new();

        let d = classify_reconciled_edges(&edges, &authoritative);
        assert_eq!(
            d,
            vec![
                EdgeDisposition::UnresolvedCandidate,
                EdgeDisposition::UnresolvedCandidate
            ]
        );
    }

    #[test]
    fn ordinary_unobserved_edge_is_left_untouched() {
        let edges = vec![edge(
            "quantification",
            "count_matrix",
            "differential_expression",
            "raw_counts",
        )];
        let authoritative: BTreeSet<(String, String)> = BTreeSet::new();

        let d = classify_reconciled_edges(&edges, &authoritative);
        assert_eq!(d, vec![EdgeDisposition::Unobserved]);
    }
}
