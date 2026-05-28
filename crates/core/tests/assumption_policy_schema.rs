//! Schema-validation test for the assumption-policy table.

#[test]
fn assumption_policy_yaml_validates_against_schema() {
    let schema_path = "../../config/_assumption-policy.schema.json";
    let yaml_path = "../../config/assumption-policy.yaml";
    let schema_raw = std::fs::read_to_string(schema_path).expect("schema must exist");
    let yaml_raw = std::fs::read_to_string(yaml_path).expect("yaml must exist");
    let yaml_value: serde_json::Value = serde_yml::from_str(&yaml_raw).expect("yaml parses");
    let schema_value: serde_json::Value = serde_json::from_str(&schema_raw).expect("schema parses");
    let compiled = jsonschema::JSONSchema::compile(&schema_value).expect("schema compiles");
    let result = compiled.validate(&yaml_value);
    if let Err(errors) = result {
        for err in errors {
            eprintln!("{}", err);
        }
        panic!("assumption-policy.yaml fails its schema");
    }
}
