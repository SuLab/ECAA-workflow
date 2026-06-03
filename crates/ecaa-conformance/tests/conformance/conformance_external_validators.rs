//! §8.5 / C5 Task 3 — under `ECAA_CONFORMANCE_MODE` the core emit path runs
//! the REAL external SHACL + OWL validators over the freshly-emitted package
//! (not the `unavailable` stubs the product build records), serializes the
//! package ABox to `package.ttl`, and blocks on failure.
//!
//! The test is CAPABILITY-PROBED: when `python3` + `pyld` + `rdflib` +
//! `pyshacl` + `owlready2` are not all importable it prints a LOUD skip notice
//! and returns (it does NOT fail), so the suite is dispatch-safe on a machine
//! without the validator toolchain. The skip is shouted so a deps-absent
//! vacuous pass can never be mistaken for a real external-validation pass. The
//! gate runs for real where the deps are installed (e.g. `make wrroc-validate`
//! / the D9 conformance run). Install the toolchain with:
//!
//! ```text
//! pip install --user --break-system-packages pyshacl pyld owlready2 rdflib runcrate
//! ```
//!
//! (or the pinned set in `requirements-validator.txt`).
//!
//! Asserts, over a real composer → `emit_package` package:
//!   * `external_validation.shacl_projection.status == "pass"`
//!   * `external_validation.owl_consistency.status == "pass"` (over the
//!     package ABox, not just the static ontology)
//!   * `<pkg>/package.ttl` was produced (the §8.5 typed-node Turtle dump).
//!
//! `ECAA_CONFORMANCE_MODE` is process-global env, so the test is `#[serial]`
//! and restores the prior env before returning.

use ecaa_workflow_core::emitter::{emit_package, EmitConfig};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Operator-facing install hint surfaced in the probe-skip notice so a
/// deps-absent vacuous pass is loudly distinguishable from a real external
/// SHACL/OWL pass. Mirrors the pinned set in `requirements-validator.txt`.
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

/// True when python3 plus every dep BOTH external checks need is importable.
/// project_package.py needs pyld/rdflib/pyshacl; owl_consistency.py needs
/// owlready2/rdflib (+ pyld for the ABox projection). The OWL check returns
/// `unavailable` (not `pass`) when owlready2 is absent, so probing it here is
/// required for the `owl_consistency == pass` assertion to be meaningful.
fn validators_available() -> bool {
    Command::new("python3")
        .args(["-c", "import pyld, rdflib, pyshacl, owlready2"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Build a real DAG via the v4 composer (mirrors ablation_contract.rs).
fn minimal_dag() -> ecaa_workflow_core::dag::DAG {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    use ecaa_workflow_core::atom_registry::AtomRegistry;
    use ecaa_workflow_core::builder::{build_dag_from_composition, build_dag_from_workflow_dag};
    use ecaa_workflow_core::composer::compose_with_modalities_full;
    use ecaa_workflow_core::goal_spec::GoalSpec;
    let config = config_root();
    let atoms = AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("atoms");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("archetypes");
    let goal = GoalSpec {
        edam_data: "data:9999".into(),
        edam_format: None,
        modifiers: Default::default(),
        source_prose: Some("conformance external-validator fixture".into()),
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
    if let Some(wf) = out.workflow_dag.as_ref() {
        build_dag_from_workflow_dag(wf, "conformance-external-fixture").expect("lower")
    } else {
        build_dag_from_composition(
            &out.composition,
            "conformance-external-fixture",
            &BTreeMap::new(),
            &[],
        )
        .expect("lower")
    }
}

fn classification() -> ecaa_workflow_core::classify::ClassificationResult {
    use ecaa_workflow_core::classify::ClassificationResult;
    ClassificationResult {
        modality: "bulk_rnaseq".into(),
        taxonomy_path: "config/stage-taxonomies/rnaseq-de.yaml".into(),
        domain: "computational biology".into(),
        workflow_description: "conformance external-validator fixture".into(),
        edam_topic: "topic:3308".into(),
        edam_operation: "operation:3223".into(),
        confidence: 0.85,
        confidence_label: "high".into(),
        organisms: vec![],
        methods_specified: vec![],
        data_sources: vec![],
        intake_text: "bulk RNA-seq conformance external-validator fixture".into(),
        goal: None,
        archetype_id: None,
        additional_modalities: vec![],
        tie_candidates: vec![],
    }
}

fn emit_into(out: &Path) -> anyhow::Result<()> {
    let dag = minimal_dag();
    let clf = classification();
    let policies_dir = config_root().join("downstream-policy");
    emit_package(&EmitConfig {
        output_dir: out,
        dag: &dag,
        classification: &clf,
        policies_dir: &policies_dir,
        policy_allowlist: None,
        claim_boundary: None,
        compute_profiles_dir: None,
        intake_facts: None,
        amend_from: None,
        amend_context: None,
        validation_contract_ref: None,
        preferred_container: None,
        runtime_prereqs: None,
        per_atom_runtime_prereqs: None,
        stage_atoms_dir: None,
        edge_kinds: None,
    })
}

/// RAII guard: set an env var on construction, restore its prior value (or
/// remove it if it was unset) on Drop — so a panic mid-test doesn't leak
/// `ECAA_CONFORMANCE_MODE` into the rest of the (serial) suite.
struct EnvGuard {
    key: &'static str,
    prev: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let prev = std::env::var(key).ok();
        std::env::set_var(key, value);
        Self { key, prev }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var(self.key, v),
            None => std::env::remove_var(self.key),
        }
    }
}

fn check_status(external: &serde_json::Value, name: &str) -> Option<String> {
    external
        .get(name)
        .and_then(|c| c.get("status"))
        .and_then(|s| s.as_str())
        .map(str::to_string)
}

#[test]
#[serial_test::serial]
fn conformance_mode_runs_real_external_validators_and_writes_package_ttl() {
    if !validators_available() {
        eprintln!(
            "\n>>> SKIP: python3 + pyld/rdflib/pyshacl/owlready2 not all importable <<<\n\
             >>> conformance_mode_runs_real_external_validators_and_writes_package_ttl did NOT run \
             — this is NOT an external SHACL/OWL pass. <<<\n\
             >>> Install the validator toolchain to run this gate for real:\n\
             >>>   {VALIDATOR_INSTALL_HINT}\n"
        );
        return;
    }

    // Conformance mode forces Full + block-on-fail and makes the core emit
    // path actually shell the external validators. Restored on Drop.
    let _conf = EnvGuard::set("ECAA_CONFORMANCE_MODE", "1");
    // Belt-and-suspenders: explicit Full so the assertion is independent of
    // the conformance-mode → Full upgrade in read_validation_mode().
    let _mode = EnvGuard::set("ECAA_VALIDATE_ON_EMIT", "full");

    let dir = tempfile::tempdir().expect("tempdir");
    // emit_package runs the external suite + block-on-fail itself; a SHACL/OWL
    // failure would surface here as an Err, which is itself a conformance bug.
    emit_into(dir.path())
        .expect("emit_package under conformance mode (external SHACL/OWL must pass)");

    // package.ttl: the §8.5 typed-node ABox dump produced by project_package.py.
    let ttl = dir.path().join("package.ttl");
    assert!(
        ttl.exists(),
        "project_package.py must serialize package.ttl into the package dir; not found at {}",
        ttl.display()
    );
    let ttl_body = std::fs::read_to_string(&ttl).expect("read package.ttl");
    assert!(
        !ttl_body.trim().is_empty(),
        "package.ttl must contain a serialized ABox (got empty file)"
    );

    // validation-summary.json carries the external-validator verdicts.
    let summary_raw = std::fs::read_to_string(dir.path().join("runtime/validation-summary.json"))
        .expect("validation-summary.json");
    let summary: serde_json::Value = serde_json::from_str(&summary_raw).expect("parse summary");
    assert_eq!(
        summary["mode"].as_str(),
        Some("full"),
        "conformance mode must run the Full validation tier"
    );
    let external = &summary["external_validation"];
    assert!(
        external.is_object(),
        "Full mode must populate external_validation; got {external}"
    );

    let shacl = check_status(external, "shacl_projection");
    assert_eq!(
        shacl.as_deref(),
        Some("pass"),
        "SHACL projection must PASS over the emitted package; external={external}"
    );

    let owl = check_status(external, "owl_consistency");
    assert_eq!(
        owl.as_deref(),
        Some("pass"),
        "OWL consistency must PASS over the package ABox (ontology + individuals); external={external}"
    );
}
