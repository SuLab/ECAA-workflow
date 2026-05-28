//! Grant v19 G1 acceptance gate — >=17/23 reproducibility-anchor
//! studies emit valid WRROC v0.5-conformant packages.
//!
//! Two tests live here:
//!
//! 1. `fixtures_directory_has_23_intakes` — always runnable smoke test;
//! asserts the 23 fixture intake.json files exist on disk. Guards the
//! fixture corpus from accidental rename / removal.
//!
//! 2. `g1_acceptance_at_least_17_of_23_fixtures_validate` — gated
//! behind `#[ignore]` because it shells out to
//! `scripts/wrroc-validate.py` which needs Python 3.11+ and
//! `runcrate>=0.5.0` from `requirements-validator.txt`. CI runs this
//! via the `make wrroc-validate` target on a job with the validator
//! deps preinstalled. The end-to-end pipeline this test exercises is
//! intake_prose -> Classifier::classify -> v4 compose ->
//! build_dag_from_workflow_dag -> build_metadata -> ro-crate-metadata.json,
//! then the runcrate wrapper validates each emitted descriptor.

use scripps_workflow_core::wrroc_validator::WrrocValidator;
use scripps_workflow_harness::wrroc_validator_impl::PythonRuncrateWrrocValidator;
use serde::Deserialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct IntakeFixture {
    study_id: String,
    intake_prose: String,
    expected_modality: String,
    expected_archetype_id: Option<String>,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("testdata/wrroc-fixtures")
}

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Always-runnable smoke test: confirm the 23 fixture intake.json files
/// are present under `testdata/wrroc-fixtures/`. This is the corpus
/// integrity gate — `make wrroc-validate` (the G1 acceptance test
/// below) depends on this directory shape.
#[test]
fn fixtures_directory_has_23_intakes() {
    let dir = fixtures_dir();
    let count = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir({}) failed: {e}", dir.display()))
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("intake.json").exists())
        .count();
    assert_eq!(
        count,
        23,
        "expected 23 fixture intakes under {}; got {count}",
        dir.display()
    );
}

/// Drive one fixture through classify -> compose (v4) ->
/// build_dag_from_workflow_dag -> build_metadata, then write
/// `ro-crate-metadata.json` into `out_dir`. Returns true on success;
/// false on a non-fatal pipeline error (the G1 gate tolerates up to
/// 6/23 failures). Hard panics on test-infrastructure errors
/// (unreadable config, malformed fixture JSON).
fn emit_package_for_fixture(fixture: &IntakeFixture, out_dir: &Path) -> bool {
    use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
    use scripps_workflow_core::atom_registry::AtomRegistry;
    use scripps_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use scripps_workflow_core::classify::Classifier;
    use scripps_workflow_core::composer::compose_with_version_and_modalities_full;
    use scripps_workflow_core::goal_spec::GoalSpec;
    use scripps_workflow_core::ro_crate::build_metadata;
    use std::collections::BTreeMap;

    let config = config_root();
    let keywords_path = config.join("modality-keywords.yaml");
    let classifier =
        Classifier::load(&keywords_path).expect("loading classifier config from config/");
    let mut clf = classifier.classify(&fixture.intake_prose);

    // Override the inferred modality with the fixture's expected
    // modality. The fixtures are curated against the canonical
    // 13-row mapping; if the classifier disagrees we still want the
    // emit pipeline to exercise the expected archetype so the WRROC
    // validation reflects shape conformance rather than classifier
    // routing accuracy (covered by other gates).
    clf.modality = fixture.expected_modality.clone();
    clf.archetype_id = fixture.expected_archetype_id.clone();
    if clf.intake_text.is_empty() {
        clf.intake_text = fixture.intake_prose.clone();
    }
    if clf.domain.is_empty() {
        clf.domain = "computational biology".into();
    }
    if clf.workflow_description.is_empty() {
        clf.workflow_description = format!("WRROC fixture {}", fixture.study_id);
    }
    if clf.edam_topic.is_empty() {
        clf.edam_topic = "topic_3170".into();
    }
    if clf.edam_operation.is_empty() {
        clf.edam_operation = "operation_2945".into();
    }

    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms"))
        .expect("loading atom registry from config/stage-atoms/");
    let archetypes = ArchetypeRegistry::load_from_dir(&config.join("archetypes"))
        .expect("loading archetype registry from config/archetypes/");

    let goal = clf.goal.clone().unwrap_or_else(|| GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some(format!(
            "WRROC fixture {} bare-modality fallback",
            fixture.study_id
        )),
        confidence: 0.0,
    });

    let modalities: Vec<&str> = vec![clf.modality.as_str()];

    let compose_result = compose_with_version_and_modalities_full(
        &goal,
        "bioinformatics",
        &atoms,
        &archetypes,
        4,
        &modalities,
        None,
        None,
        None,
    );

    let output_compose = match compose_result {
        Ok(o) => o,
        Err(e) => {
            eprintln!(
                "[wrroc-fixture {}] compose failed: {:?}",
                fixture.study_id, e
            );
            return false;
        }
    };

    let workflow_id = format!("wrroc-fixture-{}", fixture.study_id);
    let dag_result = if let Some(workflow_dag) = output_compose.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(workflow_dag, &workflow_id)
    } else {
        build_dag_from_composition(
            &output_compose.composition,
            &workflow_id,
            &BTreeMap::new(),
            &[],
        )
    };

    let dag = match dag_result {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "[wrroc-fixture {}] build_dag failed: {:?}",
                fixture.study_id, e
            );
            return false;
        }
    };

    let metadata = build_metadata(
        &dag,
        &clf,
        &scripps_workflow_core::clock::FrozenClock::default(),
    );
    let bytes = match serde_json::to_vec_pretty(&metadata) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "[wrroc-fixture {}] serialize metadata failed: {e}",
                fixture.study_id
            );
            return false;
        }
    };
    if let Err(e) = std::fs::write(out_dir.join("ro-crate-metadata.json"), bytes) {
        eprintln!(
            "[wrroc-fixture {}] writing ro-crate-metadata.json failed: {e}",
            fixture.study_id
        );
        return false;
    }
    true
}

/// G1 acceptance gate: at least 17 of 23 fixtures must produce a
/// runcrate-validating WRROC v0.5 package descriptor.
///
/// Skips with an informative message when fewer than 23 fixture
/// directories are present on disk (safe to dispatch before all
/// new fixtures from the primary-corpus expansion exist).
///
/// Ignored by default — the wrapper script needs Python and
/// `runcrate>=0.5.0` from `requirements-validator.txt`. Run via:
///
/// ```text
/// make wrroc-validate
/// ```
///
/// or directly:
///
/// ```text
/// cargo test -p scripps-workflow-core --test wrroc_v05_fixtures \
/// g1_acceptance_at_least_17_of_23_fixtures_validate \
/// -- --ignored --nocapture
/// ```
#[test]
#[ignore = "requires Python 3.11+ and runcrate>=0.5.0; CI runs via make wrroc-validate"]
fn g1_acceptance_at_least_17_of_23_fixtures_validate() {
    // Count actual fixture directories present before running the gate.
    // Fewer than 23 means the corpus is still being populated; skip
    // rather than fail so this test is dispatch-safe in partial states.
    let actual_fixture_count = std::fs::read_dir(fixtures_dir())
        .expect("read_dir(testdata/wrroc-fixtures)")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("intake.json").exists())
        .count();

    if actual_fixture_count < 23 {
        eprintln!(
            "G1 gate skipped: only {actual_fixture_count}/23 fixture directories present; \
             re-run after all 23 intake.json files have landed."
        );
        return;
    }

    let dir = tempfile::tempdir().expect("creating tempdir for emit roots");
    let mut package_dirs: Vec<PathBuf> = Vec::new();
    let mut emit_failures: Vec<String> = Vec::new();

    let mut entries: Vec<_> = std::fs::read_dir(fixtures_dir())
        .expect("read_dir(testdata/wrroc-fixtures)")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let intake_path = entry.path().join("intake.json");
        if !intake_path.exists() {
            continue;
        }

        let raw = std::fs::read_to_string(&intake_path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", intake_path.display()));
        let fixture: IntakeFixture = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("parsing {}: {e}", intake_path.display()));

        let pkg_dir = dir.path().join(&fixture.study_id);
        std::fs::create_dir_all(&pkg_dir)
            .unwrap_or_else(|e| panic!("create_dir_all({}): {e}", pkg_dir.display()));

        if emit_package_for_fixture(&fixture, &pkg_dir) {
            package_dirs.push(pkg_dir);
        } else {
            emit_failures.push(fixture.study_id.clone());
        }
    }

    assert_eq!(
        package_dirs.len() + emit_failures.len(),
        23,
        "expected 23 fixture intakes; processed {} (emitted {}, emit-failed {})",
        package_dirs.len() + emit_failures.len(),
        package_dirs.len(),
        emit_failures.len(),
    );

    let refs: Vec<&Path> = package_dirs.iter().map(|p| p.as_path()).collect();
    let report = PythonRuncrateWrrocValidator
        .validate_packages(&refs)
        .expect("invoking wrroc-validate.py");

    eprintln!("WRROC v0.5 validation report:");
    for r in &report.validated {
        eprintln!(
            "  {}: {} ({})",
            r.path,
            if r.ok { "OK" } else { "FAIL" },
            r.errors.join("; ")
        );
    }
    for fid in &emit_failures {
        eprintln!("  {fid}: EMIT_FAILED (counts as not-passed)");
    }
    eprintln!(
        "Summary: {}/{} validated, {} emit-failures, {} total fixtures",
        report.summary.passed,
        report.summary.total,
        emit_failures.len(),
        23,
    );

    assert!(
        report.summary.passed >= 17,
        "G1 acceptance: >=17/23 must validate; got {}/23 (validator passed {}, emit-failed {})",
        report.summary.passed,
        report.summary.passed,
        emit_failures.len(),
    );
}
