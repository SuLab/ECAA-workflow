//! Semantic-facet coverage on the bulk RNA-seq roster.
//!
//! A typed-data-flow proof establishes EDAM port-type subsumption plus a
//! per-facet unification for each facet the two contracts declare. Facets
//! neither side declares unify to `Unknown` — an honest "undecided", not a
//! pass. These tests pin the two ways a facet becomes decided:
//!
//! - **atom-static** facets (`units`, `normalization_state`,
//!   `statistical_state`) are declared in the atom YAML, because they are
//!   properties of the stage's contract in every run it appears in;
//! - **run-scoped** facets (`organism`, `genome_build`,
//!   `annotation_version`, `coordinate_system`, and `modality` on the
//!   atoms shared across modalities) are PROPAGATED from the run's own
//!   reference / annotation inputs, never declared on a shared atom.
//!
//! and the warn-only coverage measure over the result.

use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::compatibility::facet_coverage::{
    facet_coverage, facet_coverage_advisory, terminal_facet_coverage, FacetCoverageScope,
    FACET_COVERAGE_ADVISORY_FLOOR,
};
use ecaa_workflow_core::compatibility::facet_propagation::{propagate_run_facets, RunFacetSeed};
use ecaa_workflow_core::compatibility::{
    CompatibilityEngine, CompatibilityResult, DeterministicCompatibilityEngine, PlanningContext,
};
use ecaa_workflow_core::intake_facts::{IntakeFacts, PinnedReferenceBundle};
use ecaa_workflow_core::workflow_contracts::edge::{EdgeContract, FacetMatchKind};
use ecaa_workflow_core::workflow_contracts::port::{FormatRef, PortContract};
use ecaa_workflow_core::workflow_contracts::semantic_type::SemanticType;
use ecaa_workflow_core::workflow_contracts::task_node::{TaskNode, WorkflowDag};
use std::path::{Path, PathBuf};

fn config_stage_atoms() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/stage-atoms")
}

/// Lift one catalog atom into a `TaskNode` carrying its rich ports —
/// the same preference `composer_v4::planner::lift_to_workflow_dag`
/// applies (rich `inputs:`/`outputs:` over the coarse `edam_data`
/// synthesis).
fn lift(reg: &AtomRegistry, atom_id: &str) -> TaskNode {
    let atom = reg
        .get(atom_id)
        .unwrap_or_else(|| panic!("{atom_id} must be in the catalog"));
    let mut node = TaskNode::from_atom(atom);
    if !atom.inputs.is_empty() {
        node.inputs = atom.inputs.clone();
    }
    if !atom.outputs.is_empty() {
        node.outputs = atom.outputs.clone();
    }
    node
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

/// The alignment-path bulk RNA-seq spine, wired the way
/// `config/archetypes/bulk_rnaseq_de.yaml` declares it.
///
/// The `differential_expression` → `pathway_enrichment` edge is wired to
/// a `residual_in_*` port, which is what a real composition binds: the
/// declared `ranked_de_results` port is typed `data:0951` while the DE
/// output is `data:3134`, so `discover_companion_synthesis` appends a
/// full clone of the producer's output port and binds that instead (see
/// `runtime/proofs.jsonl` in any emitted bulk RNA-seq package). The clone
/// carries the producer's facets verbatim, which is precisely why an
/// atom-static `units:` on the DE output reaches the proof.
fn bulk_rnaseq_spine() -> WorkflowDag {
    let reg = AtomRegistry::load_from_dir(&config_stage_atoms()).expect("atom registry must load");
    let mut pathway_enrichment = lift(&reg, "pathway_enrichment");
    let differential_expression = lift(&reg, "differential_expression");
    let de_output = differential_expression
        .outputs
        .iter()
        .find(|p| p.name == "de_results")
        .expect("differential_expression must declare a de_results output")
        .clone();
    let residual_name = format!("residual_in_{}", pathway_enrichment.inputs.len());
    pathway_enrichment.inputs.push(PortContract {
        name: residual_name.clone(),
        ..de_output
    });
    WorkflowDag {
        id: "bulk_rnaseq_de".into(),
        nodes: vec![
            lift(&reg, "quantification"),
            lift(&reg, "qc_preprocessing"),
            lift(&reg, "normalisation"),
            differential_expression,
            pathway_enrichment,
        ],
        edges: vec![
            edge(
                "quantification",
                "count_matrix",
                "qc_preprocessing",
                "count_matrix",
            ),
            edge(
                "qc_preprocessing",
                "filtered_count_matrix",
                "normalisation",
                "count_matrix",
            ),
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
            edge(
                "differential_expression",
                "de_results",
                "pathway_enrichment",
                &residual_name,
            ),
        ],
        ..Default::default()
    }
}

/// Port name the spine's terminal edge binds on `pathway_enrichment`,
/// read off the wiring rather than hardcoded.
fn terminal_consumer_port(dag: &WorkflowDag) -> String {
    dag.edges
        .last()
        .expect("the spine has edges")
        .to_port
        .clone()
}

fn himes_like_intake() -> IntakeFacts {
    IntakeFacts {
        modality: "bulk_rnaseq".into(),
        organism_name: Some("Homo sapiens".into()),
        pinned_reference_bundles: vec![PinnedReferenceBundle {
            assembly: "GRCh38.p14".into(),
            release: "Ensembl 115".into(),
            content_hash: "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                .into(),
        }],
        ..Default::default()
    }
}

fn output_port<'a>(dag: &'a WorkflowDag, node: &str, port: &str) -> &'a PortContract {
    dag.nodes
        .iter()
        .find(|n| n.id == node)
        .unwrap_or_else(|| panic!("node {node}"))
        .outputs
        .iter()
        .find(|p| p.name == port)
        .unwrap_or_else(|| panic!("output {node}.{port}"))
}

fn input_port<'a>(dag: &'a WorkflowDag, node: &str, port: &str) -> &'a PortContract {
    dag.nodes
        .iter()
        .find(|n| n.id == node)
        .unwrap_or_else(|| panic!("node {node}"))
        .inputs
        .iter()
        .find(|p| p.name == port)
        .unwrap_or_else(|| panic!("input {node}.{port}"))
}

/// The terminal edge of the bulk RNA-seq spine (`differential_expression`
/// → `pathway_enrichment`) must have BOTH endpoints declaring `modality`
/// and `units` once the run-scoped facets are propagated:
///
/// - `units` comes from the atom YAML on both sides — a DE table is in
///   log2 fold change and the enrichment input reads exactly that;
/// - `modality` cannot come from the atom YAML. Every atom on this spine
///   is shared across modalities (`differential_expression` appears in 8
///   archetypes, `alignment` in 16), so a literal `modality:` on their
///   ports would refuse every other archetype. It is propagated from the
///   run's own intake instead.
#[test]
fn bulk_rnaseq_terminal_edges_declare_modality_and_units() {
    let mut dag = bulk_rnaseq_spine();

    // Before propagation `modality` is nowhere on the spine — this is the
    // gap, and asserting it keeps the test honest about what propagation
    // actually contributes.
    assert!(
        dag.nodes
            .iter()
            .flat_map(|n| n.inputs.iter().chain(n.outputs.iter()))
            .all(|p| p.modality.is_none()),
        "no shared bulk RNA-seq atom may hard-declare a modality"
    );

    let seed = RunFacetSeed::from_intake_facts(&himes_like_intake());
    let report = propagate_run_facets(&mut dag, &seed);
    assert!(
        report.conflicts.is_empty(),
        "single-modality spine must not conflict: {:?}",
        report.conflicts
    );

    let terminal = terminal_facet_coverage(&dag);
    assert_eq!(terminal.scope, FacetCoverageScope::TerminalEdges);
    assert_eq!(
        terminal.edges_considered, 1,
        "differential_expression → pathway_enrichment is the only terminal edge"
    );

    // The declared enrichment input carries the atom-static units too,
    // so the pair agrees whenever the composer does bind it.
    assert_eq!(
        input_port(&dag, "pathway_enrichment", "ranked_de_results")
            .units
            .as_deref(),
        Some("log2 fold change"),
        "atom-static units must be declared on the enrichment input"
    );

    let terminal_port = terminal_consumer_port(&dag);
    let producer = output_port(&dag, "differential_expression", "de_results");
    let consumer = input_port(&dag, "pathway_enrichment", &terminal_port);

    assert_eq!(
        producer.units.as_deref(),
        Some("log2 fold change"),
        "atom-static units must be declared on the DE output"
    );
    assert_eq!(
        consumer.units.as_deref(),
        Some("log2 fold change"),
        "the bound terminal consumer must carry the same units"
    );
    assert_eq!(
        producer.modality.as_deref(),
        Some("bulk_rnaseq"),
        "modality must reach the terminal producer by propagation"
    );
    assert_eq!(
        consumer.modality.as_deref(),
        Some("bulk_rnaseq"),
        "modality must reach the terminal consumer by propagation"
    );

    // Both facets are decided by agreement, not by an unconstrained
    // consumer.
    assert_eq!(terminal.per_facet["units"].exact_both_declared, 1);
    assert_eq!(terminal.per_facet["modality"].exact_both_declared, 1);

    // And the engine agrees when it actually proves the edge.
    let engine = DeterministicCompatibilityEngine::new();
    match engine.prove(producer, consumer, &PlanningContext::default()) {
        CompatibilityResult::Compatible(proof) => {
            for facet in ["modality", "units"] {
                let fm = proof
                    .facet_matches
                    .iter()
                    .find(|f| f.facet == facet)
                    .unwrap_or_else(|| panic!("{facet} must be surfaced in the proof"));
                assert!(
                    matches!(fm.kind, FacetMatchKind::Exact),
                    "{facet} must unify Exact, got {:?}",
                    fm.kind
                );
                assert!(
                    !fm.producer.is_empty() && !fm.consumer.is_empty(),
                    "{facet} must record both declared values, got {fm:?}"
                );
            }
        }
        other => panic!("terminal edge must be compatible, got {other:?}"),
    }
}

/// `organism` is a property of the RUN, not of any atom. It reaches the
/// downstream ports only because the run's own annotation input declared
/// it — nothing invents it, and with no annotation input the facet stays
/// unknown.
#[test]
fn organism_propagates_from_the_annotation_input() {
    fn port(name: &str, iri: &str, fmt: &str) -> PortContract {
        PortContract {
            name: name.into(),
            semantic_type: SemanticType::edam(iri, ""),
            physical_format: Some(FormatRef {
                iri: fmt.into(),
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

    // The run's pinned annotation bundle is a real node in the graph: a
    // GTF whose organism and annotation version are known because the
    // reference itself is what pins them.
    let build = || WorkflowDag {
        id: "annotation-propagation".into(),
        nodes: vec![
            node(
                "reference_bundle",
                vec![],
                vec![port("annotation", "data:1255", "format:2306")],
            ),
            node(
                "quantification",
                vec![port("annotation", "data:1255", "format:2306")],
                vec![port("count_matrix", "data:3917", "format:3475")],
            ),
            node(
                "differential_expression",
                vec![port("raw_counts", "data:3917", "format:3475")],
                vec![port("de_results", "data:3134", "format:3475")],
            ),
        ],
        edges: vec![
            edge(
                "reference_bundle",
                "annotation",
                "quantification",
                "annotation",
            ),
            edge(
                "quantification",
                "count_matrix",
                "differential_expression",
                "raw_counts",
            ),
        ],
        ..Default::default()
    };

    // Control: without the annotation declaration and with no seed,
    // organism stays unknown everywhere. Propagation invents nothing.
    let mut bare = build();
    propagate_run_facets(&mut bare, &RunFacetSeed::new());
    assert!(
        bare.nodes
            .iter()
            .flat_map(|n| n.inputs.iter().chain(n.outputs.iter()))
            .all(|p| p.organism.is_none()),
        "with nothing declared, organism must stay unknown"
    );
    assert_eq!(facet_coverage(&bare).per_facet["organism"].unknown, 2);

    // With the annotation input declaring it, the value reaches every
    // port derived from that annotation.
    let mut dag = build();
    dag.nodes[0].outputs[0].organism = Some("Homo sapiens".into());
    dag.nodes[0].outputs[0].annotation_version = Some("GENCODE 44".into());
    let report = propagate_run_facets(&mut dag, &RunFacetSeed::new());
    assert!(report.conflicts.is_empty(), "{:?}", report.conflicts);

    for (node_id, is_output, port_name) in [
        ("quantification", false, "annotation"),
        ("quantification", true, "count_matrix"),
        ("differential_expression", false, "raw_counts"),
        ("differential_expression", true, "de_results"),
    ] {
        let p = if is_output {
            output_port(&dag, node_id, port_name)
        } else {
            input_port(&dag, node_id, port_name)
        };
        assert_eq!(
            p.organism.as_deref(),
            Some("Homo sapiens"),
            "{node_id}.{port_name} did not inherit organism from the annotation input"
        );
        assert_eq!(
            p.annotation_version.as_deref(),
            Some("GENCODE 44"),
            "{node_id}.{port_name} did not inherit annotation_version"
        );
    }

    // The propagated value is attributed, not anonymous.
    assert!(
        report
            .assignments
            .iter()
            .any(|a| a.facet == "organism" && a.value == "Homo sapiens"),
        "every propagated value must be recorded in the report"
    );

    // The engine now proves the terminal edge with an Exact organism.
    let engine = DeterministicCompatibilityEngine::new();
    let producer = output_port(&dag, "quantification", "count_matrix");
    let consumer = input_port(&dag, "differential_expression", "raw_counts");
    match engine.prove(producer, consumer, &PlanningContext::default()) {
        CompatibilityResult::Compatible(proof) => {
            let fm = proof
                .facet_matches
                .iter()
                .find(|f| f.facet == "organism")
                .expect("organism must be surfaced");
            assert!(matches!(fm.kind, FacetMatchKind::Exact));
            assert_eq!(fm.producer, "Homo sapiens");
            assert_eq!(fm.consumer, "Homo sapiens");
        }
        other => panic!("expected Compatible, got {other:?}"),
    }

    // coordinate_system is NOT run-invariant: it belongs to the format,
    // and the GTF → count-matrix conversion must not carry one across.
    let mut coords = build();
    coords.nodes[0].outputs[0].coordinate_system = Some("1-based-inclusive".into());
    propagate_run_facets(&mut coords, &RunFacetSeed::new());
    assert_eq!(
        input_port(&coords, "quantification", "annotation")
            .coordinate_system
            .as_deref(),
        Some("1-based-inclusive"),
        "same-format edge carries the coordinate system"
    );
    assert!(
        output_port(&coords, "quantification", "count_matrix")
            .coordinate_system
            .is_none(),
        "a format change must not carry a coordinate convention across"
    );
}

/// The coverage measure reports the fraction of facet checks decided by
/// agreement on both sides, and is ADVISORY: it never refuses an edge and
/// never blocks emission.
#[test]
fn facet_coverage_invariant_reports_fraction_exact() {
    let mut dag = bulk_rnaseq_spine();

    let before = facet_coverage(&dag);
    assert_eq!(
        before.checks_total,
        before.edges_considered * 8,
        "every considered edge contributes one check per unified facet"
    );
    // The arithmetic is exactly `exact_both_declared / checks_total`.
    let expect_before = before.exact_both_declared as f64 / before.checks_total as f64;
    assert!((before.fraction_exact() - expect_before).abs() < 1e-12);
    // A consumer that declares nothing agreed to nothing, so a
    // producer-only declaration is never folded into the exact bucket.
    let bucket_sum = before.exact_both_declared
        + before.producer_only
        + before.unknown
        + before.subtype
        + before.substituted
        + before.incompatible;
    assert_eq!(bucket_sum, before.checks_total);
    assert_eq!(
        before.incompatible, 0,
        "the atom-static declarations must not make any spine edge incompatible"
    );

    let seed = RunFacetSeed::from_intake_facts(&himes_like_intake());
    propagate_run_facets(&mut dag, &seed);
    let after = facet_coverage(&dag);

    assert_eq!(after.checks_total, before.checks_total);
    assert_eq!(after.incompatible, 0);
    assert!(
        after.exact_both_declared > before.exact_both_declared,
        "propagating the run-scoped facets must decide checks that were \
         previously unknown: {} -> {}",
        before.exact_both_declared,
        after.exact_both_declared
    );
    assert!(after.fraction_exact() > before.fraction_exact());
    assert!(after.unknown < before.unknown);

    // modality / organism / genome_build / annotation_version are decided
    // on every spine edge once the run facets are propagated.
    for facet in ["modality", "organism", "genome_build", "annotation_version"] {
        assert_eq!(
            after.per_facet[facet].exact_both_declared, after.edges_considered,
            "{facet} must be decided on every spine edge"
        );
        assert_eq!(after.per_facet[facet].unknown, 0);
    }

    // Advisory, not a gate: asking for the verdict changes nothing, and
    // the result is a message or nothing at all.
    let snapshot = after.clone();
    let verdict = facet_coverage_advisory(&after, FACET_COVERAGE_ADVISORY_FLOOR);
    assert_eq!(
        facet_coverage(&dag),
        snapshot,
        "the advisory must not mutate the measurement"
    );
    if let Some(msg) = verdict {
        assert!(
            msg.contains("advisory"),
            "an advisory message must say so: {msg}"
        );
    }

    // An all-bare DAG trips the advisory; the spine after propagation
    // clears the floor.
    let bare = WorkflowDag {
        id: "bare".into(),
        nodes: vec![
            {
                let mut n = TaskNode::skeleton("a", "a");
                n.outputs = vec![PortContract {
                    name: "out".into(),
                    semantic_type: SemanticType::edam("data:3917", ""),
                    ..Default::default()
                }];
                n
            },
            {
                let mut n = TaskNode::skeleton("b", "b");
                n.inputs = vec![PortContract {
                    name: "in".into(),
                    semantic_type: SemanticType::edam("data:3917", ""),
                    ..Default::default()
                }];
                n
            },
        ],
        edges: vec![edge("a", "out", "b", "in")],
        ..Default::default()
    };
    let bare_coverage = facet_coverage(&bare);
    assert!(bare_coverage.fraction_exact().abs() < f64::EPSILON);
    assert!(facet_coverage_advisory(&bare_coverage, FACET_COVERAGE_ADVISORY_FLOOR).is_some());
    assert!(after.fraction_exact() >= FACET_COVERAGE_ADVISORY_FLOOR);
}
