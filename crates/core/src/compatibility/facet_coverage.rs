//! Facet coverage — how much of a DAG's semantic-facet surface is
//! actually decided rather than left unknown.
//!
//! # What this measures, and what it does not
//!
//! A `CompatibilityProof` today establishes **EDAM port-type
//! subsumption**: the producer's output type is the consumer's input
//! type or a subtype of it. Alongside that it records a per-facet
//! unification for the eight facets in [`FACETS`]. When neither side
//! declares a facet the unification is `Unknown`, which is an honest
//! "the composer does not know", not a pass.
//!
//! This module counts those outcomes. A high fraction of
//! [`FacetCoverage::exact_both_declared`] means the two contracts
//! *stated* the same organism / build / units and the composer checked
//! that statement. It does NOT mean the data on disk matches the
//! declaration — nothing here reads an artifact — and it is not a
//! safety property. It is a coverage measure over declarations.
//!
//! # Counting rules
//!
//! `unify_facet` returns `Exact` for two distinct situations and they
//! are counted separately here:
//!
//! - **both sides declared and agreed** → [`FacetCoverage::exact_both_declared`].
//!   This is the only bucket [`FacetCoverage::fraction_exact`] counts.
//! - **producer declared, consumer left the facet unconstrained** →
//!   [`FacetCoverage::producer_only`]. The engine treats this as
//!   compatible (an unconstrained consumer cannot be violated) but no
//!   agreement was checked, so folding it into "exact" would inflate
//!   the number.
//!
//! Coverage is computed from the endpoint `PortContract`s rather than
//! from `CompatibilityProof::facet_matches`, so it measures the
//! contracts themselves and is unaffected by which matches the engine
//! chooses to surface into a proof.
//!
//! # Status: advisory
//!
//! [`facet_coverage_advisory`] is WARN-ONLY. Nothing in this module
//! refuses an edge, blocks emission, or feeds a Required gate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::workflow_contracts::edge::EdgeContract;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

use super::engine::facet_subtype_rationale;
use super::facet_propagation::terminal_edge_indices;
use super::facet_unification::{unify_facet, FacetUnification};

/// The facets the compatibility engine unifies, in engine order.
pub const FACETS: [&str; 8] = [
    "modality",
    "organism",
    "genome_build",
    "annotation_version",
    "coordinate_system",
    "units",
    "normalization_state",
    "statistical_state",
];

/// Which edges a [`FacetCoverage`] was computed over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacetCoverageScope {
    /// Every edge in the DAG.
    AllEdges,
    /// Only edges whose consumer has no outgoing edge — the results
    /// the run actually reports on.
    TerminalEdges,
}

impl FacetCoverageScope {
    fn label(self) -> &'static str {
        match self {
            FacetCoverageScope::AllEdges => "all edges",
            FacetCoverageScope::TerminalEdges => "terminal edges",
        }
    }
}

/// Per-facet breakdown of one coverage measurement.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCoverageRow {
    /// Both endpoints declared the facet and the values agreed.
    pub exact_both_declared: usize,
    /// Producer declared it; consumer left it unconstrained.
    pub producer_only: usize,
    /// Producer is a subtype of what the consumer accepts.
    pub subtype: usize,
    /// Reconciled by a declared adapter.
    pub substituted: usize,
    /// Neither side declared it, or only the consumer did.
    pub unknown: usize,
    /// Both declared and the values are irreconcilable.
    pub incompatible: usize,
}

impl FacetCoverageRow {
    /// Total checks recorded in this row.
    pub fn total(&self) -> usize {
        self.exact_both_declared
            + self.producer_only
            + self.subtype
            + self.substituted
            + self.unknown
            + self.incompatible
    }
}

/// Aggregate facet coverage over a set of edges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetCoverage {
    /// Which edges this covers.
    pub scope: FacetCoverageScope,
    /// Edges that contributed checks. An edge naming ports that do not
    /// exist on either node (a structural splice) contributes none.
    pub edges_considered: usize,
    /// Sum over [`Self::per_facet`] of every bucket.
    pub checks_total: usize,
    /// Both endpoints declared and agreed.
    pub exact_both_declared: usize,
    /// Producer declared, consumer unconstrained.
    pub producer_only: usize,
    /// Producer subtype of consumer.
    pub subtype: usize,
    /// Reconciled by an adapter.
    pub substituted: usize,
    /// Undecided.
    pub unknown: usize,
    /// Irreconcilable.
    pub incompatible: usize,
    /// Per-facet rows, keyed by facet name.
    pub per_facet: BTreeMap<String, FacetCoverageRow>,
}

impl FacetCoverage {
    fn empty(scope: FacetCoverageScope) -> Self {
        Self {
            scope,
            edges_considered: 0,
            checks_total: 0,
            exact_both_declared: 0,
            producer_only: 0,
            subtype: 0,
            substituted: 0,
            unknown: 0,
            incompatible: 0,
            per_facet: BTreeMap::new(),
        }
    }

    /// Fraction of checks where both endpoints declared the facet and
    /// agreed. `0.0` when no checks were recorded — an empty scope has
    /// no coverage to claim.
    pub fn fraction_exact(&self) -> f64 {
        if self.checks_total == 0 {
            return 0.0;
        }
        self.exact_both_declared as f64 / self.checks_total as f64
    }

    /// Fraction of checks the composer could not decide.
    pub fn fraction_unknown(&self) -> f64 {
        if self.checks_total == 0 {
            return 0.0;
        }
        self.unknown as f64 / self.checks_total as f64
    }

    /// One-line human summary. Deliberately phrased as coverage of
    /// *declarations*.
    pub fn summary(&self) -> String {
        format!(
            "facet coverage ({}): {}/{} checks agreed on both sides ({:.1}%), \
             {} producer-only, {} unknown, {} subtype, {} substituted, {} incompatible, \
             over {} edges",
            self.scope.label(),
            self.exact_both_declared,
            self.checks_total,
            self.fraction_exact() * 100.0,
            self.producer_only,
            self.unknown,
            self.subtype,
            self.substituted,
            self.incompatible,
            self.edges_considered,
        )
    }

    fn record(&mut self, facet: &str, outcome: &FacetUnification, both_declared: bool) {
        let row = self.per_facet.entry(facet.to_string()).or_default();
        match outcome {
            FacetUnification::Exact if both_declared => {
                row.exact_both_declared += 1;
                self.exact_both_declared += 1;
            }
            // Defensive compatibility for a legacy or externally deserialized
            // `Exact` paired with a missing declaration. New unification emits
            // `ProducerOnly`, but the coverage report must never count an
            // inconsistent one-sided row as two-sided agreement.
            FacetUnification::Exact => {
                row.producer_only += 1;
                self.producer_only += 1;
            }
            FacetUnification::ProducerOnly { .. } => {
                row.producer_only += 1;
                self.producer_only += 1;
            }
            FacetUnification::Subtype { .. } => {
                row.subtype += 1;
                self.subtype += 1;
            }
            FacetUnification::Substituted { .. } => {
                row.substituted += 1;
                self.substituted += 1;
            }
            FacetUnification::Unknown { .. } => {
                row.unknown += 1;
                self.unknown += 1;
            }
            FacetUnification::Incompatible { .. } => {
                row.incompatible += 1;
                self.incompatible += 1;
            }
        }
        self.checks_total += 1;
    }
}

fn output_port<'a>(node: &'a TaskNode, name: &str) -> Option<&'a PortContract> {
    node.outputs.iter().find(|p| p.name == name)
}

fn input_port<'a>(node: &'a TaskNode, name: &str) -> Option<&'a PortContract> {
    node.inputs.iter().find(|p| p.name == name)
}

fn facet_pair<'a>(
    producer: &'a PortContract,
    consumer: &'a PortContract,
    facet: &str,
) -> (Option<&'a str>, Option<&'a str>) {
    match facet {
        "modality" => (producer.modality.as_deref(), consumer.modality.as_deref()),
        "organism" => (producer.organism.as_deref(), consumer.organism.as_deref()),
        "genome_build" => (
            producer.genome_build.as_deref(),
            consumer.genome_build.as_deref(),
        ),
        "annotation_version" => (
            producer.annotation_version.as_deref(),
            consumer.annotation_version.as_deref(),
        ),
        "coordinate_system" => (
            producer.coordinate_system.as_deref(),
            consumer.coordinate_system.as_deref(),
        ),
        "units" => (producer.units.as_deref(), consumer.units.as_deref()),
        "normalization_state" => (
            producer.normalization_state.as_deref(),
            consumer.normalization_state.as_deref(),
        ),
        "statistical_state" => (
            producer.statistical_state.as_deref(),
            consumer.statistical_state.as_deref(),
        ),
        _ => (None, None),
    }
}

/// Coverage over an explicit edge subset, identified by index into
/// `dag.edges`.
pub fn facet_coverage_over(
    dag: &WorkflowDag,
    edge_indices: &[usize],
    scope: FacetCoverageScope,
) -> FacetCoverage {
    let mut coverage = FacetCoverage::empty(scope);
    let by_id: BTreeMap<&str, &TaskNode> = dag.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    for &i in edge_indices {
        let Some(edge) = dag.edges.get(i) else {
            continue;
        };
        let (Some(producer), Some(consumer)) = (
            by_id.get(edge.from_node.as_str()),
            by_id.get(edge.to_node.as_str()),
        ) else {
            continue;
        };
        let (Some(pp), Some(cp)) = (
            output_port(producer, &edge.from_port),
            input_port(consumer, &edge.to_port),
        ) else {
            // Structural / ordering-only edges name no real ports and
            // carry no data, so they carry no facet check either.
            continue;
        };
        coverage.edges_considered += 1;
        for facet in FACETS {
            let (p, c) = facet_pair(pp, cp, facet);
            // Same subtype table the engine applies, so the measurement
            // and the proof can never disagree about an edge.
            let outcome = unify_facet(
                facet,
                p,
                c,
                |pv, cv| facet_subtype_rationale(facet, pv, cv),
                |_, _| None,
            );
            coverage.record(facet, &outcome, p.is_some() && c.is_some());
        }
    }
    coverage
}

/// Coverage over every edge in the DAG.
pub fn facet_coverage(dag: &WorkflowDag) -> FacetCoverage {
    let all: Vec<usize> = (0..dag.edges.len()).collect();
    facet_coverage_over(dag, &all, FacetCoverageScope::AllEdges)
}

/// Coverage over the terminal edges — the ones feeding the nodes a run
/// actually reports from.
pub fn terminal_facet_coverage(dag: &WorkflowDag) -> FacetCoverage {
    let terminal = terminal_edge_indices(&dag.edges);
    facet_coverage_over(dag, &terminal, FacetCoverageScope::TerminalEdges)
}

/// Advisory floor. Chosen as a visible-progress marker, not a
/// scientific threshold: below it, most facet checks on the reported
/// results are undecided.
pub const FACET_COVERAGE_ADVISORY_FLOOR: f64 = 0.25;

/// WARN-ONLY advisory. Returns a message when the measured fraction is
/// below `floor`, `None` otherwise. Callers log it; nothing refuses an
/// edge or blocks emission on it.
pub fn facet_coverage_advisory(coverage: &FacetCoverage, floor: f64) -> Option<String> {
    if coverage.checks_total == 0 {
        return Some(format!(
            "facet coverage ({}): no facet checks recorded — no edge connected two \
             declared ports (advisory)",
            coverage.scope.label()
        ));
    }
    if coverage.fraction_exact() < floor {
        return Some(format!(
            "{} — below the {:.0}% advisory floor (advisory only, nothing is blocked)",
            coverage.summary(),
            floor * 100.0
        ));
    }
    None
}

/// Emit [`facet_coverage_advisory`] through `tracing` at WARN.
pub fn log_facet_coverage_advisory(coverage: &FacetCoverage, floor: f64) {
    if let Some(msg) = facet_coverage_advisory(coverage, floor) {
        tracing::warn!("{msg}");
    }
}

/// Coverage read back from recorded proofs rather than from live port
/// contracts. Used to audit an already-emitted `runtime/proofs.jsonl`,
/// where the ports are no longer available.
///
/// This sees only the facet matches the engine chose to surface, so it
/// can undercount relative to [`facet_coverage`]. `Exact` rows with an
/// empty consumer value are counted as producer-only, mirroring the
/// live-port accounting.
pub fn facet_coverage_from_proof_rows(edges: &[EdgeContract]) -> FacetCoverage {
    use crate::workflow_contracts::edge::FacetMatchKind;
    let mut coverage = FacetCoverage::empty(FacetCoverageScope::AllEdges);
    for edge in edges {
        if edge.proof.facet_matches.is_empty() {
            continue;
        }
        coverage.edges_considered += 1;
        for fm in &edge.proof.facet_matches {
            let row = coverage.per_facet.entry(fm.facet.clone()).or_default();
            match fm.kind {
                FacetMatchKind::Exact => {
                    if fm.producer.is_empty() || fm.consumer.is_empty() {
                        row.producer_only += 1;
                        coverage.producer_only += 1;
                    } else {
                        row.exact_both_declared += 1;
                        coverage.exact_both_declared += 1;
                    }
                }
                FacetMatchKind::Subtype => {
                    row.subtype += 1;
                    coverage.subtype += 1;
                }
                FacetMatchKind::Substituted => {
                    row.substituted += 1;
                    coverage.substituted += 1;
                }
                FacetMatchKind::Unknown => {
                    row.unknown += 1;
                    coverage.unknown += 1;
                }
            }
            coverage.checks_total += 1;
        }
    }
    coverage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::semantic_type::SemanticType;

    fn port(name: &str) -> PortContract {
        PortContract {
            name: name.into(),
            semantic_type: SemanticType::edam("data:3917", ""),
            ..Default::default()
        }
    }

    fn two_node_dag() -> WorkflowDag {
        let mut a = TaskNode::skeleton("a", "a");
        a.outputs = vec![port("out")];
        let mut b = TaskNode::skeleton("b", "b");
        b.inputs = vec![port("in")];
        WorkflowDag {
            id: "t".into(),
            nodes: vec![a, b],
            edges: vec![EdgeContract {
                from_node: "a".into(),
                from_port: "out".into(),
                to_node: "b".into(),
                to_port: "in".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn bare_ports_are_all_unknown() {
        let dag = two_node_dag();
        let cov = facet_coverage(&dag);
        assert_eq!(cov.checks_total, 8);
        assert_eq!(cov.unknown, 8);
        assert_eq!(cov.exact_both_declared, 0);
        assert!((cov.fraction_exact() - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn both_declared_and_agreeing_counts_as_exact() {
        let mut dag = two_node_dag();
        dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
        dag.nodes[1].inputs[0].organism = Some("Homo sapiens".into());
        let cov = facet_coverage(&dag);
        assert_eq!(cov.exact_both_declared, 1);
        assert_eq!(cov.per_facet["organism"].exact_both_declared, 1);
        assert_eq!(cov.unknown, 7);
    }

    #[test]
    fn producer_only_is_not_counted_as_exact() {
        let mut dag = two_node_dag();
        dag.nodes[0].outputs[0].units = Some("read counts".into());
        let cov = facet_coverage(&dag);
        assert_eq!(
            cov.exact_both_declared, 0,
            "an unconstrained consumer agreed to nothing"
        );
        assert_eq!(cov.producer_only, 1);
        assert_eq!(cov.per_facet["units"].producer_only, 1);
    }

    #[test]
    fn consumer_only_is_unknown() {
        let mut dag = two_node_dag();
        dag.nodes[1].inputs[0].units = Some("read counts".into());
        let cov = facet_coverage(&dag);
        assert_eq!(cov.per_facet["units"].unknown, 1);
        assert_eq!(cov.exact_both_declared, 0);
    }

    #[test]
    fn mismatch_is_counted_as_incompatible() {
        let mut dag = two_node_dag();
        dag.nodes[0].outputs[0].genome_build = Some("GRCh37".into());
        dag.nodes[1].inputs[0].genome_build = Some("GRCh38".into());
        let cov = facet_coverage(&dag);
        assert_eq!(cov.incompatible, 1);
        assert_eq!(cov.per_facet["genome_build"].incompatible, 1);
    }

    #[test]
    fn structural_edge_with_no_real_ports_contributes_nothing() {
        let mut dag = two_node_dag();
        dag.edges
            .push(EdgeContract::synthetic_splice("a".into(), "b".into()));
        let cov = facet_coverage(&dag);
        assert_eq!(cov.edges_considered, 1);
        assert_eq!(cov.checks_total, 8);
    }

    #[test]
    fn advisory_is_warn_only_and_fires_below_the_floor() {
        let dag = two_node_dag();
        let cov = facet_coverage(&dag);
        let msg = facet_coverage_advisory(&cov, FACET_COVERAGE_ADVISORY_FLOOR)
            .expect("all-unknown coverage must trip the advisory");
        assert!(msg.contains("advisory"), "{msg}");
        // Nothing about the advisory mutates or refuses anything: the
        // coverage value is unchanged by asking for it.
        assert_eq!(facet_coverage(&dag), cov);
    }

    #[test]
    fn advisory_is_silent_above_the_floor() {
        let mut dag = two_node_dag();
        dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
        dag.nodes[0].outputs[0].genome_build = Some("GRCh38.p14".into());
        dag.nodes[0].outputs[0].modality = Some("bulk_rnaseq".into());
        dag.nodes[1].inputs[0].organism = Some("Homo sapiens".into());
        dag.nodes[1].inputs[0].genome_build = Some("GRCh38.p14".into());
        dag.nodes[1].inputs[0].modality = Some("bulk_rnaseq".into());
        let cov = facet_coverage(&dag);
        assert_eq!(cov.exact_both_declared, 3);
        assert!(cov.fraction_exact() > FACET_COVERAGE_ADVISORY_FLOOR);
        assert!(facet_coverage_advisory(&cov, FACET_COVERAGE_ADVISORY_FLOOR).is_none());
    }

    #[test]
    fn terminal_scope_covers_only_edges_into_sinks() {
        let mut a = TaskNode::skeleton("a", "a");
        a.outputs = vec![port("out")];
        let mut b = TaskNode::skeleton("b", "b");
        b.inputs = vec![port("in")];
        b.outputs = vec![port("out")];
        let mut c = TaskNode::skeleton("c", "c");
        c.inputs = vec![port("in")];
        let dag = WorkflowDag {
            id: "t".into(),
            nodes: vec![a, b, c],
            edges: vec![
                EdgeContract {
                    from_node: "a".into(),
                    from_port: "out".into(),
                    to_node: "b".into(),
                    to_port: "in".into(),
                    ..Default::default()
                },
                EdgeContract {
                    from_node: "b".into(),
                    from_port: "out".into(),
                    to_node: "c".into(),
                    to_port: "in".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let cov = terminal_facet_coverage(&dag);
        assert_eq!(cov.scope, FacetCoverageScope::TerminalEdges);
        assert_eq!(cov.edges_considered, 1);
        assert_eq!(cov.checks_total, 8);
    }

    #[test]
    fn proof_row_reader_matches_the_live_port_reading_for_a_declared_pair() {
        let mut dag = two_node_dag();
        dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
        dag.nodes[1].inputs[0].organism = Some("Homo sapiens".into());
        let live = facet_coverage(&dag);

        use crate::workflow_contracts::edge::{FacetMatch, FacetMatchKind};
        let mut edge = dag.edges[0].clone();
        edge.proof.facet_matches = vec![FacetMatch {
            facet: "organism".into(),
            producer: "Homo sapiens".into(),
            consumer: "Homo sapiens".into(),
            kind: FacetMatchKind::Exact,
            rationale: None,
        }];
        let from_rows = facet_coverage_from_proof_rows(&[edge]);
        assert_eq!(
            from_rows.exact_both_declared, live.exact_both_declared,
            "both readings must agree on the declared-and-agreeing count"
        );
    }
}
