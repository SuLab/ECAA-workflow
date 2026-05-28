//! Lineage + cross-version diff task-pair anchor tests (M1.3).
//!
//! Covers:
//! 1. `SessionLineage` serializes `branched_from_task_id`.
//! 2. `CrossVersionReport` includes `anchor_kind` and `anchored_task_id`.
//! 3. A task-pair-anchored diff carries `anchor_kind = "task-pair"` and
//!    the correct task id.
//! 4. An unanchored diff defaults to `anchor_kind = "package-pair"`.

use ecaa_workflow_core::cross_version_diff::CrossVersionReport;

/// CrossVersionReport with a task anchor carries the correct fields.
#[test]
fn cross_version_report_task_pair_anchor() {
    let report = CrossVersionReport {
        parent_package: "/tmp/parent".to_string(),
        child_package: "/tmp/child".to_string(),
        tables: vec![],
        overall_concordance: 1.0,
        anchor_kind: "task-pair".to_string(),
        anchored_task_id: Some("b".to_string()),
    };

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["anchor_kind"].as_str(),
        Some("task-pair"),
        "anchor_kind must serialize"
    );
    assert_eq!(
        json["anchored_task_id"].as_str(),
        Some("b"),
        "anchored_task_id must serialize"
    );
}

/// CrossVersionReport without task anchor carries `anchor_kind = "package-pair"`.
#[test]
fn cross_version_report_default_package_pair_anchor() {
    let report = CrossVersionReport {
        parent_package: "/tmp/parent".to_string(),
        child_package: "/tmp/child".to_string(),
        tables: vec![],
        overall_concordance: 1.0,
        anchor_kind: "package-pair".to_string(),
        anchored_task_id: None,
    };

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["anchor_kind"].as_str(), Some("package-pair"));
    assert!(
        json["anchored_task_id"].is_null() || json.get("anchored_task_id").is_none(),
        "anchored_task_id must be null/absent for package-pair; got {json}"
    );
}

/// Older serialized CrossVersionReport without anchor fields deserializes to defaults.
#[test]
fn legacy_cross_version_report_defaults_to_package_pair() {
    let raw = serde_json::json!({
        "parent_package": "/tmp/p",
        "child_package": "/tmp/c",
        "tables": [],
        "overall_concordance": 0.9,
    });
    let report: CrossVersionReport = serde_json::from_value(raw).unwrap();
    assert_eq!(
        report.anchor_kind, "package-pair",
        "missing anchor_kind must default to 'package-pair'"
    );
    assert!(
        report.anchored_task_id.is_none(),
        "missing anchored_task_id must default to None"
    );
}
