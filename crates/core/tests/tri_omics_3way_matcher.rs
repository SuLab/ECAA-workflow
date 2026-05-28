//! Tri-omics superset-fallback test for the broadened
//! `find_match_cross_omics` matcher.
//!
//! When the classifier under-counts modalities (returns 2 of 3) but the
//! SME prose explicitly names n-way intent ("tri-omics", "three-way
//! analysis"), the matcher's superset fallback should rescue the
//! 3-way archetype. Without n_way_intent the matcher refuses to
//! superset-match a 2-modality input against a 3-modality archetype
//! (orphan-branch prevention).

#[test]
fn tri_omics_matcher_picks_3way_archetype_via_subset() {
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    let archetypes = ArchetypeRegistry::load_from_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/archetypes")
            .as_path(),
    )
    .unwrap();
    // Classifier produced only 2 of 3 modalities — common failure mode.
    let matches = archetypes.find_match_cross_omics(
        "data:0951",
        Some("format:3475"),
        "bioinformatics",
        &["bulk_rnaseq", "atac_seq"],
        None,
        true, // n_way_intent set — SME prose mentions "tri-omics"
        "",
    );
    assert!(
        !matches.is_empty(),
        "n-way intent should rescue subset matches"
    );
    let top = matches[0].0;
    assert!(
        top.cross_omics_modalities.contains(&"chip_seq".to_string()),
        "should pick the 3-way archetype that supersets [bulk_rnaseq, atac_seq]; \
         got {:?} with cross_omics_modalities={:?}",
        top.id,
        top.cross_omics_modalities
    );
}

#[test]
fn tri_omics_matcher_refuses_superset_without_intent() {
    // Same 2-modality input, but n_way_intent=false. The matcher must
    // NOT rescue the 3-way archetype — falling through to the generic
    // multi-modal synthesizer is the correct conservative path when
    // the SME hasn't named n-way intent.
    use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
    let archetypes = ArchetypeRegistry::load_from_dir(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("config/archetypes")
            .as_path(),
    )
    .unwrap();
    let matches = archetypes.find_match_cross_omics(
        "data:0951",
        Some("format:3475"),
        "bioinformatics",
        &["bulk_rnaseq", "atac_seq"],
        None,
        false, // no n-way intent
        "",
    );
    // The 2-way [bulk_rnaseq, atac_seq] set isn't authored in the
    // catalog as a strict set-match; without n_way_intent the matcher
    // returns empty (no superset rescue allowed).
    let any_3way = matches
        .iter()
        .any(|(a, _)| a.cross_omics_modalities.len() >= 3);
    assert!(
        !any_3way,
        "without n_way_intent the matcher must not return 3-way supersets; \
         got {:?}",
        matches
            .iter()
            .map(|(a, _)| (&a.id, &a.cross_omics_modalities))
            .collect::<Vec<_>>()
    );
}

#[test]
fn is_n_way_intent_detects_tri_omics_phrasings() {
    use ecaa_workflow_core::classify::is_n_way_intent;
    assert!(is_n_way_intent("we want a tri-omics analysis"));
    assert!(is_n_way_intent("Three-way analysis of RNA+ATAC+ChIP."));
    assert!(is_n_way_intent("Tri omics integration pipeline."));
    assert!(is_n_way_intent("three omics layered"));
    assert!(is_n_way_intent("n-way integration of all layers"));
    assert!(!is_n_way_intent("RNA-seq and proteomics joint analysis."));
    assert!(!is_n_way_intent("cross-omics two-way join"));
}
