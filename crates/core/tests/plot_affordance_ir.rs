use ecaa_workflow_core::plot_affordance::{
    AffordanceProof, GeneratedReviewStatus, GenericPrimitive, PlotAffordance, PlotSafety,
};

fn proof() -> AffordanceProof {
    AffordanceProof {
        source_semantic_type: "EDAM:data_3134".into(),
        ontology_walk: vec![],
        registry_snapshot_id: "snap-2026-05-08-a".into(),
        theme_version: "theme.json:sha256:abc".into(),
        rationale: "exact match".into(),
    }
}

#[test]
fn round_trips_registered() {
    let a = PlotAffordance::Registered {
        figure_ids: vec!["volcano".into(), "ma_plot".into()],
        renderer_module: "runtime.plotting.stages.differential_expression".into(),
        proof: proof(),
    };
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
    assert_eq!(a.safety(), PlotSafety::Validated);
    assert!(!a.is_provisional());
}

#[test]
fn round_trips_inherited_via_ontology() {
    let a = PlotAffordance::InheritedViaOntology {
        parent_term: "EDAM:data_3134".into(),
        figure_ids: vec!["volcano".into()],
        renderer_module: "runtime.plotting.stages.differential_expression".into(),
        proof: AffordanceProof {
            ontology_walk: vec!["EDAM:data_3134".into()],
            ..proof()
        },
    };
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
    assert!(a.is_provisional());
    assert_eq!(a.safety(), PlotSafety::InheritanceWarn);
}

#[test]
fn round_trips_structural_fallback() {
    let a = PlotAffordance::StructuralFallback {
        primitive: GenericPrimitive::MatrixOverview,
        figure_id: GenericPrimitive::MatrixOverview.figure_id().to_string(),
        warning: "type-specific renderer pending".into(),
        proof: proof(),
    };
    let json = serde_json::to_string(&a).unwrap();
    let back: PlotAffordance = serde_json::from_str(&json).unwrap();
    assert_eq!(a, back);
    assert_eq!(a.safety(), PlotSafety::Generic);
}

#[test]
fn round_trips_generated_sandboxed() {
    let a = PlotAffordance::GeneratedSandboxed {
        renderer_module: "runtime.plotting.stages._generated.spatial_graph_embedding".into(),
        figure_ids: vec!["spatial_embedding_overview".into()],
        review_status: GeneratedReviewStatus::SandboxValidated,
        proof: proof(),
    };
    let back: PlotAffordance = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(a, back);
}

#[test]
fn round_trips_deferred() {
    let a = PlotAffordance::Deferred {
        data_artifact_relpath: "runtime/outputs/x/result.parquet".into(),
        recommendation: "trajectory plot via scvelo".into(),
        sme_check_required: true,
        proof: proof(),
    };
    let back: PlotAffordance = serde_json::from_str(&serde_json::to_string(&a).unwrap()).unwrap();
    assert_eq!(a, back);
    assert_eq!(a.safety(), PlotSafety::None);
}

#[test]
fn primitive_figure_ids_are_unique() {
    use std::collections::BTreeSet;
    let ids: BTreeSet<&str> = [
        GenericPrimitive::MatrixOverview,
        GenericPrimitive::Distribution,
        GenericPrimitive::CategoricalSummary,
        GenericPrimitive::Pairs,
        GenericPrimitive::ScalarCard,
    ]
    .iter()
    .map(|p| p.figure_id())
    .collect();
    assert_eq!(ids.len(), 5);
}
