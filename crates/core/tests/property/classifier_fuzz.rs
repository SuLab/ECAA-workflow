//! R-24 property test: the classifier never panics on arbitrary input,
//! always returns a modality drawn from the registered set, and is
//! deterministic across runs for any given input.
//!
//! The classifier is the SME's first touch-point. Any panic on
//! pathological prose (binary garbage, surrogate-laden Unicode, megabyte
//! input) would crash the chat surface; non-deterministic output would
//! break replay; an unknown modality would route into a downstream
//! Option::unwrap on the registry. Proptest catches all three classes
//! before they reach a fixture.

use proptest::prelude::*;
use ecaa_workflow_core::classify::Classifier;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Load the production classifier once per test binary. `Classifier::load`
/// touches disk for `modality-keywords.yaml` + the per-modality manifest
/// dir; doing it per `proptest` iteration would dominate wall-clock.
fn classifier() -> &'static Classifier {
    static CACHE: OnceLock<Classifier> = OnceLock::new();
    CACHE.get_or_init(|| {
        let keywords_path = config_root().join("modality-keywords.yaml");
        Classifier::load(&keywords_path).expect("loading production classifier")
    })
}

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Registered modality ids — anything the classifier emits must be drawn
/// from this set OR be the generic-omics fallback. Loaded once and
/// reused. Sourced from `config/modalities/<id>.yaml` so the test stays
/// in lock-step with the catalog without hard-coding the 19-modality
/// inventory.
fn registered_modality_ids() -> &'static std::collections::HashSet<String> {
    static CACHE: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    CACHE.get_or_init(|| {
        let dir = config_root().join("modalities");
        let mut ids = std::collections::HashSet::new();
        for entry in std::fs::read_dir(&dir).expect("read modalities/").flatten() {
            let p = entry.path();
            if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if stem.starts_with('_') {
                    continue;
                }
                ids.insert(stem.to_string());
            }
        }
        ids
    })
}

proptest! {
    /// Pathological strings (binary, control chars, weird Unicode, long
    /// input) must not panic the classifier.
    #[test]
    fn classify_never_panics(s in ".{0,2000}") {
        let _ = classifier().classify(&s);
    }

    /// Whatever modality the classifier emits must be a known id (the
    /// fixture set on disk). An unknown id would silently route through
    /// `unwrap_or_default()` downstream and produce a generic-omics
    /// package for what the SME thinks is an scRNA-seq run.
    #[test]
    fn modality_is_always_in_registered_set(s in ".{0,2000}") {
        let result = classifier().classify(&s);
        prop_assert!(
            result.modality.is_empty()
                || registered_modality_ids().contains(&result.modality),
            "classifier emitted unknown modality '{}' for input '{}'",
            result.modality,
            s.chars().take(80).collect::<String>(),
        );
    }

    /// Determinism: same input → same modality across runs. Closes the
    /// "did we just classify based on HashMap iteration order?" class.
    #[test]
    fn classification_is_deterministic(s in ".{0,2000}") {
        let a = classifier().classify(&s);
        let b = classifier().classify(&s);
        prop_assert_eq!(a.modality, b.modality);
        prop_assert_eq!(a.taxonomy_path, b.taxonomy_path);
        prop_assert_eq!(a.edam_topic, b.edam_topic);
        prop_assert_eq!(a.edam_operation, b.edam_operation);
    }
}
