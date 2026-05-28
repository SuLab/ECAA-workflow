//! Adding a 4th project class is a single YAML drop +
//! fixture test, not a Rust code edit.
//!
//! This test proves that the `ProjectClassRegistry` accepts a
//! synthetic `survey` class authored only in YAML and exposes its
//! metadata to consumers without any code change. The closed
//! `ProjectClass` enum stays in place for type-system exhaustiveness;
//! consumers that need to dispatch on the new class still need an
//! enum variant addition. The registry layer covers the
//! prompt-addendum + container-policy + BCO-routing surface that
//! used to require per-class match arms.

use ecaa_workflow_core::project_class_registry::ProjectClassRegistry;
use std::path::Path;

#[test]
fn synthetic_4th_class_loads_from_yaml_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Copy the schema sidecar so the loader passes validation.
    let schema_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/project-classes/_project_class.schema.json");
    std::fs::copy(&schema_src, tmp.path().join("_project_class.schema.json")).expect("schema copy");

    // Drop a 4th class YAML — no Rust edits required for the registry
    // to accept it.
    std::fs::write(
        tmp.path().join("survey.yaml"),
        r#"
id: survey
display_name: Survey Research
prompt_addendum_path: project-class-prompts/survey.txt
taxonomy_pattern: "survey-*.yaml"
requires_container: false
auto_bco: true
role_addenda:
  Validation: project-class-prompts/survey.validation.txt
  Pilot: project-class-prompts/survey.pilot.txt
"#,
    )
    .expect("write synthetic class");

    let reg = ProjectClassRegistry::load_from_dir(tmp.path()).expect("registry load");
    let survey = reg.get("survey").expect("survey class must register");
    assert_eq!(survey.id, "survey");
    assert_eq!(survey.display_name, "Survey Research");
    assert_eq!(
        survey.prompt_addendum_path.as_deref(),
        Some("project-class-prompts/survey.txt")
    );
    assert_eq!(survey.taxonomy_pattern.as_deref(), Some("survey-*.yaml"));
    assert!(survey.auto_bco, "auto_bco must round-trip from YAML");
    assert!(!survey.requires_container);

    // role_addenda lookup picks the per-role override when declared.
    assert_eq!(
        survey.role_addenda.get("Validation"),
        Some(&"project-class-prompts/survey.validation.txt".to_string())
    );
    assert_eq!(
        survey.role_addenda.get("Pilot"),
        Some(&"project-class-prompts/survey.pilot.txt".to_string())
    );
    assert!(
        !survey.role_addenda.contains_key("Operation"),
        "Operation role has no override; lookup falls back to per-class addendum"
    );
}

#[test]
fn schema_validates_required_fields() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let schema_src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/project-classes/_project_class.schema.json");
    std::fs::copy(&schema_src, tmp.path().join("_project_class.schema.json")).unwrap();

    // Missing display_name — must fail schema validation.
    std::fs::write(tmp.path().join("incomplete.yaml"), "id: incomplete\n").unwrap();
    assert!(
        ProjectClassRegistry::load_from_dir(tmp.path()).is_err(),
        "schema must reject classes missing display_name"
    );
}
