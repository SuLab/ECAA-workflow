//! §8.3 — real WRROC substrate round-trip via the runcrate-backed
//! validator (`PythonRuncrateWrrocValidator`), wired into the
//! conformance suite over a *freshly-emitted* package rather than the
//! `#[ignore]`d harness/core integration tests.
//!
//! Both tests are CAPABILITY-PROBED: if `runcrate version` fails (the
//! toolchain from `requirements-validator.txt` is not installed) the
//! test prints a LOUD skip notice and returns. This keeps the gate runnable
//! in `make wrroc-validate` (where the deps ARE present) without
//! `#[ignore]` hiding it from the default suite on dev machines — and the
//! shouted skip makes a deps-absent vacuous pass impossible to mistake for a
//! real runcrate-validation pass. Install the toolchain with:
//!
//! ```text
//! pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate
//! ```
//!
//! (or the pinned set in `requirements-validator.txt`).
//!
//! Positive case: a real emitted-then-FINALIZED (executed) descriptor
//! validates with 0 failures and declares all 6 required profile IRIs.
//! `conformsTo` is EXECUTION-AWARE (T6′): a pre-execution PLAN crate declares
//! only the 3 plan-set profiles, and the three WRROC v0.5 run profiles are
//! added by the finalize path once real per-output `CreateAction`s exist, so
//! the positive case registers a produced output table (turning the plan crate
//! into an executed crate) before validating the all-6 contract.
//! Negative case: a deliberately non-conformant 4-IRI descriptor (one
//! required WRROC profile dropped, no ParameterConnection / p-plan:Plan)
//! MUST yield ≥1 failure — the test that would catch a validator that
//! trivially passes everything.

use ecaa_workflow_conformance::WrrocValidator;
use ecaa_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Operator-facing install hint surfaced in the probe-skip notice so a
/// runcrate-absent vacuous pass is loudly distinguishable from a real WRROC
/// validation pass. Mirrors the pinned set in `requirements-validator.txt`.
const VALIDATOR_INSTALL_HINT: &str =
    "pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate";

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Returns true when `runcrate version` succeeds, i.e. the validator
/// toolchain is installed and this gate can run for real.
fn runcrate_available() -> bool {
    std::process::Command::new("runcrate")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Drive the v4 composer → build_dag → build_metadata pipeline and write
/// a real `ro-crate-metadata.json` (6 profile IRIs + ParameterConnection
/// + p-plan:Plan entities) into `out_dir`. Mirrors the emit path used by
/// the G1 acceptance gate in `wrroc_v05_conformance.rs`.
fn emit_real_descriptor(out_dir: &Path) {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use ecaa_workflow_core::classify::ClassificationResult;
    use ecaa_workflow_core::composer::compose_with_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    use ecaa_workflow_core::ro_crate::build_metadata;
    use std::collections::BTreeMap;

    let config = config_root();
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("archetypes");

    let goal = GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("wrroc runcrate conformance fixture".into()),
        confidence: 0.0,
    };
    let out = compose_with_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        &["bulk_rnaseq"],
        None,
        None,
        None,
    )
    .expect("compose");

    let dag = if let Some(wf) = out.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(wf, "wrroc-runcrate-fixture").expect("lower")
    } else {
        build_dag_from_composition(
            &out.composition,
            "wrroc-runcrate-fixture",
            &BTreeMap::new(),
            &[],
        )
        .expect("lower")
    };

    let clf = ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "wrroc runcrate conformance fixture".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "bulk RNA-seq wrroc runcrate fixture".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    };

    let metadata = build_metadata(
        &dag,
        &clf,
        &ecaa_workflow_core::clock::FrozenClock::default(),
    );
    let bytes = serde_json::to_vec_pretty(&metadata).expect("serialize metadata");
    std::fs::write(out_dir.join("ro-crate-metadata.json"), bytes).expect("write descriptor");

    // EXECUTION-AWARE conformsTo (T6′): the descriptor just written is a
    // PRE-EXECUTION PLAN crate declaring only the 3 plan-set profiles. To
    // exercise the all-6 executed contract, register a real produced output
    // table — this appends a retrospective `CreateAction` (a real run action)
    // and the finalize path then upgrades `conformsTo` to the full executed
    // set. We add ONE table for each task that has a HowToStep so the produced
    // output is attributed to a real step.
    let graph = metadata["@graph"].as_array().expect("@graph array");
    let step_tasks: Vec<String> = graph
        .iter()
        .filter(|e| matches!(e.get("@type").and_then(|t| t.as_str()), Some("HowToStep")))
        .filter_map(|e| {
            e.get("@id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.strip_prefix("#step-"))
                .map(str::to_string)
        })
        .collect();
    // Fall back to a single synthetic task if the DAG produced no steps, so the
    // finalize path always has at least one produced output to register.
    let tasks: Vec<String> = if step_tasks.is_empty() {
        vec!["stage".to_string()]
    } else {
        step_tasks
    };
    for task in &tasks {
        let table_dir = out_dir.join("runtime").join("outputs").join(task);
        std::fs::create_dir_all(&table_dir).expect("create output dir");
        std::fs::write(
            table_dir.join("result.tsv"),
            "gene\tlog2fc\tpadj\nGENE1\t1.0\t0.01\n",
        )
        .expect("write output table");
    }
    let added = ecaa_workflow_core::ro_crate::register_produced_output_tables(out_dir)
        .expect("register produced output tables");
    assert!(
        added >= 1,
        "finalize must register >=1 produced output table (real run action) so \
         conformsTo upgrades to the executed set"
    );
}

#[test]
fn runcrate_validates_emitted_descriptor_with_all_six_iris() {
    if !runcrate_available() {
        eprintln!(
            "\n>>> SKIP: `runcrate version` failed (toolchain not installed) <<<\n\
             >>> runcrate_validates_emitted_descriptor_with_all_six_iris did NOT run \
             — this is NOT a runcrate-validation pass. <<<\n\
             >>> Install the validator toolchain to run this gate for real:\n\
             >>>   {VALIDATOR_INSTALL_HINT}\n"
        );
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");
    // Emits the plan crate AND finalizes it (registers a produced output table),
    // so the descriptor is an EXECUTED crate carrying real run actions — the
    // precondition for truthfully declaring the three WRROC v0.5 run profiles.
    emit_real_descriptor(dir.path());

    // 1. The executed descriptor must declare all 6 required profile IRIs.
    let raw = std::fs::read_to_string(dir.path().join("ro-crate-metadata.json")).expect("read");
    let metadata: serde_json::Value = serde_json::from_str(&raw).expect("parse");
    let descriptor = metadata["@graph"]
        .as_array()
        .expect("@graph array")
        .iter()
        .find(|e| e["@id"] == "ro-crate-metadata.json")
        .expect("descriptor entry");
    let conforms = descriptor["conformsTo"]
        .as_array()
        .expect("conformsTo array");
    let ids: Vec<&str> = conforms.iter().filter_map(|c| c["@id"].as_str()).collect();
    for iri in ecaa_workflow_types::consts::REQUIRED_PROFILE_IRIS {
        assert!(
            ids.contains(iri),
            "emitted descriptor must declare required profile IRI {iri}; got {ids:?}"
        );
    }
    assert_eq!(
        ids.len(),
        6,
        "expected exactly 6 conformsTo IRIs; got {ids:?}"
    );

    // 2. runcrate validates it with zero failures.
    let report = PythonRuncrateWrrocValidator
        .validate_packages(&[dir.path()])
        .expect("invoking wrroc-validate.py");
    assert_eq!(
        report.summary.failed,
        0,
        "real emitted descriptor must validate cleanly; errors: {:?}",
        report
            .validated
            .iter()
            .flat_map(|p| p.errors.clone())
            .collect::<Vec<_>>()
    );
    assert!(report.validated.iter().all(|p| p.ok));
}

#[test]
fn runcrate_rejects_deficient_four_iri_descriptor() {
    if !runcrate_available() {
        eprintln!(
            "\n>>> SKIP: `runcrate version` failed (toolchain not installed) <<<\n\
             >>> runcrate_rejects_deficient_four_iri_descriptor did NOT run \
             — this is NOT a runcrate-validation pass. <<<\n\
             >>> Install the validator toolchain to run this gate for real:\n\
             >>>   {VALIDATOR_INSTALL_HINT}\n"
        );
        return;
    }

    let dir = tempfile::tempdir().expect("tempdir");

    // A deliberately deficient descriptor: only 4 conformsTo IRIs, and
    // one of the four required WRROC profiles (provenance/0.5) is
    // DROPPED. No ParameterConnection / p-plan:Plan entities either. The
    // validator MUST flag at least the missing required profile.
    let metadata = json!({
        "@context": "https://w3id.org/ro/crate/1.1/context",
        "@graph": [
            {
                "@id": "ro-crate-metadata.json",
                "@type": "CreativeWork",
                "about": {"@id": "./"},
                "conformsTo": [
                    {"@id": "https://w3id.org/ro/crate/1.1"},
                    {"@id": "https://w3id.org/ro/wfrun/process/0.5"},
                    {"@id": "https://w3id.org/ro/wfrun/workflow/0.5"},
                    {"@id": "https://w3id.org/workflowhub/workflow-ro-crate/1.0"}
                ]
            },
            {
                "@id": "./",
                "@type": "Dataset",
                "name": "deficient four-iri package"
            }
        ]
    });
    let bytes = serde_json::to_vec_pretty(&metadata).expect("serialize");
    std::fs::write(dir.path().join("ro-crate-metadata.json"), bytes).expect("write");

    let report = PythonRuncrateWrrocValidator
        .validate_packages(&[dir.path()])
        .expect("invoking wrroc-validate.py");

    assert!(
        report.summary.failed >= 1,
        "a 4-IRI descriptor missing the provenance/0.5 profile (and the \
         Tier-3 entities) MUST yield >=1 failure; got {:?}",
        report
    );
    let errs: Vec<String> = report
        .validated
        .iter()
        .flat_map(|p| p.errors.clone())
        .collect();
    assert!(
        errs.iter().any(|e| e.contains("provenance/0.5"))
            || errs.iter().any(|e| e.contains("ParameterConnection"))
            || errs.iter().any(|e| e.contains("p-plan:Plan")),
        "expected a missing-profile or missing-Tier-3-entity error; got {errs:?}"
    );
}
