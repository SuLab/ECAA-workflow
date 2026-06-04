//! Concurrency + determinism coverage for the session-keyed verifier
//! decision substrate (core-01 / core-03).
//!
//! The single-threaded composer-determinism gate
//! (`tests/composer/composer_determinism.rs`) asserts byte-stability of
//! the *composition*. This sibling adds the concurrency dimension the
//! substrate fix exists to protect: two server sessions composing at the
//! same time, on separate OS threads, must each drain only their own
//! verifier decisions. Before the substrate became session-keyed, both
//! threads pushed into one process-wide `Vec` and the first to drain
//! scooped the other's rows — silently contaminating
//! `runtime/verifier-decisions.jsonl`.
//!
//! Auto-discovered as its own integration-test crate (top-level
//! `tests/*.rs` file; no `[[test]]` entry needed).

use ecaa_workflow_core::archetype_registry::ArchetypeRegistry;
use ecaa_workflow_core::atom_registry::AtomRegistry;
use ecaa_workflow_core::composer;
use ecaa_workflow_core::decision_substrate::{self, VerifierDecision};
use ecaa_workflow_core::goal_spec::GoalSpec;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::thread;

/// Serializes the two `#[test]`s in this binary so neither touches the
/// shared substrate's *unscoped default bucket* (or runs an unscoped
/// merge-all `drain()`) while the other is mid-flight. The concurrent
/// test still spawns its own worker threads under their own session
/// scopes — those are isolated by the session key, not this guard.
static SUBSTRATE_GUARD: Mutex<()> = Mutex::new(());

fn config_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .join("config")
}

/// Goal that uniquely matches `variant_calling_germline` (mirrors the
/// single-threaded determinism gate). The compatibility engine fires
/// `prove()` calls during planning, so the substrate accumulates rows.
fn variant_calling_goal() -> GoalSpec {
    GoalSpec {
        edam_data: "data:3498".into(),
        edam_format: Some("format:3016".into()),
        modifiers: std::collections::BTreeMap::new(),
        source_prose: Some("Variant calls in VCF.".into()),
        confidence: 0.92,
    }
}

fn hash_composition(result: &composer::CompositionResult) -> String {
    let s = format!("{:#?}", result);
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    hex::encode(hasher.finalize())
}

/// Two sessions composing concurrently on separate threads each drain
/// only their own substrate decisions, and each composition is
/// byte-identical to a serial baseline (so the session scoping does not
/// perturb the deterministic-output contract).
#[test]
fn concurrent_compose_isolates_substrate_per_session() {
    let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let config = config_root();
    let atoms =
        AtomRegistry::load_from_dir(&config.join("stage-atoms")).expect("load atom registry");
    let archetypes =
        ArchetypeRegistry::load_from_dir(&config.join("archetypes")).expect("load archetypes");

    // Serial baseline. Establishes the byte-stable composition the
    // concurrent runs must reproduce. `compose()` is unscoped (records
    // into the default bucket); we don't assert on its substrate rows, so
    // clear the default bucket afterward to keep this binary's later
    // unscoped drain hermetic.
    let baseline = composer::compose(
        &variant_calling_goal(),
        "bioinformatics",
        &atoms,
        &archetypes,
    )
    .expect("baseline compose succeeds");
    let baseline_hash = hash_composition(&baseline);
    let _ = decision_substrate::drain();

    // Unique session ids so other tests in this binary can't collide on
    // the shared (process-wide) substrate map.
    let tag = std::process::id();
    let sess_a = format!("concurrent-compose-A-{tag}");
    let sess_b = format!("concurrent-compose-B-{tag}");
    let _ = decision_substrate::drain_session(&sess_a);
    let _ = decision_substrate::drain_session(&sess_b);

    // A barrier forces both compose calls to overlap so a shared-buffer
    // implementation would interleave their `record()` calls.
    let barrier = Arc::new(Barrier::new(2));

    let run = |session: String| {
        let atoms = atoms.clone();
        let archetypes = archetypes.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            let out = composer::compose_with_modalities_full(
                &variant_calling_goal(),
                "bioinformatics",
                &atoms,
                &archetypes,
                &[],
                None,
                None,
                Some(session.as_str()),
            )
            .expect("scoped compose succeeds");
            // Drain this session's bucket explicitly (the emit-time
            // session-isolated writer mirrors this call).
            let decisions = decision_substrate::drain_session(&session);
            (hash_composition(&out.composition), decisions)
        })
    };

    let ha = run(sess_a.clone());
    let hb = run(sess_b.clone());
    let (hash_a, decisions_a) = ha.join().expect("thread A joins");
    let (hash_b, decisions_b) = hb.join().expect("thread B joins");

    // Compositions are byte-identical to the serial baseline.
    assert_eq!(
        hash_a, baseline_hash,
        "session A's concurrent composition diverged from the serial baseline"
    );
    assert_eq!(
        hash_b, baseline_hash,
        "session B's concurrent composition diverged from the serial baseline"
    );

    // Both sessions recorded substrate rows (the engine fires prove()
    // during planning). If this regresses to zero the isolation test
    // below would be vacuous, so assert it explicitly.
    assert!(
        !decisions_a.is_empty(),
        "session A recorded no substrate rows — the test can't prove isolation"
    );
    assert!(
        !decisions_b.is_empty(),
        "session B recorded no substrate rows — the test can't prove isolation"
    );

    // Cross-contamination guard: every UnificationAttempted/Succeeded/
    // Failed row carries port ids; the two sessions compose the same
    // goal, so a row's *content* can't distinguish them. Instead assert
    // the bucket boundaries themselves: draining A then B yields disjoint
    // ownership — neither bucket grew from the other thread's pushes.
    // The strongest available check is total-count conservation: a serial
    // single-session run records exactly `expected` rows; each concurrent
    // session must record the same count (a shared buffer would inflate
    // the first-drained session and starve the second).
    // `compose_with_modalities_full` enters the matching
    // `enter_session(serial_session)` scope internally (see
    // `composer::dispatch`), so its rows route into `serial_session`.
    let serial_session = format!("serial-baseline-{tag}");
    let _ = decision_substrate::drain_session(&serial_session);
    let _ = composer::compose_with_modalities_full(
        &variant_calling_goal(),
        "bioinformatics",
        &atoms,
        &archetypes,
        &[],
        None,
        None,
        Some(serial_session.as_str()),
    )
    .expect("serial scoped compose succeeds");
    let serial_rows = decision_substrate::drain_session(&serial_session).len();

    assert_eq!(
        decisions_a.len(),
        serial_rows,
        "session A's row count ({}) differs from the serial single-session count ({}) — \
         cross-session contamination",
        decisions_a.len(),
        serial_rows
    );
    assert_eq!(
        decisions_b.len(),
        serial_rows,
        "session B's row count ({}) differs from the serial single-session count ({}) — \
         cross-session contamination",
        decisions_b.len(),
        serial_rows
    );

    // Buckets are fully drained — nothing leaks into a later unscoped
    // merge-all drain elsewhere in this binary.
    assert!(decision_substrate::drain_session(&sess_a).is_empty());
    assert!(decision_substrate::drain_session(&sess_b).is_empty());
}

/// Sanity: a plain unscoped `record` + `drain` still round-trips on the
/// default bucket (the path the in-crate property tests and the legacy
/// emit-time writer use).
#[test]
fn unscoped_record_drain_round_trips_default_bucket() {
    let _guard = SUBSTRATE_GUARD.lock().unwrap_or_else(|e| e.into_inner());
    let tag = std::process::id();
    let marker = format!("unscoped-marker-{tag}");
    // Clear the default bucket (unscoped drain merges all buckets, but
    // no scoped buckets should exist at this point in this binary).
    let _ = decision_substrate::drain();
    decision_substrate::record(VerifierDecision::UnificationAttempted {
        id: marker.clone(),
        timestamp: decision_substrate::timestamp(),
        producer_port: "p".into(),
        consumer_port: "c".into(),
        ctx_hash: "h".into(),
    });
    let drained = decision_substrate::drain();
    assert!(
        drained.iter().any(|e| matches!(
            e,
            VerifierDecision::UnificationAttempted { id, .. } if *id == marker
        )),
        "unscoped record/drain lost the default-bucket marker row"
    );
}
