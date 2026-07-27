//! Run-scoped facet propagation.
//!
//! Two disjoint classes of semantic facet reach a `PortContract`:
//!
//! - **Atom-declared** — `units`, `normalization_state`,
//!   `statistical_state` (and `modality` on the handful of atoms that
//!   are genuinely single-modality). These are properties of the
//!   atom's own contract: `quantification` emits raw read counts in
//!   every run it appears in, so its YAML declares them once.
//! - **Run-scoped** — `organism`, `genome_build`, `annotation_version`,
//!   `coordinate_system`, and `modality` on every atom that is shared
//!   across modalities. No atom can declare these: `differential_
//!   expression` is used by 8 modalities and `alignment` by 16, so a
//!   literal `modality:` on either port would make the atom refuse
//!   every other archetype (two differing `Some` values unify to
//!   `FacetUnification::Incompatible`).
//!
//! This module handles the second class. Run-scoped facets are
//! **propagated, never invented**: a value only ever reaches a port
//! because some port in the same data-flow component declared it, or
//! because the run's own intake pinned it (`IntakeFacts.organism_name`,
//! `IntakeFacts.pinned_reference_bundles`). When nothing declares a
//! facet it stays `None` and the proof honestly records `Unknown`.
//!
//! # Why components, not a forward sweep
//!
//! Propagation runs over the *connected components* of the port graph
//! (ports joined by data-flow edges, plus the intra-node join from a
//! node's inputs to its outputs). A component is assigned a value only
//! when the declared values inside it are unanimous. That gives two
//! properties a directional sweep does not:
//!
//! 1. **It cannot manufacture an incompatibility.** Every port in a
//!    component ends up carrying the same value, and every edge lies
//!    inside a component, so a propagated facet can never produce the
//!    `(Some(p), Some(c)) where p != c` arm that refuses an edge. A
//!    forward sweep can: two branches seeded differently meet at a
//!    join node and the already-written value clashes with the
//!    late-discovered one.
//! 2. **A join of two disagreeing branches degrades to `Unknown`, not
//!    to a guess.** A cross-omics DAG whose RNA branch declares
//!    `bulk_rnaseq` and whose ATAC branch declares `atac_seq` leaves
//!    the integrating node's ports unset and records a
//!    [`FacetConflict`].
//!
//! # Scope
//!
//! This module mutates `PortContract` facets on a `WorkflowDag`. It
//! does NOT re-run the compatibility engine — edge proofs recorded
//! before propagation are stale afterwards, so the composer must call
//! this before it proves its edges.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::workflow_contracts::edge::EdgeContract;
use crate::workflow_contracts::port::PortContract;
use crate::workflow_contracts::task_node::{TaskNode, WorkflowDag};

/// How a facet is allowed to travel along a typed data-flow edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacetPropagation {
    /// Invariant under every transformation a pipeline applies to the
    /// data: trimming, aligning, counting and testing all preserve the
    /// organism the reads came from and the assembly they were mapped
    /// to. Travels across every data-flow edge.
    RunInvariant,
    /// A property of the physical format, not of the biology: BAM is
    /// 1-based inclusive, BED is 0-based half-open. Travels only
    /// across an edge whose two endpoints share a `physical_format`,
    /// so a format conversion stops it rather than silently carrying a
    /// wrong coordinate convention downstream.
    FormatScoped,
    /// Never propagated. The atom's own contract is the only authority
    /// — a stage that normalizes changes `normalization_state`, so
    /// inheriting the upstream value would be a lie.
    AtomDeclared,
}

/// Propagation rule for a facet name. Unknown facet names are
/// [`FacetPropagation::AtomDeclared`] — the conservative default, since
/// an unrecognized facet has no known invariance.
pub fn propagation_rule(facet: &str) -> FacetPropagation {
    match facet {
        "organism" | "genome_build" | "annotation_version" | "modality" => {
            FacetPropagation::RunInvariant
        }
        "coordinate_system" => FacetPropagation::FormatScoped,
        _ => FacetPropagation::AtomDeclared,
    }
}

/// The facets this module propagates, in a fixed order so reports are
/// byte-stable.
pub const PROPAGATED_FACETS: [&str; 5] = [
    "modality",
    "organism",
    "genome_build",
    "annotation_version",
    "coordinate_system",
];

/// Which side of a node a port sits on. Input and output ports may
/// share a name, so the side is part of a port's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PortSide {
    /// An entry in `TaskNode::inputs`.
    Input,
    /// An entry in `TaskNode::outputs`.
    Output,
}

/// Identity of one port inside a `WorkflowDag`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PortKey {
    /// `TaskNode::id`.
    pub node_id: String,
    /// Input or output.
    pub side: PortSide,
    /// `PortContract::name`.
    pub port: String,
}

impl PortKey {
    fn new(node_id: &str, side: PortSide, port: &str) -> Self {
        Self {
            node_id: node_id.to_string(),
            side,
            port: port.to_string(),
        }
    }
}

/// Where a propagated value came from. Recorded on every assignment so
/// a reader can tell a declared value from an inherited one without
/// re-deriving the component.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum FacetOrigin {
    /// Inherited from a port that declares the facet in its atom YAML
    /// (or from an explicit anchor).
    DeclaredOnPort {
        /// The declaring port.
        origin: PortKey,
    },
    /// Seeded from the run's own intake facts. Only applied to a
    /// component that contains a source-node port (a run input) and
    /// that declares the facet nowhere.
    RunScopedSeed {
        /// Intake field the value was read from, e.g.
        /// `IntakeFacts.organism_name`.
        field: String,
    },
}

/// One facet value written onto a port by propagation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetAssignment {
    /// Port that received the value.
    pub port: PortKey,
    /// Facet name.
    pub facet: String,
    /// Value written.
    pub value: String,
    /// Where the value came from.
    pub origin: FacetOrigin,
}

/// A data-flow component whose declared values disagree. Nothing is
/// assigned inside it — the honest outcome is `Unknown` on the edges,
/// not a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetConflict {
    /// Facet name.
    pub facet: String,
    /// The distinct declared values found in the component, sorted.
    pub values: Vec<String>,
    /// The ports that declared them, sorted.
    pub declaring_ports: Vec<PortKey>,
}

/// Outcome of one [`propagate_run_facets`] call. Purely informational:
/// nothing in this report blocks composition.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FacetPropagationReport {
    /// Values written, sorted by `(facet, port)`.
    pub assignments: Vec<FacetAssignment>,
    /// Components left unset because their declared values disagreed.
    pub conflicts: Vec<FacetConflict>,
}

impl FacetPropagationReport {
    /// Count of ports that received a value.
    pub fn assigned_count(&self) -> usize {
        self.assignments.len()
    }
}

/// A run-scoped facet value plus the intake field it was read from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeedValue {
    /// The value.
    pub value: String,
    /// Intake field of origin, recorded verbatim into
    /// [`FacetOrigin::RunScopedSeed`].
    pub field: String,
}

/// Run-scoped facet values known before composition.
///
/// Two tiers, both optional:
///
/// - `run_scoped` — facets the whole run shares (the organism the
///   samples came from, the assembly the references were pinned to).
///   Applied only to components that contain a run-input port and that
///   declare the facet nowhere, so an atom-declared value always wins.
/// - `anchors` — a value pinned to one named port, for callers that
///   know exactly which reference/annotation input carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunFacetSeed {
    run_scoped: BTreeMap<String, SeedValue>,
    anchors: BTreeMap<PortKey, BTreeMap<String, SeedValue>>,
}

impl RunFacetSeed {
    /// Empty seed. Propagation still runs — it redistributes whatever
    /// the atoms themselves declare.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a run-wide value for `facet`. Ignored for facets whose
    /// rule is [`FacetPropagation::AtomDeclared`] — those have no
    /// run-level meaning and silently accepting one would let a caller
    /// stamp `normalization_state` across a whole DAG.
    pub fn with_run_scoped(
        mut self,
        facet: &str,
        value: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        if propagation_rule(facet) == FacetPropagation::AtomDeclared {
            return self;
        }
        self.run_scoped.insert(
            facet.to_string(),
            SeedValue {
                value: value.into(),
                field: field.into(),
            },
        );
        self
    }

    /// Pin a value to one named port. Same `AtomDeclared` guard as
    /// [`Self::with_run_scoped`].
    pub fn with_anchor(
        mut self,
        node_id: &str,
        side: PortSide,
        port: &str,
        facet: &str,
        value: impl Into<String>,
        field: impl Into<String>,
    ) -> Self {
        if propagation_rule(facet) == FacetPropagation::AtomDeclared {
            return self;
        }
        self.anchors
            .entry(PortKey::new(node_id, side, port))
            .or_default()
            .insert(
                facet.to_string(),
                SeedValue {
                    value: value.into(),
                    field: field.into(),
                },
            );
        self
    }

    /// Derive the run-scoped tier from the run's own intake facts.
    ///
    /// - `modality` ← `IntakeFacts.modality` (skipped when empty).
    /// - `organism` ← `IntakeFacts.organism_name`.
    /// - `genome_build` ← the pinned reference bundles' `assembly`,
    ///   and `annotation_version` ← their `release` — each only when
    ///   the run pinned exactly one distinct value. A run with two
    ///   assemblies has no single run-level build, so neither is
    ///   seeded and the facet stays `Unknown`.
    ///
    /// `coordinate_system` is deliberately absent: intake pins no
    /// coordinate convention, so it can only ever come from a port
    /// that declares it.
    pub fn from_intake_facts(facts: &crate::intake_facts::IntakeFacts) -> Self {
        let mut seed = Self::new();
        if !facts.modality.trim().is_empty() {
            seed = seed.with_run_scoped("modality", facts.modality.clone(), "IntakeFacts.modality");
        }
        if let Some(organism) = facts
            .organism_name
            .as_ref()
            .filter(|o| !o.trim().is_empty())
        {
            seed = seed.with_run_scoped("organism", organism.clone(), "IntakeFacts.organism_name");
        }
        let assemblies: BTreeSet<&str> = facts
            .pinned_reference_bundles
            .iter()
            .map(|b| b.assembly.trim())
            .filter(|a| !a.is_empty())
            .collect();
        if assemblies.len() == 1 {
            seed = seed.with_run_scoped(
                "genome_build",
                assemblies.iter().next().expect("len checked").to_string(),
                "IntakeFacts.pinned_reference_bundles[].assembly",
            );
        }
        let releases: BTreeSet<&str> = facts
            .pinned_reference_bundles
            .iter()
            .map(|b| b.release.trim())
            .filter(|r| !r.is_empty())
            .collect();
        if releases.len() == 1 {
            seed = seed.with_run_scoped(
                "annotation_version",
                releases.iter().next().expect("len checked").to_string(),
                "IntakeFacts.pinned_reference_bundles[].release",
            );
        }
        seed
    }

    /// True when no value of either tier is set.
    pub fn is_empty(&self) -> bool {
        self.run_scoped.is_empty() && self.anchors.is_empty()
    }
}

/// Read one facet off a port.
fn read_facet(port: &PortContract, facet: &str) -> Option<String> {
    match facet {
        "modality" => port.modality.clone(),
        "organism" => port.organism.clone(),
        "genome_build" => port.genome_build.clone(),
        "annotation_version" => port.annotation_version.clone(),
        "coordinate_system" => port.coordinate_system.clone(),
        "units" => port.units.clone(),
        "normalization_state" => port.normalization_state.clone(),
        "statistical_state" => port.statistical_state.clone(),
        _ => None,
    }
}

/// Write one facet onto a port. No-op for an unknown facet name.
fn write_facet(port: &mut PortContract, facet: &str, value: String) {
    match facet {
        "modality" => port.modality = Some(value),
        "organism" => port.organism = Some(value),
        "genome_build" => port.genome_build = Some(value),
        "annotation_version" => port.annotation_version = Some(value),
        "coordinate_system" => port.coordinate_system = Some(value),
        "units" => port.units = Some(value),
        "normalization_state" => port.normalization_state = Some(value),
        "statistical_state" => port.statistical_state = Some(value),
        _ => {}
    }
}

fn port_of<'a>(node: &'a TaskNode, side: PortSide, name: &str) -> Option<&'a PortContract> {
    let ports = match side {
        PortSide::Input => &node.inputs,
        PortSide::Output => &node.outputs,
    };
    ports.iter().find(|p| p.name == name)
}

fn format_iri(port: &PortContract) -> Option<&str> {
    port.physical_format.as_ref().map(|f| f.iri.as_str())
}

/// Deterministic union-find over `PortKey`s. `BTreeMap`-backed and
/// always iterated in key order, so the component partition — and
/// therefore the assignment list — is byte-stable.
#[derive(Default)]
struct PortUnionFind {
    parent: BTreeMap<PortKey, PortKey>,
}

impl PortUnionFind {
    fn add(&mut self, key: PortKey) {
        self.parent.entry(key.clone()).or_insert(key);
    }

    fn find(&mut self, key: &PortKey) -> PortKey {
        let mut cur = key.clone();
        loop {
            let Some(parent) = self.parent.get(&cur).cloned() else {
                return cur;
            };
            if parent == cur {
                return cur;
            }
            cur = parent;
        }
    }

    fn union(&mut self, a: &PortKey, b: &PortKey) {
        let ra = self.find(a);
        let rb = self.find(b);
        if ra == rb {
            return;
        }
        // Smaller key becomes the root so the partition does not depend
        // on insertion order.
        let (root, child) = if ra < rb { (ra, rb) } else { (rb, ra) };
        self.parent.insert(child, root);
    }
}

/// Propagate the run-scoped facets across `dag`, in place.
///
/// Only unset facets are written; a value declared in atom YAML is
/// never overwritten. Returns the assignments made and the components
/// left unset because their declared values disagreed.
pub fn propagate_run_facets(dag: &mut WorkflowDag, seed: &RunFacetSeed) -> FacetPropagationReport {
    let mut report = FacetPropagationReport::default();
    let node_index: BTreeMap<String, usize> = dag
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();

    // A source node has no incoming edge. Its ports are the run's own
    // inputs and are the only place the intake-derived tier is allowed
    // to land.
    let has_incoming: BTreeSet<&str> = dag.edges.iter().map(|e| e.to_node.as_str()).collect();
    let source_nodes: BTreeSet<String> = dag
        .nodes
        .iter()
        .filter(|n| !has_incoming.contains(n.id.as_str()))
        .map(|n| n.id.clone())
        .collect();

    for facet in PROPAGATED_FACETS {
        let rule = propagation_rule(facet);
        propagate_one_facet(
            dag,
            &node_index,
            &source_nodes,
            seed,
            facet,
            rule,
            &mut report,
        );
    }

    report.assignments.sort_by(|a, b| {
        a.facet
            .cmp(&b.facet)
            .then_with(|| a.port.cmp(&b.port))
            .then_with(|| a.value.cmp(&b.value))
    });
    report
        .conflicts
        .sort_by(|a, b| a.facet.cmp(&b.facet).then_with(|| a.values.cmp(&b.values)));
    report
}

fn propagate_one_facet(
    dag: &mut WorkflowDag,
    node_index: &BTreeMap<String, usize>,
    source_nodes: &BTreeSet<String>,
    seed: &RunFacetSeed,
    facet: &str,
    rule: FacetPropagation,
    report: &mut FacetPropagationReport,
) {
    // Values declared in atom YAML, read before anything is written so
    // an assignment can never be mistaken for a declaration. These are
    // also the ports propagation must not overwrite.
    let mut declared_on_port: BTreeMap<PortKey, String> = BTreeMap::new();
    let mut uf = PortUnionFind::default();
    for node in &dag.nodes {
        for (side, ports) in [
            (PortSide::Input, &node.inputs),
            (PortSide::Output, &node.outputs),
        ] {
            for port in ports {
                let key = PortKey::new(&node.id, side, &port.name);
                uf.add(key.clone());
                if let Some(v) = read_facet(port, facet) {
                    declared_on_port.insert(key, v);
                }
            }
        }
    }
    // Explicit anchors count toward unanimity exactly as a YAML
    // declaration does — a caller naming a reference/annotation port is
    // asserting the same thing the YAML would — but the anchored port
    // is still WRITTEN, since the value is not on the contract yet.
    let mut anchored: BTreeMap<PortKey, SeedValue> = BTreeMap::new();
    for (key, facets) in &seed.anchors {
        if let Some(sv) = facets.get(facet) {
            uf.add(key.clone());
            if !declared_on_port.contains_key(key) {
                anchored.insert(key.clone(), sv.clone());
            }
        }
    }

    link_edges(dag, node_index, rule, &mut uf);
    link_within_nodes(dag, rule, &mut uf);

    // Partition into components.
    let keys: Vec<PortKey> = uf.parent.keys().cloned().collect();
    let mut components: BTreeMap<PortKey, Vec<PortKey>> = BTreeMap::new();
    for key in keys {
        let root = uf.find(&key);
        components.entry(root).or_default().push(key);
    }

    for (_root, members) in components {
        let declared_here: Vec<(&PortKey, &str)> = members
            .iter()
            .filter_map(|k| {
                declared_on_port
                    .get(k)
                    .map(|v| (k, v.as_str()))
                    .or_else(|| anchored.get(k).map(|sv| (k, sv.value.as_str())))
            })
            .collect();
        let distinct: BTreeSet<&str> = declared_here.iter().map(|(_, v)| *v).collect();

        let (value, origin) = match distinct.len() {
            0 => {
                // Nothing declared anywhere in this component. The
                // intake tier applies only when the component actually
                // touches a run input.
                let touches_source = members.iter().any(|k| source_nodes.contains(&k.node_id));
                match seed.run_scoped.get(facet) {
                    Some(sv) if touches_source => (
                        sv.value.clone(),
                        FacetOrigin::RunScopedSeed {
                            field: sv.field.clone(),
                        },
                    ),
                    _ => continue,
                }
            }
            1 => {
                let value = (*distinct.iter().next().expect("len checked")).to_string();
                // Attribute to the lowest-keyed declaring port so the
                // rationale is stable across runs.
                let origin_port = declared_here
                    .iter()
                    .map(|(k, _)| *k)
                    .min()
                    .expect("len checked")
                    .clone();
                let origin = match anchored.get(&origin_port) {
                    Some(sv) => FacetOrigin::RunScopedSeed {
                        field: sv.field.clone(),
                    },
                    None => FacetOrigin::DeclaredOnPort {
                        origin: origin_port,
                    },
                };
                (value, origin)
            }
            _ => {
                report.conflicts.push(FacetConflict {
                    facet: facet.to_string(),
                    values: distinct.iter().map(|v| (*v).to_string()).collect(),
                    declaring_ports: {
                        let mut ports: Vec<PortKey> =
                            declared_here.iter().map(|(k, _)| (*k).clone()).collect();
                        ports.sort();
                        ports
                    },
                });
                continue;
            }
        };

        for key in &members {
            if declared_on_port.contains_key(key) {
                continue;
            }
            let Some(idx) = node_index.get(&key.node_id) else {
                continue;
            };
            let node = &mut dag.nodes[*idx];
            let ports = match key.side {
                PortSide::Input => &mut node.inputs,
                PortSide::Output => &mut node.outputs,
            };
            let Some(port) = ports.iter_mut().find(|p| p.name == key.port) else {
                continue;
            };
            if read_facet(port, facet).is_some() {
                continue;
            }
            write_facet(port, facet, value.clone());
            report.assignments.push(FacetAssignment {
                port: key.clone(),
                facet: facet.to_string(),
                value: value.clone(),
                origin: origin.clone(),
            });
        }
    }
}

/// Join the two endpoints of every data-flow edge.
fn link_edges(
    dag: &WorkflowDag,
    node_index: &BTreeMap<String, usize>,
    rule: FacetPropagation,
    uf: &mut PortUnionFind,
) {
    for edge in &dag.edges {
        let (Some(pi), Some(ci)) = (
            node_index.get(&edge.from_node),
            node_index.get(&edge.to_node),
        ) else {
            continue;
        };
        let producer = &dag.nodes[*pi];
        let consumer = &dag.nodes[*ci];
        let (Some(pp), Some(cp)) = (
            port_of(producer, PortSide::Output, &edge.from_port),
            port_of(consumer, PortSide::Input, &edge.to_port),
        ) else {
            // Structural edges (splices, ordering-only wiring) name
            // ports that do not exist on either node; they carry no
            // data, so they carry no facet either.
            continue;
        };
        if !may_link(rule, pp, cp) {
            continue;
        }
        uf.union(
            &PortKey::new(&edge.from_node, PortSide::Output, &edge.from_port),
            &PortKey::new(&edge.to_node, PortSide::Input, &edge.to_port),
        );
    }
}

/// Join each node's inputs to its outputs — the data a stage emits is
/// derived from the data it read, so a run-invariant facet crosses the
/// stage.
fn link_within_nodes(dag: &WorkflowDag, rule: FacetPropagation, uf: &mut PortUnionFind) {
    for node in &dag.nodes {
        for input in &node.inputs {
            for output in &node.outputs {
                if !may_link(rule, input, output) {
                    continue;
                }
                uf.union(
                    &PortKey::new(&node.id, PortSide::Input, &input.name),
                    &PortKey::new(&node.id, PortSide::Output, &output.name),
                );
            }
        }
    }
}

fn may_link(rule: FacetPropagation, a: &PortContract, b: &PortContract) -> bool {
    match rule {
        FacetPropagation::RunInvariant => true,
        // A coordinate convention belongs to the format. Two ports of
        // the same format share it; a conversion does not.
        FacetPropagation::FormatScoped => match (format_iri(a), format_iri(b)) {
            (Some(x), Some(y)) => x == y,
            _ => false,
        },
        FacetPropagation::AtomDeclared => false,
    }
}

/// Indices into `edges` of the DAG's terminal edges — those whose
/// consumer has no outgoing edge. Shared with `facet_coverage` so both
/// agree on what "terminal" means.
pub fn terminal_edge_indices(edges: &[EdgeContract]) -> Vec<usize> {
    let has_outgoing: BTreeSet<&str> = edges.iter().map(|e| e.from_node.as_str()).collect();
    edges
        .iter()
        .enumerate()
        .filter(|(_, e)| !has_outgoing.contains(e.to_node.as_str()))
        .map(|(i, _)| i)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow_contracts::edge::EdgeContract;
    use crate::workflow_contracts::port::FormatRef;
    use crate::workflow_contracts::semantic_type::SemanticType;

    fn port(name: &str, iri: &str, fmt: Option<&str>) -> PortContract {
        PortContract {
            name: name.into(),
            semantic_type: SemanticType::edam(iri, ""),
            physical_format: fmt.map(|f| FormatRef {
                iri: f.into(),
                label: None,
                extension: None,
            }),
            ..Default::default()
        }
    }

    fn node(id: &str, inputs: Vec<PortContract>, outputs: Vec<PortContract>) -> TaskNode {
        let mut n = TaskNode::skeleton(id, id);
        n.inputs = inputs;
        n.outputs = outputs;
        n
    }

    fn edge(from: &str, from_port: &str, to: &str, to_port: &str) -> EdgeContract {
        EdgeContract {
            from_node: from.into(),
            from_port: from_port.into(),
            to_node: to.into(),
            to_port: to_port.into(),
            ..Default::default()
        }
    }

    fn chain() -> WorkflowDag {
        WorkflowDag {
            id: "t".into(),
            nodes: vec![
                node(
                    "acquire",
                    vec![],
                    vec![port("counts", "data:3917", Some("format:3475"))],
                ),
                node(
                    "normalise",
                    vec![port("counts", "data:3917", Some("format:3475"))],
                    vec![port("normalized", "data:3917", Some("format:3475"))],
                ),
                node(
                    "de",
                    vec![port("normalized", "data:3917", Some("format:3475"))],
                    vec![port("results", "data:3134", Some("format:3475"))],
                ),
            ],
            edges: vec![
                edge("acquire", "counts", "normalise", "counts"),
                edge("normalise", "normalized", "de", "normalized"),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn declared_value_reaches_every_port_in_the_component() {
        let mut dag = chain();
        dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
        let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
        for n in &dag.nodes {
            for p in n.inputs.iter().chain(n.outputs.iter()) {
                assert_eq!(
                    p.organism.as_deref(),
                    Some("Homo sapiens"),
                    "{}.{} did not inherit organism",
                    n.id,
                    p.name
                );
            }
        }
        assert_eq!(report.assigned_count(), 4, "{:?}", report.assignments);
        assert!(report.conflicts.is_empty());
    }

    #[test]
    fn nothing_declared_and_no_seed_leaves_every_facet_unset() {
        let mut dag = chain();
        let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
        assert!(report.assignments.is_empty());
        for n in &dag.nodes {
            for p in n.inputs.iter().chain(n.outputs.iter()) {
                assert!(p.organism.is_none());
                assert!(p.genome_build.is_none());
                assert!(p.modality.is_none());
            }
        }
    }

    #[test]
    fn run_scoped_seed_only_lands_on_a_component_touching_a_source() {
        let mut dag = chain();
        // `island` has no edges at all, so it is a source too — give it
        // an incoming edge from `de` so it is provably not one.
        dag.nodes.push(node(
            "island",
            vec![port("bundle", "data:2048", Some("format:3464"))],
            vec![],
        ));
        dag.edges.push(edge("de", "results", "island", "bundle"));
        let seed = RunFacetSeed::new().with_run_scoped("organism", "Mus musculus", "test");
        propagate_run_facets(&mut dag, &seed);
        // Whole chain is one component reaching the `acquire` source.
        assert_eq!(
            dag.nodes[3].inputs[0].organism.as_deref(),
            Some("Mus musculus")
        );
    }

    #[test]
    fn declared_value_beats_the_run_scoped_seed() {
        let mut dag = chain();
        dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
        let seed = RunFacetSeed::new().with_run_scoped("organism", "Mus musculus", "test");
        propagate_run_facets(&mut dag, &seed);
        assert_eq!(
            dag.nodes[2].outputs[0].organism.as_deref(),
            Some("Homo sapiens")
        );
    }

    #[test]
    fn disagreeing_declarations_assign_nothing_and_record_a_conflict() {
        let mut dag = chain();
        dag.nodes[0].outputs[0].genome_build = Some("GRCh37".into());
        dag.nodes[2].outputs[0].genome_build = Some("GRCh38".into());
        let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
        assert!(
            dag.nodes[1].inputs[0].genome_build.is_none(),
            "a disagreeing component must stay unset"
        );
        let c = report
            .conflicts
            .iter()
            .find(|c| c.facet == "genome_build")
            .expect("conflict recorded");
        assert_eq!(c.values, vec!["GRCh37".to_string(), "GRCh38".to_string()]);
    }

    #[test]
    fn propagation_never_creates_a_facet_mismatch_on_an_edge() {
        // Two declared modalities meeting at a join node: the join's
        // ports stay unset rather than picking a side.
        let mut dag = WorkflowDag {
            id: "join".into(),
            nodes: vec![
                node("rna", vec![], vec![port("counts", "data:3917", None)]),
                node("atac", vec![], vec![port("peaks", "data:1255", None)]),
                node(
                    "integrate",
                    vec![
                        port("counts", "data:3917", None),
                        port("peaks", "data:1255", None),
                    ],
                    vec![port("joint", "data:2048", None)],
                ),
            ],
            edges: vec![
                edge("rna", "counts", "integrate", "counts"),
                edge("atac", "peaks", "integrate", "peaks"),
            ],
            ..Default::default()
        };
        dag.nodes[0].outputs[0].modality = Some("bulk_rnaseq".into());
        dag.nodes[1].outputs[0].modality = Some("atac_seq".into());
        let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
        assert!(dag.nodes[2].inputs[0].modality.is_none());
        assert!(dag.nodes[2].inputs[1].modality.is_none());
        assert!(dag.nodes[2].outputs[0].modality.is_none());
        assert!(report.conflicts.iter().any(|c| c.facet == "modality"));
    }

    #[test]
    fn coordinate_system_stops_at_a_format_change() {
        let mut dag = WorkflowDag {
            id: "fmt".into(),
            nodes: vec![
                node(
                    "align",
                    vec![],
                    vec![port("bam", "data:0863", Some("format:2572"))],
                ),
                node(
                    "count",
                    vec![port("bam", "data:0863", Some("format:2572"))],
                    vec![port("matrix", "data:3917", Some("format:3475"))],
                ),
            ],
            edges: vec![edge("align", "bam", "count", "bam")],
            ..Default::default()
        };
        dag.nodes[0].outputs[0].coordinate_system = Some("1-based-inclusive".into());
        propagate_run_facets(&mut dag, &RunFacetSeed::new());
        assert_eq!(
            dag.nodes[1].inputs[0].coordinate_system.as_deref(),
            Some("1-based-inclusive"),
            "same-format edge carries the coordinate system"
        );
        assert!(
            dag.nodes[1].outputs[0].coordinate_system.is_none(),
            "a format change must not carry the coordinate system across"
        );
    }

    #[test]
    fn atom_declared_facets_are_never_propagated() {
        let mut dag = chain();
        dag.nodes[0].outputs[0].normalization_state = Some("raw".into());
        dag.nodes[0].outputs[0].units = Some("read counts".into());
        propagate_run_facets(&mut dag, &RunFacetSeed::new());
        assert!(dag.nodes[1].inputs[0].normalization_state.is_none());
        assert!(dag.nodes[1].inputs[0].units.is_none());
    }

    #[test]
    fn seed_refuses_atom_declared_facets() {
        let seed = RunFacetSeed::new()
            .with_run_scoped("normalization_state", "normalized", "test")
            .with_run_scoped("units", "TPM", "test");
        assert!(seed.is_empty());
    }

    #[test]
    fn propagation_is_deterministic_across_repeats() {
        let mut first: Option<String> = None;
        for _ in 0..25 {
            let mut dag = chain();
            dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
            let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
            let json = serde_json::to_string(&report).unwrap();
            if let Some(prev) = &first {
                assert_eq!(prev, &json, "propagation report is not deterministic");
            }
            first = Some(json);
        }
    }

    #[test]
    fn intake_seed_reads_organism_and_a_single_pinned_bundle() {
        use crate::intake_facts::{IntakeFacts, PinnedReferenceBundle};
        let facts = IntakeFacts {
            modality: "bulk_rnaseq".into(),
            organism_name: Some("Homo sapiens".into()),
            pinned_reference_bundles: vec![PinnedReferenceBundle {
                assembly: "GRCh38.p14".into(),
                release: "Ensembl 115".into(),
                content_hash: "sha256:0".into(),
            }],
            ..Default::default()
        };
        let seed = RunFacetSeed::from_intake_facts(&facts);
        assert!(!seed.is_empty());
        let mut dag = chain();
        propagate_run_facets(&mut dag, &seed);
        let p = &dag.nodes[2].outputs[0];
        assert_eq!(p.organism.as_deref(), Some("Homo sapiens"));
        assert_eq!(p.genome_build.as_deref(), Some("GRCh38.p14"));
        assert_eq!(p.annotation_version.as_deref(), Some("Ensembl 115"));
        assert_eq!(p.modality.as_deref(), Some("bulk_rnaseq"));
    }

    #[test]
    fn two_pinned_assemblies_seed_no_genome_build() {
        use crate::intake_facts::{IntakeFacts, PinnedReferenceBundle};
        let facts = IntakeFacts {
            modality: "bulk_rnaseq".into(),
            pinned_reference_bundles: vec![
                PinnedReferenceBundle {
                    assembly: "GRCh38.p14".into(),
                    release: "Ensembl 115".into(),
                    content_hash: "sha256:0".into(),
                },
                PinnedReferenceBundle {
                    assembly: "GRCm39".into(),
                    release: "Ensembl 115".into(),
                    content_hash: "sha256:1".into(),
                },
            ],
            ..Default::default()
        };
        let seed = RunFacetSeed::from_intake_facts(&facts);
        let mut dag = chain();
        propagate_run_facets(&mut dag, &seed);
        assert!(dag.nodes[0].outputs[0].genome_build.is_none());
        // A single distinct release across both bundles still seeds.
        assert_eq!(
            dag.nodes[0].outputs[0].annotation_version.as_deref(),
            Some("Ensembl 115")
        );
    }

    #[test]
    fn anchor_pins_a_named_reference_port() {
        let mut dag = chain();
        let seed = RunFacetSeed::new().with_anchor(
            "normalise",
            PortSide::Input,
            "counts",
            "genome_build",
            "GRCh38.p14",
            "test-anchor",
        );
        propagate_run_facets(&mut dag, &seed);
        assert_eq!(
            dag.nodes[2].outputs[0].genome_build.as_deref(),
            Some("GRCh38.p14")
        );
    }

    #[test]
    fn terminal_edges_are_the_edges_into_sinks() {
        let dag = chain();
        let terminal = terminal_edge_indices(&dag.edges);
        assert_eq!(terminal, vec![1]);
    }
}
