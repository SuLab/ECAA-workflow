//! Integration tests for the flexible-plotting resolver-at-lowering wiring
//! (Phase A1–A3).
//!
//! These tests verify:
//! - A1: The affordance resolution logic produces correct records for a
//! 2-task DAG (one Registered, one Opaque semantic type).
//! - A2: `StructuralFallback` results are counted correctly by
//! `AffordanceFallbackCounter`.
//! - A3 (unit): The `PlotAffordanceRecord.provisional` flag is `false` for
//! `Registered` and `true` for all other variants.
//!
//! The full emit integration (write_affordance_sidecars + patch_ro_crate_metadata
//! stamping `ecaax:provisional` on ImageObject entities) is covered by the
//! conversation-crate test in `crates/conversation/src/emit/mod.rs::tests::
//! affordance_sidecars_written_and_provisional_flags_stamped`. That test is
//! `#[ignore]` because it requires the full emit chain (config/, tempdir, tokio).
//!
//! **Why these tests live in crates/core**: the resolution logic and the
//! `PlotAffordanceRecord` type are both in `ecaa_workflow_core`. The
//! conversation crate's `write_affordance_sidecars` is a thin orchestrator
//! over this core logic; exercising it separately keeps test scope minimal.

use ecaa_workflow_core::backend_emitters::workflow_json::PlotAffordanceRecord;
use ecaa_workflow_core::plot_affordance::registry::{PlotAffordanceRegistry, RegisteredAffordance};
use ecaa_workflow_core::plot_affordance::{
    resolve_affordance, AffordanceFallbackCounter, GenericPrimitive, PhysicalShape, PlotAffordance,
    PortDescriptor,
};
use std::collections::BTreeMap;

// ── In-memory registry fixture ──────────────────────────────────────────────

struct InMemoryRegistry {
    affordances: BTreeMap<String, RegisteredAffordance>,
    snapshot_id: String,
}

impl PlotAffordanceRegistry for InMemoryRegistry {
    fn lookup_exact(&self, t: &str) -> Option<&RegisteredAffordance> {
        self.affordances.get(t)
    }
    fn parents_of(&self, _t: &str) -> Vec<String> {
        vec![]
    }
    fn snapshot_id(&self) -> &str {
        &self.snapshot_id
    }
    fn iter(&self) -> Box<dyn Iterator<Item = (&str, &RegisteredAffordance)> + '_> {
        Box::new(self.affordances.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

fn two_task_registry() -> InMemoryRegistry {
    let mut affordances = BTreeMap::new();
    // One registered entry for "data:3917" (count matrix).
    affordances.insert(
        "data:3917".to_string(),
        RegisteredAffordance {
            semantic_type: "data:3917".to_string(),
            figure_ids: vec!["count_matrix_heatmap".to_string()],
            renderer_module: "runtime.plotting.stages.qc".to_string(),
            theme_version: "theme.json".to_string(),
        },
    );
    InMemoryRegistry {
        affordances,
        snapshot_id: "snap-integ-test".to_string(),
    }
}

// ── Task A1: resolver produces correct Registered record ────────────────────

#[test]
fn registered_semantic_type_resolves_to_registered_affordance() {
    let registry = two_task_registry();
    let port = PortDescriptor {
        semantic_type_iri: "data:3917",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let affordance = resolve_affordance(&port, &registry, "theme.json");
    assert!(
        matches!(affordance, PlotAffordance::Registered { .. }),
        "Expected Registered, got {:?}",
        affordance
    );
    assert!(
        !affordance.is_provisional(),
        "Registered must not be provisional"
    );
}

// ── Task A1: Opaque type with Unknown shape resolves to Deferred ────────────

#[test]
fn opaque_semantic_type_with_unknown_shape_resolves_to_deferred() {
    let registry = two_task_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:opaque:my_task",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let affordance = resolve_affordance(&port, &registry, "theme.json");
    // PhysicalShape::Unknown has no structural_primitive() → Deferred.
    assert!(
        matches!(affordance, PlotAffordance::Deferred { .. }),
        "Expected Deferred for Opaque + Unknown shape, got {:?}",
        affordance
    );
    assert!(affordance.is_provisional(), "Deferred must be provisional");
}

// ── Task A1: Opaque type with Numeric2D shape resolves to StructuralFallback ─

#[test]
fn opaque_semantic_type_with_numeric2d_shape_resolves_to_structural_fallback() {
    let registry = two_task_registry();
    let port = PortDescriptor {
        semantic_type_iri: "ecaax:opaque:my_matrix_task",
        declared_parents: &[],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let affordance = resolve_affordance(&port, &registry, "theme.json");
    assert!(
        matches!(
            affordance,
            PlotAffordance::StructuralFallback {
                primitive: GenericPrimitive::MatrixOverview,
                ..
            }
        ),
        "Expected StructuralFallback(MatrixOverview) for Opaque + Numeric2D, got {:?}",
        affordance
    );
    assert!(
        affordance.is_provisional(),
        "StructuralFallback must be provisional"
    );
}

// ── Task A1: record building + JSONL serialization ──────────────────────────

#[test]
fn plot_affordance_records_serialize_to_valid_jsonl() {
    let registry = two_task_registry();

    // Build records for the 2-task scenario: task_a = Registered, task_b = Deferred.
    let port_a = PortDescriptor {
        semantic_type_iri: "data:3917",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let port_b = PortDescriptor {
        semantic_type_iri: "ecaax:opaque:task_b",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let affordance_a = resolve_affordance(&port_a, &registry, "theme.json");
    let affordance_b = resolve_affordance(&port_b, &registry, "theme.json");

    let records = vec![
        PlotAffordanceRecord {
            task_id: "task_a".to_string().into(),
            port_name: "out".to_string(),
            provisional: affordance_a.is_provisional(),
            affordance: affordance_a,
        },
        PlotAffordanceRecord {
            task_id: "task_b".to_string().into(),
            port_name: "out".to_string(),
            provisional: affordance_b.is_provisional(),
            affordance: affordance_b,
        },
    ];

    // Serialize as JSONL.
    let mut jsonl = String::new();
    for rec in &records {
        jsonl.push_str(&serde_json::to_string(rec).expect("serialization failed"));
        jsonl.push('\n');
    }
    let lines: Vec<&str> = jsonl.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "Expected 2 JSONL lines, one per task");

    // First record: Registered, provisional: false.
    let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(first["task_id"], "task_a");
    assert_eq!(first["provisional"], false);
    assert_eq!(first["affordance"]["kind"], "registered");

    // Second record: Deferred, provisional: true.
    let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(second["task_id"], "task_b");
    assert_eq!(second["provisional"], true);
    assert_eq!(second["affordance"]["kind"], "deferred");
}

// ── Task A2: counter increments only on StructuralFallback ──────────────────

#[test]
fn fallback_counter_increments_on_structural_fallback_only() {
    let registry = two_task_registry();
    let mut counter = AffordanceFallbackCounter::default();

    // Registered port — must NOT increment the counter.
    let port_reg = PortDescriptor {
        semantic_type_iri: "data:3917",
        declared_parents: &[],
        physical_shape: PhysicalShape::Unknown,
    };
    let affordance_reg = resolve_affordance(&port_reg, &registry, "theme.json");
    if let PlotAffordance::StructuralFallback { primitive, .. } = &affordance_reg {
        let prim_str = serde_json::to_value(primitive)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        counter.record("data:3917", &prim_str);
    }

    // Opaque port with Numeric2D → StructuralFallback(MatrixOverview).
    let port_opaque = PortDescriptor {
        semantic_type_iri: "ecaax:opaque:matrix_task",
        declared_parents: &[],
        physical_shape: PhysicalShape::Numeric2D,
    };
    let affordance_opaque = resolve_affordance(&port_opaque, &registry, "theme.json");
    if let PlotAffordance::StructuralFallback { primitive, .. } = &affordance_opaque {
        let prim_str = serde_json::to_value(primitive)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        counter.record("ecaax:opaque:matrix_task", &prim_str);
    }

    // Only the Opaque task incremented the counter.
    let gaps = counter.all_gaps_sorted_by_count_desc();
    assert_eq!(gaps.len(), 1, "Expected exactly 1 counter entry");
    assert_eq!(gaps[0].0, "ecaax:opaque:matrix_task");
    assert_eq!(gaps[0].1, "matrix_overview");
    assert_eq!(gaps[0].2, 1);
}

// ── Task A3: provisional flag shape ─────────────────────────────────────────

#[test]
fn provisional_flag_matches_non_registered_variants() {
    use ecaa_workflow_core::plot_affordance::affordance::AffordanceProof;

    let proof = AffordanceProof {
        source_semantic_type: "test".into(),
        ontology_walk: vec![],
        registry_snapshot_id: "snap".into(),
        theme_version: "v1".into(),
        rationale: "test".into(),
    };

    // Registered — NOT provisional.
    let reg = PlotAffordance::Registered {
        figure_ids: vec!["fig1".into()],
        renderer_module: "mod".into(),
        proof: proof.clone(),
    };
    assert!(!reg.is_provisional(), "Registered must not be provisional");

    // InheritedViaOntology — provisional.
    let inh = PlotAffordance::InheritedViaOntology {
        parent_term: "parent:1".into(),
        figure_ids: vec!["fig1".into()],
        renderer_module: "mod".into(),
        proof: proof.clone(),
    };
    assert!(
        inh.is_provisional(),
        "InheritedViaOntology must be provisional"
    );

    // StructuralFallback — provisional.
    let sf = PlotAffordance::StructuralFallback {
        primitive: GenericPrimitive::MatrixOverview,
        figure_id: "__structural_matrix_overview".into(),
        warning: "w".into(),
        proof: proof.clone(),
    };
    assert!(
        sf.is_provisional(),
        "StructuralFallback must be provisional"
    );

    // Deferred — provisional.
    let def = PlotAffordance::Deferred {
        data_artifact_relpath: "".into(),
        recommendation: "r".into(),
        sme_check_required: true,
        proof,
    };
    assert!(def.is_provisional(), "Deferred must be provisional");
}

// ── Full-chain integration test (requires tempdir + full emit pipeline) ──────
//
// This test exercises the conversation crate's `write_affordance_sidecars`
// + `patch_ro_crate_metadata` chain. It is `#[ignore]` because it requires:
// 1. A populated `config/` directory (plot-affordances/, stage-atoms/),
// 2. A real package directory produced by `core::emit_package`, and
// 3. The `tokio` async runtime.
//
// Run with: cargo test -p ecaa-workflow-conversation -- --ignored
// See `crates/conversation/src/emit/mod.rs::tests::
// affordance_sidecars_written_and_provisional_flags_stamped`.
#[test]
#[ignore]
fn full_emit_chain_integration_placeholder() {
    // This placeholder documents the expected behavior tested in the
    // conversation crate:
    // - runtime/plot_affordances.jsonl is written with ≥ 1 record per task.
    // - Registered-type tasks produce records with provisional: false.
    // - Opaque/StructuralFallback/Deferred tasks produce records with provisional: true.
    // - ImageObject entities in ro-crate-metadata.json for provisional tasks
    // carry "ecaax:provisional": true and "ecaax:affordanceVariant": "<tag>".
    // - Registered tasks' figure entities carry neither flag.
}
