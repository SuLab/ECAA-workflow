//! Behavioral tests for the `list_atoms` tool.
//!
//! Read-only catalog inspection added (Tool::COUNT 19 → 20).
//! Verifies: (a) unfiltered call returns the full catalog, (b) each
//! filter narrows correctly, (c) `produces_edam_data` filter matches
//! atoms used downstream (long-read DTU emits `data:0951`),
//! (d) truncation behavior with `max_results`.

use scripps_workflow_conversation::{dispatch_one, Session, Tool, ToolContext};
use std::path::{Path, PathBuf};

fn config_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config")
}

fn ctx() -> ToolContext {
    ToolContext::new(config_dir(), "claude-sonnet-4-6")
}

fn parse_args(args_json: serde_json::Value) -> Tool {
    // The enum variant uses `#[serde(flatten)]` so the filter fields
    // appear at the top level of the tool-call payload, alongside
    // `tool_name`. Merge them.
    let mut full = serde_json::json!({ "tool_name": "list_atoms" });
    if let Some(obj) = args_json.as_object() {
        for (k, v) in obj.iter() {
            full[k] = v.clone();
        }
    }
    serde_json::from_value::<Tool>(full).expect("Tool::ListAtoms deserializes")
}

#[tokio::test]
async fn unfiltered_returns_full_catalog() {
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({}));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error, "result: {:?}", result);
    let total = result.content["total_in_registry"]
        .as_u64()
        .expect("integer");
    let matched = result.content["matched"].as_u64().expect("integer");
    // ~73 atoms today; absolute floor accommodates renames without flakes.
    assert!(total >= 70, "expected ≥70 atoms in catalog, got {total}");
    assert_eq!(matched, total, "unfiltered match must equal total");
    let first = &result.content["atoms"][0];
    assert!(first["id"].is_string());
    assert!(first["role"].is_string());
    assert!(first["edam_operation"].is_string());
}

#[tokio::test]
async fn filter_by_modality_narrows_via_archetype_lookup() {
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({ "modality": "long_read_rnaseq" }));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error);
    let matched = result.content["matched"].as_u64().expect("integer");
    // long_read_rnaseq archetype references isoform_discovery + DTU at
    // Minimum (post archetype fix).
    assert!(
        matched >= 2,
        "expected ≥2 long_read_rnaseq atoms, got {matched}"
    );
    let ids: Vec<String> = result.content["atoms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.iter().any(|i| i == "differential_transcript_usage"),
        "long_read modality must surface differential_transcript_usage; got {ids:?}"
    );
}

#[tokio::test]
async fn filter_by_role_discovery_returns_discover_atoms_only() {
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({ "role": "discovery" }));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error);
    let atoms = result.content["atoms"].as_array().unwrap();
    assert!(!atoms.is_empty(), "expected ≥1 discovery atom");
    for atom in atoms {
        assert_eq!(atom["role"].as_str(), Some("discovery"));
    }
}

#[tokio::test]
async fn filter_by_produces_edam_data_0951() {
    // data:0951 = statistical estimate score (effect-size + p-value).
    // After the long-read fix, `differential_transcript_usage`
    // exposes this output.
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({ "produces_edam_data": "data:0951" }));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error);
    let ids: Vec<String> = result.content["atoms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        ids.contains(&"differential_transcript_usage".to_string()),
        "differential_transcript_usage should match data:0951; got {ids:?}"
    );
}

#[tokio::test]
async fn filter_by_has_method_choice_surfaces_deferred_to_target() {
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({ "has_method_choice": true }));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error);
    let atoms = result.content["atoms"].as_array().unwrap();
    assert!(!atoms.is_empty(), "expected ≥1 atom with method_choice");
    for atom in atoms {
        assert!(
            atom["method_choice_deferred_to"].is_string(),
            "has_method_choice=true atom must expose deferred_to: {atom}"
        );
    }
}

#[tokio::test]
async fn max_results_truncates() {
    let mut session = Session::new(false);
    let tool = parse_args(serde_json::json!({ "max_results": 3 }));
    let result = dispatch_one(&tool, &mut session, &ctx()).await;
    assert!(!result.is_error);
    assert_eq!(result.content["atoms"].as_array().unwrap().len(), 3);
    assert!(result.content["truncated"].as_bool().unwrap());
    let matched = result.content["matched"].as_u64().unwrap();
    assert!(matched > 3, "matched should exceed truncated bound");
}
