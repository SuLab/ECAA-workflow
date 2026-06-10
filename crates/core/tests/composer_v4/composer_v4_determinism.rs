//! 100× determinism replay test for
//! `composer_v4::plan`. Catches non-determinism that would escape the
//! per-module sort discipline (forward / backward / meet are each
//! deterministic individually; this checks the integrated planner is
//! byte-stable across replays given the same inputs).
//!
//! The replay harness serializes each `PlannerResult` through serde
//! JSON and SHA-256 hashes the bytes — comparing hashes catches every
//! drift, including ones that show up only in deeply-nested fields
//! (proof warnings, adapter id orderings, score tuple components).
//!
//! This single-platform replay test is the live in-repo determinism
//! guard. (A cross-platform canonical-hash baseline gate coupled to a
//! `.github/ci/determinism-baseline.json` file once lived here; that
//! file belongs to the deprecated internal CI surface and is absent from
//! this slim OSS repo, so the dead gate was removed — M19.)

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer_v4::{plan, PlannerResult, PlanningContext};
use ecaa_workflow_core::goal_spec::GoalSpec;
use ecaa_workflow_core::workflow_contracts::{
    data_product::DataProductContract,
    workflow_intent::{DesiredOutput, WorkflowIntent},
};
use sha2::{Digest, Sha256};

#[test]
fn plan_is_byte_stable_across_100_replays() {
    let atom_reg =
        AtomRegistry::load_from_dir(std::path::Path::new("../../config/stage-atoms")).unwrap();
    let archetype_reg =
        ArchetypeRegistry::load_from_dir(std::path::Path::new("../../config/archetypes")).unwrap();

    let de_goal = DataProductContract::sample_de_table();
    let goal_iri = de_goal.semantic_type.stable_id();

    let goal = GoalSpec {
        edam_data: goal_iri.clone(),
        edam_format: Some("format:3475".into()),
        modifiers: Default::default(),
        source_prose: Some("differential expression table from paired RNA-seq".into()),
        confidence: 0.9,
    };

    let intent = WorkflowIntent {
        id: "determinism_test".into(),
        schema_version: semver::Version::new(1, 0, 0),
        goal: "bulk RNA-seq DE".into(),
        modality: Some("bulk_rnaseq".into()),
        project_class: Some("research".into()),
        available_data: vec![DataProductContract::sample_paired_fastq()],
        desired_outputs: vec![DesiredOutput {
            label: "differential expression table".into(),
            edam_data: Some(goal_iri),
            edam_format: Some("format:3475".into()),
            human_readable: false,
        }],
        ..Default::default()
    };
    let mut ctx = PlanningContext::new(intent);
    ctx.max_branches = 32;
    ctx.max_depth = 6;
    ctx.max_alternatives = 3;

    let baseline = plan(&ctx, &goal, "research", &atom_reg, &archetype_reg);
    let baseline_hash = sha256(&baseline);

    for i in 0..100 {
        let replay = plan(&ctx, &goal, "research", &atom_reg, &archetype_reg);
        let replay_hash = sha256(&replay);
        assert_eq!(
            replay_hash, baseline_hash,
            "iteration {i}: planner output drifted from baseline; this indicates \
             non-deterministic iteration order in forward/backward/meet/scoring \
             (likely a HashMap that escaped the BTreeMap discipline)"
        );
    }
}

/// Hash a serializable value via SHA-256 over its JSON serialization.
/// `PlannerResult` carries `WorkflowDag` (with `f64`s in iteration
/// declarations) so we can't derive `Eq`; JSON-then-hash is the
/// established determinism pattern in this crate (see
/// `meet_in_the_middle` round-trip in `composer_v4_meet_in_middle.rs`).
fn sha256<T: serde::Serialize>(t: &T) -> String {
    let bytes = serde_json::to_vec(t)
        .expect("PlannerResult should be serde-serializable; if this panics, add Serialize derive");
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    format!("{:x}", hasher.finalize())
}

/// Sanity check: the helper itself is deterministic so a per-replay
/// failure can't be blamed on the harness.
#[test]
fn sha256_helper_is_deterministic() {
    let a = sha256(&"hello world");
    let b = sha256(&"hello world");
    assert_eq!(a, b);
    let _: PlannerResult = ecaa_workflow_core::composer_v4::PlannerResult {
        primary: ecaa_workflow_core::workflow_contracts::outcome::ComposeOutcome::PartialDag {
            dag: Default::default(),
            unresolved_gaps: Vec::new(),
        },
        alternatives: Vec::new(),
    };
}

// The cross-platform canonical-hash baseline gate that used to live here
// (`emit_baseline_hashes` + `cross_platform_hashes_match_baseline`, keyed
// off `../../.github/ci/determinism-baseline.json`) was removed in M19:
// the baseline file is part of the deprecated internal CI surface and is
// absent from this slim OSS repo, so the gate could never run. The
// single-platform `plan_is_byte_stable_across_100_replays` above remains
// the live in-repo determinism guard.
