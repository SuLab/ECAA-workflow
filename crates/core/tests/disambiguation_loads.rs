#[test]
fn disambiguation_registry_loads_diablo_vs_mofa() {
    use scripps_workflow_core::disambiguation::DisambiguationRegistry;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("config/classifier-disambiguation.yaml");
    let reg = DisambiguationRegistry::load(&path).unwrap();
    let pair = reg.pairs.iter().find(|p| p.id == "diablo_vs_mofa").unwrap();
    assert_eq!(pair.rivals.len(), 2);
    assert_eq!(pair.quick_replies.len(), 2);
}
