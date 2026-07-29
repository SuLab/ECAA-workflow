//! `collapse_one_of_gaps` — the meet-in-the-middle post-pass that
//! reconciles `AtomDefinition.input_groups` (one-of substrate choices,
//! e.g. `differential_expression`'s `counts` group over raw|normalized
//! count matrices) against the per-input-port `MissingProducer` gaps the
//! main loop records.
//!
//! An unbound member of a satisfied one-of group is a legitimate unused
//! alternative, not a compositional defect — the per-member gap must be
//! dropped. An under-bound group still needs exactly ONE group-level
//! gap so the SME / repair registry sees a single actionable signal
//! instead of N noisy per-member proposals.

use ecaa_workflow_core::atom::AtomDefinition;
use ecaa_workflow_core::composer_v4::meet_in_middle::collapse_one_of_gaps;
use ecaa_workflow_core::repair::proposal::RepairGap;
use ecaa_workflow_core::repair::strategy::GapKind;
use ecaa_workflow_core::workflow_contracts::edge::{CompatibilityProof, EdgeContract, EdgeKind};

const CONSUMER_ID: &str = "differential_expression";

/// A consumer atom declaring a `counts` one-of group over
/// `raw_counts` / `normalized_counts`, mirroring the DE count-substrate
/// closure this task exists to support.
fn consumer_with_counts_one_of() -> AtomDefinition {
    let yaml = r#"
id: differential_expression
version: "1.0.0"
role: operation
description: "test fixture: DE atom with a one-of counts substrate group"
edam_operation: "operation:3223"
edam_data: "data:0951"
edam_format: "format:3475"
assignee: agent
inputs: []
outputs: []
input_groups:
  - name: counts
    kind: one_of
    members: [raw_counts, normalized_counts]
    min_bound: 1
"#;
    serde_yaml_ng::from_str(yaml).expect("fixture atom must deserialize")
}

fn bound_edge(to_port: &str) -> EdgeContract {
    EdgeContract {
        from_node: "data_acquisition".into(),
        from_port: format!("{to_port}_out"),
        to_node: CONSUMER_ID.into(),
        to_port: to_port.into(),
        proof: CompatibilityProof::default(),
        kind: EdgeKind::TypedDataFlow,
        chain_of_custody: None,
        mutually_exclusive_group: None,
    }
}

fn missing_producer_gap(id: &str, consumer_port: &str) -> RepairGap {
    RepairGap {
        id: id.into(),
        statement: format!(
            "no compatible producer found for consumer {CONSUMER_ID}'s input port {consumer_port}"
        ),
        kind: GapKind::MissingProducer,
        consumer_node: CONSUMER_ID.into(),
        consumer_port: consumer_port.into(),
        producer_node: None,
        producer_port: None,
        facet_mismatches: Vec::new(),
    }
}

/// One member (`raw_counts`) bound, the other (`normalized_counts`)
/// recorded a `MissingProducer` gap by the per-input-port loop.
fn fixture_one_bound() -> (
    AtomDefinition,
    Vec<EdgeContract>,
    Vec<String>,
    Vec<RepairGap>,
) {
    let consumer = consumer_with_counts_one_of();
    let edges = vec![bound_edge("raw_counts")];
    let gap_id = format!("{CONSUMER_ID}:1");
    let gaps = vec![gap_id.clone()];
    let repair = vec![missing_producer_gap(&gap_id, "normalized_counts")];
    (consumer, edges, gaps, repair)
}

/// Neither member bound — both recorded a `MissingProducer` gap.
fn fixture_zero_bound() -> (
    AtomDefinition,
    Vec<EdgeContract>,
    Vec<String>,
    Vec<RepairGap>,
) {
    let consumer = consumer_with_counts_one_of();
    let edges: Vec<EdgeContract> = vec![];
    let raw_id = format!("{CONSUMER_ID}:0");
    let norm_id = format!("{CONSUMER_ID}:1");
    let gaps = vec![raw_id.clone(), norm_id.clone()];
    let repair = vec![
        missing_producer_gap(&raw_id, "raw_counts"),
        missing_producer_gap(&norm_id, "normalized_counts"),
    ];
    (consumer, edges, gaps, repair)
}

#[test]
fn one_of_with_one_bound_member_drops_sibling_gap_and_tags_edge() {
    let (consumer, mut edges, mut gaps, mut repair) = fixture_one_bound();
    collapse_one_of_gaps(&consumer, &mut edges, &mut gaps, &mut repair, CONSUMER_ID);

    assert!(
        repair
            .iter()
            .all(|g| g.consumer_port != "normalized_counts"),
        "unbound sibling gap must be collapsed, got {repair:?}"
    );
    assert!(
        gaps.iter().all(|id| id != &format!("{CONSUMER_ID}:1")),
        "unbound sibling's legacy string gap must be collapsed too, got {gaps:?}"
    );
    assert_eq!(
        edges[0].mutually_exclusive_group.as_deref(),
        Some("counts"),
        "bound member edge must carry the group tag"
    );
}

#[test]
fn one_of_with_zero_bound_members_yields_single_group_gap() {
    let (consumer, mut edges, mut gaps, mut repair) = fixture_zero_bound();
    collapse_one_of_gaps(&consumer, &mut edges, &mut gaps, &mut repair, CONSUMER_ID);

    let count = repair
        .iter()
        .filter(|g| g.consumer_port.starts_with("counts"))
        .count();
    assert_eq!(
        count, 1,
        "zero-bound one-of must leave exactly one group-level gap, got {repair:?}"
    );
    // The per-member gaps must be gone from BOTH the structured and the
    // legacy string gap lists.
    assert!(repair
        .iter()
        .all(|g| g.consumer_port != "raw_counts" && g.consumer_port != "normalized_counts"));
    assert!(gaps
        .iter()
        .all(|id| id != &format!("{CONSUMER_ID}:0") && id != &format!("{CONSUMER_ID}:1")));
}
