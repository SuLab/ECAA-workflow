//! Verifies TaskId round-trips through serde as a string.

use scripps_workflow_core::ids::TaskId;

#[test]
fn taskid_serializes_as_bare_string() {
    let id: TaskId = "task-bulk-rnaseq-1".into();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""task-bulk-rnaseq-1""#);
}

#[test]
fn taskid_deserializes_from_bare_string() {
    let id: TaskId = serde_json::from_str(r#""task-x""#).unwrap();
    // .as_ref() works on String (Step A) and will work on the newtype (Step B)
    let s: &str = id.as_ref();
    assert_eq!(s, "task-x");
}

#[test]
fn taskid_btreemap_lookup_by_str() {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<TaskId, i32> = BTreeMap::new();
    m.insert("task-x".into(), 42);
    // Borrow<str> impl is what makes this work in Step B.
    // In Step A (type alias = String) this trivially works.
    assert_eq!(m.get("task-x").copied(), Some(42));
}
