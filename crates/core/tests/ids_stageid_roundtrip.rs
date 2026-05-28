//! StageId serde + BTreeMap roundtrip.

use ecaa_workflow_core::ids::StageId;

#[test]
fn stageid_serializes_as_bare_string() {
    let id: StageId = "discover_diffexp".into();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""discover_diffexp""#);
}

#[test]
fn stageid_deserializes_from_bare_string() {
    let id: StageId = serde_json::from_str(r#""compute_pca""#).unwrap();
    assert_eq!(id.as_str(), "compute_pca");
}

#[test]
fn stageid_btreemap_lookup_by_str() {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<StageId, i32> = BTreeMap::new();
    m.insert("x".into(), 42);
    assert_eq!(m.get("x").copied(), Some(42));
}
