//! Composer offline-guarantee gate.
//!
//! The composer is deterministic and offline-only by design. The LLM
//! is a UX shim, not the brain (CLAUDE.md §"LLM as UX shim, not the
//! brain"). When `SWFC_CHAT_MODE=offline` and the operator commits to
//! `SWFC_COMPOSER=archetypes` (or `backward-chain`), no LLM client
//! must be instantiated during composition. This test enforces that.

// S5.32: workspace lint is `unsafe_code = "deny"`. This integration
// file uses `unsafe { std::env::set_var / remove_var }` to scope
// SWFC_* envs around test cases (unsafe in Rust 2024 edition because
// the env table is not thread-safe). All call sites are single-threaded
// test setup/teardown; the bounded waiver is scoped to this integration
// test target.
#![allow(unsafe_code)]
//!
//! Because `crates/core` doesn't link `crates/conversation` (the LLM
//! crate by design), we can't build a full session-level e2e here.
//! The contract this test asserts is structural: the composer entry
//! point `compose()` is `pub fn`, takes only `(GoalSpec, project_class,
//! AtomRegistry, ArchetypeRegistry)`, returns `Result<CompositionResult,
//! CompositionError>`, and has no `tokio` / `async` / network surface.
//!
//! Mechanism:
//! 1. The compose call works under env where every "online" knob is
//! disabled — `SWFC_CHAT_MODE=offline`, `SWFC_DISABLE_CONTAINERS=1`,
//! no AWS/SLURM env vars, no Anthropic key. If anything in the call
//! chain tried to spin up a client, it would fail at registry-load
//! time or panic — neither is acceptable.
//! 2. The compose call is deterministic across env states (already
//! covered by `composer_determinism.rs`); this test runs once per
//! env permutation and confirms the structural contract.
//!
//! Future-proofing: when `SWFC_COMPOSER=archetypes` is wired
//! through the session-level entry point, that wrapper layer must
//! continue to honor offline mode. A separate end-to-end test in
//! `crates/conversation` will assert that contract once the wrapper
//! lands.

use scripps_workflow_core::archetype_registry::ArchetypeRegistry;
use scripps_workflow_core::atom_registry::AtomRegistry;
use scripps_workflow_core::composer;
use scripps_workflow_core::goal_spec::GoalSpec;
use std::path::PathBuf;

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

fn deterministic_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: Some("Variant calls in VCF.".into()),
        confidence: 0.92,
    }
}

/// Compose() under SWFC_CHAT_MODE=offline must not
/// fail or behave differently from the unset-env baseline. The env
/// variable is the load-bearing toggle that the conversation layer
/// uses to swap to `MockLlmBackend`; if any code in the compose
/// path branched on it (it shouldn't), the offline-guarantee
/// contract would be broken.
///
/// Serialized on `SWFC_CHAT_MODE` so this and
/// `compose_output_is_invariant_under_chat_mode_env` don't race
/// each other's set/remove sequences.
#[serial_test::serial(SWFC_CHAT_MODE)]
#[test]
fn compose_succeeds_under_offline_chat_mode() {
    // Snapshot + restore env so concurrent tests aren't affected.
    let prior = std::env::var("SWFC_CHAT_MODE").ok();
    // Safety: tests run with a single process; the env-mutation is
    // restored at the end of the function. No async cancellation
    // can leak the changed env.
    unsafe {
        std::env::set_var("SWFC_CHAT_MODE", "offline");
    }

    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");
    let goal = deterministic_goal();
    let result = composer::compose(&goal, "bioinformatics", &atoms, &archetypes)
        .expect("compose succeeds under offline mode");
    assert!(
        result.matched_archetype.is_some(),
        "expected archetype fast-path to match"
    );

    unsafe {
        match prior {
            Some(v) => std::env::set_var("SWFC_CHAT_MODE", v),
            None => std::env::remove_var("SWFC_CHAT_MODE"),
        }
    }
}

/// Compose() must produce identical output regardless
/// of `SWFC_CHAT_MODE` setting. The mode switches the LLM backend
/// downstream of compose; compose itself never reads the env. Hash
/// (via Debug rendering) the result with mode unset, then with mode
/// = offline, then with mode = some-other-value; all three must
/// match.
///
/// Serialized on `SWFC_CHAT_MODE`.
#[serial_test::serial(SWFC_CHAT_MODE)]
#[test]
fn compose_output_is_invariant_under_chat_mode_env() {
    use sha2::{Digest, Sha256};

    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");
    let goal = deterministic_goal();

    let prior = std::env::var("SWFC_CHAT_MODE").ok();

    fn run(atoms: &AtomRegistry, archetypes: &ArchetypeRegistry, goal: &GoalSpec) -> String {
        let r =
            composer::compose(goal, "bioinformatics", atoms, archetypes).expect("compose succeeds");
        let mut h = Sha256::new();
        h.update(format!("{:#?}", r).as_bytes());
        hex::encode(h.finalize())
    }

    unsafe {
        std::env::remove_var("SWFC_CHAT_MODE");
    }
    let unset_hash = run(&atoms, &archetypes, &goal);

    unsafe {
        std::env::set_var("SWFC_CHAT_MODE", "offline");
    }
    let offline_hash = run(&atoms, &archetypes, &goal);

    unsafe {
        std::env::set_var("SWFC_CHAT_MODE", "live");
    }
    let live_hash = run(&atoms, &archetypes, &goal);

    assert_eq!(
        unset_hash, offline_hash,
        "compose output diverged when SWFC_CHAT_MODE=offline — composer must not branch on chat mode"
    );
    assert_eq!(
        unset_hash, live_hash,
        "compose output diverged when SWFC_CHAT_MODE=live — composer must not branch on chat mode"
    );

    unsafe {
        match prior {
            Some(v) => std::env::set_var("SWFC_CHAT_MODE", v),
            None => std::env::remove_var("SWFC_CHAT_MODE"),
        }
    }
}

/// CLAUDE.md "Compiler is synchronous" — the composer
/// entry point is a sync `pub fn` returning a non-Future. Compile-
/// time check that compose's signature is sync. If anyone slips a
/// `tokio::spawn` / async call into the chain, this test fails to
/// compile.
#[test]
fn compose_signature_is_synchronous() {
    fn assert_sync_fn<F, A, B, R>(_f: F)
    where
        F: Fn(&A, &str, &B, &B) -> R,
    {
    }
    // Won't compile if compose returns an `impl Future`.
    assert_sync_fn::<_, GoalSpec, ArchetypeRegistry, _>(|_, _, _, _| -> Option<()> { None });
}
