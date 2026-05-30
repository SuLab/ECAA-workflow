use ecaa_workflow_core::plot_affordance::registry::{PlotAffordanceRegistry, RegisteredAffordance};
use ecaa_workflow_core::plot_affordance::{
    resolve_affordance, GenericPrimitive, PhysicalShape, PlotAffordance, PortDescriptor,
};
use std::collections::BTreeMap;

struct InMemoryRegistry {
    affordances: BTreeMap<String, RegisteredAffordance>,
    parents: BTreeMap<String, Vec<String>>,
    snapshot_id: String,
}

impl PlotAffordanceRegistry for InMemoryRegistry {
    fn lookup_exact(&self, t: &str) -> Option<&RegisteredAffordance> {
        self.affordances.get(t)
    }
    fn parents_of(&self, t: &str) -> Vec<String> {
        self.parents.get(t).cloned().unwrap_or_default()
    }
    fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_> {
        Box::new(self.affordances.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

fn de_table_registry() -> InMemoryRegistry {
    let mut affordances = BTreeMap::new();
    affordances.insert(
        "EDAM:data_3134".into(),
        RegisteredAffordance {
            semantic_type: "EDAM:data_3134".into(),
            figure_ids: vec!["volcano".into(), "ma_plot".into()],
            renderer_module: "runtime.plotting.stages.differential_expression".into(),
            theme_version: "theme.json".into(),
        },
    );
    let mut parents = BTreeMap::new();
    parents.insert(
        "ecaax:spatial_de_table".into(),
        vec!["EDAM:data_3134".into()],
    );
    InMemoryRegistry {
        affordances,
        parents,
        snapshot_id: "snap-test".into(),
    }
}

#[test]
fn exact_match_returns_registered() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "EDAM:data_3134",
        declared_parents: &[],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let a = resolve_affordance(&port, &reg, "theme.json");
    match a {
        PlotAffordance::Registered { figure_ids, .. } => {
            assert_eq!(figure_ids, vec!["volcano", "ma_plot"]);
        }
        other => panic!("expected Registered, got {other:?}"),
    }
}

#[test]
fn declared_parent_returns_inherited() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:novel_de_table",
        declared_parents: &["EDAM:data_3134".into()],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let a = resolve_affordance(&port, &reg, "theme.json");
    match a {
        PlotAffordance::InheritedViaOntology {
            parent_term,
            figure_ids,
            ..
        } => {
            assert_eq!(parent_term, "EDAM:data_3134");
            assert_eq!(figure_ids, vec!["volcano", "ma_plot"]);
        }
        other => panic!("expected InheritedViaOntology, got {other:?}"),
    }
}

#[test]
fn registry_ancestor_walk_returns_inherited() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:spatial_de_table",
        declared_parents: &[],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let a = resolve_affordance(&port, &reg, "theme.json");
    match a {
        PlotAffordance::InheritedViaOntology { parent_term, .. } => {
            assert_eq!(parent_term, "EDAM:data_3134");
        }
        other => panic!("expected InheritedViaOntology via registry walk, got {other:?}"),
    }
}

#[test]
fn no_match_falls_back_to_structural() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:totally_novel_thing",
        declared_parents: &[],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let a = resolve_affordance(&port, &reg, "theme.json");
    match a {
        PlotAffordance::StructuralFallback { primitive, .. } => {
            assert_eq!(primitive, GenericPrimitive::MatrixOverview);
        }
        other => panic!("expected StructuralFallback, got {other:?}"),
    }
}

#[test]
fn unknown_shape_returns_deferred() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:opaque_blob",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let a = resolve_affordance(&port, &reg, "theme.json");
    match a {
        PlotAffordance::Deferred {
            sme_check_required, ..
        } => {
            assert!(sme_check_required);
        }
        other => panic!("expected Deferred, got {other:?}"),
    }
}

#[test]
fn replay_determinism() {
    let reg = de_table_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:novel_de_table",
        declared_parents: &["EDAM:data_3134".into()],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let baseline = resolve_affordance(&port, &reg, "theme.json");
    for _ in 0..100 {
        assert_eq!(resolve_affordance(&port, &reg, "theme.json"), baseline);
    }
}
