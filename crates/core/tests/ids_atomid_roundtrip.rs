//! AtomId serde + BTreeMap roundtrip.
use ecaa_workflow_core::ids::AtomId;

#[test]
fn atomid_serializes_as_bare_string() {
    let id: AtomId = "compute_pca_from_normalized_matrix".into();
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(json, r#""compute_pca_from_normalized_matrix""#);
}

#[test]
fn atomid_deserializes_from_bare_string() {
    let id: AtomId = serde_json::from_str(r#""x""#).unwrap();
    assert_eq!(id.as_str(), "x");
}

#[test]
fn atomid_btreemap_lookup_by_str() {
    use std::collections::BTreeMap;
    let mut m: BTreeMap<AtomId, i32> = BTreeMap::new();
    m.insert("x".into(), 42);
    assert_eq!(m.get("x").copied(), Some(42));
}
