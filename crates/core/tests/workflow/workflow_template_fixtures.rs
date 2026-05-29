//! `WorkflowTemplate` fixtures cover at least
//! one conditional, one scatter/gather expansion, and one bounded
//! iterative procedure. None of these touch the existing builder
//! path — they exist to pin the IR shape so the lowering pass
//! has something concrete to lower.
//!
//! Per CLAUDE.md "Architecture rules": these fixtures must
//! round-trip through serde deterministically (BTreeMap-backed
//! ordering, sorted iteration). Determinism tests on the round
//! trip pin that contract.

use ecaa_workflow_core::workflow_contracts::task_node::{
    ConditionalEdge, IterationDeclaration, ScatterDeclaration, WorkflowTemplate,
};
use ecaa_workflow_core::workflow_contracts::{
    CompatibilityProof, EdgeContract, Implementation, OciImageRef, PortContract, SemanticType,
    TaskNode,
};

fn align_node() -> TaskNode {
    let mut n = TaskNode::skeleton("align_reads", "Align reads to reference");
    n.inputs.push(PortContract {
        name: "fastq".into(),
        semantic_type: SemanticType::edam("data:2044", "Sequence"),
        ..Default::default()
    });
    n.outputs.push(PortContract {
        name: "bam".into(),
        semantic_type: SemanticType::edam("data:0863", "Sequence alignment"),
        ..Default::default()
    });
    n.implementation = Implementation::ContainerCommand {
        image: OciImageRef {
            image: "ghcr.io/scripps/bio-base".into(),
            tag: "v0.4.0".into(),
            digest: String::new(),
            arch: vec!["amd64".into()],
            gpu: false,
        },
        command_template: vec!["align".into(), "${input.fastq}".into()],
    };
    n
}

fn quantify_node() -> TaskNode {
    let mut n = TaskNode::skeleton("quantify_features", "Count features per gene");
    n.inputs.push(PortContract {
        name: "bam".into(),
        semantic_type: SemanticType::edam("data:0863", "Sequence alignment"),
        ..Default::default()
    });
    n.outputs.push(PortContract {
        name: "counts".into(),
        semantic_type: SemanticType::edam("data:3917", "Count matrix"),
        ..Default::default()
    });
    n
}

fn de_node() -> TaskNode {
    let mut n = TaskNode::skeleton("differential_expression", "Test DE genes");
    n.inputs.push(PortContract {
        name: "counts".into(),
        semantic_type: SemanticType::edam("data:3917", "Count matrix"),
        ..Default::default()
    });
    n.outputs.push(PortContract {
        name: "de_table".into(),
        semantic_type: SemanticType::edam("data:3917", "DE results"),
        ..Default::default()
    });
    n
}

fn batch_correct_node() -> TaskNode {
    let mut n = TaskNode::skeleton("batch_correct", "Adjust for batch effects");
    n.inputs.push(PortContract {
        name: "counts".into(),
        semantic_type: SemanticType::edam("data:3917", "Count matrix"),
        ..Default::default()
    });
    n.outputs.push(PortContract {
        name: "counts".into(),
        semantic_type: SemanticType::edam("data:3917", "Count matrix"),
        ..Default::default()
    });
    n
}

fn cluster_node() -> TaskNode {
    let mut n = TaskNode::skeleton("clustering", "Cluster cells / samples");
    n.inputs.push(PortContract {
        name: "embedding".into(),
        semantic_type: SemanticType::edam("data:3917", "Embedding"),
        ..Default::default()
    });
    n.outputs.push(PortContract {
        name: "labels".into(),
        semantic_type: SemanticType::edam("data:3917", "Cluster labels"),
        ..Default::default()
    });
    n
}

#[test]
fn conditional_template_round_trips() {
    // Conditional: batch_correct only runs when discovery says so.
    let template = WorkflowTemplate {
        id: "rnaseq_with_optional_bc".into(),
        name: "Bulk RNA-seq with conditional batch correction".into(),
        description: Some(
            "Aligns, quantifies, optionally batch-corrects, and tests DE genes.".into(),
        ),
        nodes: vec![
            align_node(),
            quantify_node(),
            batch_correct_node(),
            de_node(),
        ],
        edges: vec![
            EdgeContract {
                from_node: "align_reads".into(),
                from_port: "bam".into(),
                to_node: "quantify_features".into(),
                to_port: "bam".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            },
            EdgeContract {
                from_node: "quantify_features".into(),
                from_port: "counts".into(),
                to_node: "batch_correct".into(),
                to_port: "counts".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            },
            EdgeContract {
                from_node: "batch_correct".into(),
                from_port: "counts".into(),
                to_node: "differential_expression".into(),
                to_port: "counts".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            },
        ],
        conditionals: vec![ConditionalEdge {
            gate_node_id: "batch_correct".into(),
            expression: "discover_batch_correction.result.batch_correction_required == true".into(),
            rationale: Some("Skip batch correction when no batch effect detected".into()),
        }],
        scatters: vec![],
        iterations: vec![],
    };

    let json = serde_json::to_string_pretty(&template).unwrap();
    let back: WorkflowTemplate = serde_json::from_str(&json).unwrap();
    // Direct equality skipped because IterationDeclaration carries
    // f64; the canonical equality test is byte-level on stable
    // serialization.
    let json2 = serde_json::to_string_pretty(&back).unwrap();
    assert_eq!(json, json2, "conditional template round-trip changed shape");

    // Determinism: re-serializing the same value twice produces
    // byte-identical output.
    let json3 = serde_json::to_string_pretty(&template).unwrap();
    assert_eq!(
        json, json3,
        "conditional template serialization non-deterministic"
    );
}

#[test]
fn scatter_gather_template_round_trips() {
    // Scatter/gather: per-sample alignment + per-sample
    // quantification fan in to a study-level DE call.
    let template = WorkflowTemplate {
        id: "rnaseq_per_sample_de".into(),
        name: "Bulk RNA-seq DE with per-sample fan-out".into(),
        description: Some("Each sample aligns + quantifies; DE runs once over the matrix.".into()),
        nodes: vec![align_node(), quantify_node(), de_node()],
        edges: vec![
            EdgeContract {
                from_node: "align_reads".into(),
                from_port: "bam".into(),
                to_node: "quantify_features".into(),
                to_port: "bam".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            },
            EdgeContract {
                from_node: "quantify_features".into(),
                from_port: "counts".into(),
                to_node: "differential_expression".into(),
                to_port: "counts".into(),
                proof: CompatibilityProof::default(),
                chain_of_custody: None,
            },
        ],
        conditionals: vec![],
        scatters: vec![
            ScatterDeclaration {
                scatter_node_id: "align_reads".into(),
                shard_key: "sample".into(),
                gather_node_id: Some("differential_expression".into()),
            },
            ScatterDeclaration {
                scatter_node_id: "quantify_features".into(),
                shard_key: "sample".into(),
                gather_node_id: Some("differential_expression".into()),
            },
        ],
        iterations: vec![],
    };

    let json = serde_json::to_string_pretty(&template).unwrap();
    let back: WorkflowTemplate = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn iterative_template_round_trips() {
    // Bounded iteration: cluster → check silhouette → cluster
    // again until silhouette > 0.7 for 2 consecutive iterations.
    let template = WorkflowTemplate {
        id: "scrnaseq_iterative_clustering".into(),
        name: "Single-cell clustering with iterate-until-converged".into(),
        description: Some("Clustering re-runs until silhouette stabilizes.".into()),
        nodes: vec![cluster_node()],
        edges: vec![],
        conditionals: vec![],
        scatters: vec![],
        iterations: vec![IterationDeclaration {
            iterate_node_id: "clustering".into(),
            max_iterations: 10,
            min_iterations: 2,
            metric_source: "result.silhouette".into(),
            operator: "gt".into(),
            threshold: 0.7,
            consecutive_iterations: 2,
        }],
    };

    let json = serde_json::to_string_pretty(&template).unwrap();
    let back: WorkflowTemplate = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string_pretty(&back).unwrap();
    assert_eq!(json, json2);

    // Determinism on iteration declarations: f64 threshold
    // serializes as a stable JSON number, so re-running produces
    // byte-identical output.
    let json3 = serde_json::to_string_pretty(&template).unwrap();
    assert_eq!(json, json3);
}

#[test]
fn empty_template_round_trips() {
    let template = WorkflowTemplate::default();
    let json = serde_json::to_string(&template).unwrap();
    let back: WorkflowTemplate = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn templates_serialize_deterministically() {
    // Run all three templates back-to-back and assert serialization
    // is byte-stable across calls.
    let templates = vec![
        WorkflowTemplate {
            id: "t1".into(),
            name: "t1".into(),
            description: None,
            nodes: vec![align_node()],
            edges: vec![],
            conditionals: vec![],
            scatters: vec![],
            iterations: vec![],
        },
        WorkflowTemplate {
            id: "t2".into(),
            name: "t2".into(),
            description: None,
            nodes: vec![],
            edges: vec![],
            conditionals: vec![],
            scatters: vec![],
            iterations: vec![IterationDeclaration {
                iterate_node_id: "x".into(),
                max_iterations: 5,
                min_iterations: 1,
                metric_source: "result.metric".into(),
                operator: "lt".into(),
                threshold: 0.001,
                consecutive_iterations: 2,
            }],
        },
    ];
    for t in &templates {
        let s1 = serde_json::to_string(t).unwrap();
        let s2 = serde_json::to_string(t).unwrap();
        let s3 = serde_json::to_string(t).unwrap();
        assert_eq!(s1, s2);
        assert_eq!(s2, s3);
    }
}
