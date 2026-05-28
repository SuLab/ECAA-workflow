//! V4 schema-validation test for the modality-ontology
//! coverage matrix.

#[test]
fn modality_ontology_coverage_yaml_validates_against_schema() {
    let schema_path = "../../config/_modality-ontology-coverage.schema.json";
    let yaml_path = "../../config/modality-ontology-coverage.yaml";
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
        panic!("modality-ontology-coverage.yaml fails its schema");
    }
}
