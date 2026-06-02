//! The interpretation-policy schema documents the additive `expected`
//! block and a policy carrying it validates clean.
use std::path::Path;

fn config_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/downstream-policy")
}

#[test]
fn schema_declares_expected_property() {
    let schema_path = config_dir().join("interpretation-policy.schema.json");
    let raw = std::fs::read_to_string(&schema_path).unwrap();
    let schema: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let expected = &schema["properties"]["verifiableEntities"]["properties"]["expected"];
    assert!(
        expected.is_object(),
        "schema must declare verifiableEntities.expected"
    );
    assert_eq!(expected["type"], serde_json::json!("array"));
}

#[test]
fn base_policy_with_expected_block_validates() {
    // The shipped base policy must still load + validate after the schema
    // gains the `expected` property (load_and_validate is the emit-time gate).
    let policy_path = config_dir().join("interpretation-policy.json");
    let res = ecaa_workflow_core::policy_schema::load_and_validate(&policy_path);
    assert!(
        res.is_ok(),
        "base interpretation-policy must validate: {res:?}"
    );
}
