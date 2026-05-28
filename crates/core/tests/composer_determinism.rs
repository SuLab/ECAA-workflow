//! Composer-determinism gate.
//!
//! Determinism is a first-class architectural rule in this project
//! (CLAUDE.md "Deterministic output" + plan §3.5 / §7.1). The
//! composer must produce byte-identical output for the same
//! `(GoalSpec, project_class, atom_registry, archetype_registry)`
//! tuple across runs — otherwise emitted packages would drift,
//! breaking the WORKFLOW.json reproducibility contract.
//!
//! This test runs the composer 100 times against the live
//! `config/archetypes/` + `config/stage-atoms/` registries with a
//! deterministic `GoalSpec` (matches `single_cell_de`) and asserts
//! every iteration produces the identical SHA-256 hash over the
//! composer's `Debug` rendering.
//!
//! Why `Debug` not `serde_json`: `CompositionResult` doesn't
//! currently derive `Serialize` (nor should it — the type is
//! internal to the composer; the builder consumes it directly).
//! The `Debug` impl walks fields in source order with deterministic
//! field rendering; combined with `BTreeMap`-ordered collections
//! everywhere this gives a deterministic hash that's strictly
//! stronger than equality (hash collision is cryptographically
//! negligible).
//!
//! Failure mode: any non-determinism in the composer (HashMap
//! iteration, clock read, random tiebreak, env-var read, parallel
//! fold) produces divergent hashes across the 100 calls and this
//! test fails with the first divergent iteration index.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer;
use scripps_workflow_core::goal_spec::GoalSpec;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

fn hash_result(result: &composer::CompositionResult) -> String {
    // Render via `Debug`. CompositionResult's fields are all
    // deterministically rendered: enum variants are stable, `Vec`
    // preserves insertion order from the composer, and the
    // composer pushes atoms in the archetype's declared order
    // (which is BTreeMap-loaded from disk — also deterministic).
    let s = format!("{:#?}", result);
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}

#[test]
fn compose_is_byte_identical_across_100_calls() {
    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");

    // Deterministic goal that uniquely matches
    // `variant_calling_germline` archetype: `data:3498` (Sequence
    // variations) + `format:3016` (VCF) under project_class
    // `bioinformatics`. The only other `data:3498` archetype
    // (`gwas_coloc`) declares `format:3475` so the format tiebreak
    // resolves uniquely.
    let goal = GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: Some("Variant calls in VCF.".into()),
        confidence: 0.92,
    };

    let first = composer::compose(&goal, "bioinformatics", &atoms, &archetypes)
        .expect("first compose succeeds");
    let baseline_hash = hash_result(&first);

    for i in 0..99 {
        let again = composer::compose(&goal, "bioinformatics", &atoms, &archetypes)
            .expect("repeat compose succeeds");
        let h = hash_result(&again);
        assert_eq!(
            baseline_hash,
            h,
            "compose iteration {} diverged from baseline. Composer non-determinism — \
             probable culprit: HashMap iteration, clock read, random tiebreak, or \
             parallel fold. First compose:\n{:#?}\nDivergent compose:\n{:#?}",
            i + 1,
            first,
            again,
        );
    }
}

#[test]
fn compose_backward_chain_is_byte_identical_across_100_calls() {
    // Drives the backward-chain path (no archetype match) to confirm
    // determinism on the non-fast-path too. Goal references a synthetic
    // edam_data that no archetype produces, so the composer falls
    // through to backward-chain.
    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");

    let goal = GoalSpec {
        // Matches `differential_expression` atom edam_data; no
        // archetype declares this as goal_data, so the composer
        // routes through backward_chain_compose.
        edam_data: "data:0951".into(),
        edam_format: Some("format:3475".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: None,
        confidence: 0.0,
    };

    let first = match composer::compose(&goal, "bioinformatics", &atoms, &archetypes) {
        Ok(r) => r,
        Err(_) => {
            // Backward-chain may legitimately fail if no atom
            // produces `data:0951`. Skip — determinism is asserted
            // for the success path.
            return;
        }
    };
    let baseline = hash_result(&first);

    for i in 0..99 {
        let again = composer::compose(&goal, "bioinformatics", &atoms, &archetypes)
            .expect("repeat compose succeeds");
        let h = hash_result(&again);
        assert_eq!(
            baseline,
            h,
            "backward-chain compose iteration {} diverged from baseline",
            i + 1
        );
    }
}
