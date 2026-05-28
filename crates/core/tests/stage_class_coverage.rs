//! Invariant: every stageClass listed in best-practice-validation-contract.json
//! cycleRequirements must have a matching entry in best-practice-scoring-policy.json
//! stageClassRules. Otherwise method-selection blockers for that class
//! cannot compute a composite score.

use std::collections::HashSet;

fn policies_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/downstream-policy")
}

fn classes(file: &str, key: &str) -> HashSet<String> {
    let path = policies_dir().join(file);
    let bytes = std::fs::read(&path).expect("read policy");
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("parse policy");
    v[key]
        .as_array()
        .expect("array")
        .iter()
        .map(|e| e["stageClass"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn every_validation_contract_class_has_scoring_rule() {
    let contract = classes(
        "best-practice-validation-contract.json",
        "cycleRequirements",
    );
    let scoring = classes("best-practice-scoring-policy.json", "stageClassRules");
    let mut missing: Vec<_> = contract.difference(&scoring).collect();
    missing.sort();
    assert!(
        missing.is_empty(),
        "stageClasses in validation-contract but not in scoring-policy: {:?}",
        missing
    );
}
