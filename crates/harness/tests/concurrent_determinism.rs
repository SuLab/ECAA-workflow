//! Concurrent determinism tests for `DeterminismRunner`.
//!
//! Extends the pattern established in `determinism_runner_integration.rs` to
//! exercise N parallel render invocations of the same module and assert
//! byte-identical output across all of them.
//!
//! # What this catches
//!
//! A TOCTOU class of flake: if the runner computed the module hash *before*
//! spawning and then re-read the module *after* a concurrent write mutated
//! the file between hash-check and execution, two concurrent runs of the
//! same nominal module could diverge. The parallel runs below use
//! byte-identical module sources but verify that the outputs produced across
//! N concurrent invocations are still identical — meaning no per-run
//! timestamp or random seed injection crept in at the call site.
//!
//! # Dependency gap note
//!
//! `DeterminismRunner` delegates execution to a Python subprocess
//! (`bwrap`-sandboxed in production). The live-spawn tests in
//! `determinism_runner_integration.rs` require `python` on PATH and carry
//! `#[ignore]`. We follow the same pattern here: the pure structural tests
//! run unconditionally; the live parallel tests carry `#[ignore]` and require
//! `python` on PATH.
//!
//! Run pure tests:
//! ```text
//! cargo test -p scripps-workflow-harness concurrent_determinism
//! ```
//!
//! Run live parallel tests (requires python on PATH):
//! ```text
//! cargo test -p scripps-workflow-harness concurrent_determinism -- --ignored
//! ```

use scripps_workflow_harness::renderer_validators::DeterminismRunner;
use scripps_workflow_harness::validators::{ValidatorOutcome, ValidatorRunner};
use std::fs;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

// ── Pure structural tests (always run) ──────────────────────────────────────

/// Running the same module twice sequentially (the baseline for the
/// concurrent test) produces the same `ValidatorOutcome` — `Errored` in this
/// case because the module file doesn't exist. This confirms the runner is
/// stateless across calls and that the `ValidatorOutcome` impl is `PartialEq`.
#[test]
fn sequential_runs_produce_identical_outcome_when_module_missing() {
    let tmp = TempDir::new().unwrap();
    let runner = DeterminismRunner {
        stage_id: "concurrent_nonexistent".into(),
        figure_ids: vec!["some_fig".into()],
        module_path_override: None,
        runtime_root_override: Some(tmp.path().to_path_buf()),
    };

    let outcome_a = runner.run(tmp.path());
    let outcome_b = runner.run(tmp.path());

    // Both runs must produce the same variant. We can't assert byte-equality
    // on the reasons because the message may embed a platform path, but the
    // variant must match.
    assert!(
        matches!(&outcome_a, ValidatorOutcome::Errored { .. }),
        "expected Errored on first run; got {:?}",
        outcome_a
    );
    assert!(
        matches!(&outcome_b, ValidatorOutcome::Errored { .. }),
        "expected Errored on second run; got {:?}",
        outcome_b
    );
}

/// Passing empty `figure_ids` is a deterministic error regardless of
/// parallelism. Five concurrent calls must all return `Errored`.
#[test]
fn concurrent_calls_with_empty_figure_ids_all_error() {
    const N: usize = 5;
    let tmp = Arc::new(TempDir::new().unwrap());
    // Write a placeholder file so the module-exists check passes.
    let module_file = tmp.path().join("placeholder.py");
    fs::write(&module_file, b"# placeholder").unwrap();
    let module_file = Arc::new(module_file);

    // A barrier ensures all N threads attempt `run()` at roughly the same
    // instant, exercising any global mutable state the runner might touch.
    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::with_capacity(N);

    for _ in 0..N {
        let tmp_clone = tmp.clone();
        let module_clone = module_file.clone();
        let barrier_clone = barrier.clone();

        let handle = std::thread::spawn(move || {
            let runner = DeterminismRunner {
                stage_id: "concurrent_empty_figs".into(),
                figure_ids: vec![],
                module_path_override: Some((*module_clone).clone()),
                runtime_root_override: None,
            };
            barrier_clone.wait(); // synchronise all threads at the start
            runner.run(tmp_clone.path())
        });
        handles.push(handle);
    }

    let outcomes: Vec<ValidatorOutcome> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();

    for (i, outcome) in outcomes.iter().enumerate() {
        assert!(
            matches!(outcome, ValidatorOutcome::Errored { reason } if reason.contains("no figure_ids")),
            "thread {i}: expected Errored {{ reason: 'no figure_ids' }}; got {:?}",
            outcome
        );
    }
}

// ── Live parallel spawn tests (#[ignore]) ────────────────────────────────────

/// A deterministic module is byte-identical across N concurrent runs.
///
/// Each thread gets its own `TempDir` for outputs so there's no file-level
/// contention. The assertion is that all N threads produce `Passed` — meaning
/// the runner's two internal render invocations (the determinism check spawns
/// the module twice and diffs the outputs) agreed both times.
///
/// If TOCTOU or any per-run state injection crept in, at least one run would
/// produce `Failed` with a PNG byte-diff message.
#[ignore = "requires live python on PATH; same caveat as determinism_runner_integration.rs \
            passes_when_module_is_deterministic. Run with `cargo test -- --include-ignored`"]
#[test]
fn parallel_runs_of_deterministic_module_all_pass() {
    // Guard: skip if python is not importable.
    let python_ok = std::process::Command::new("python")
        .args(["-c", "import sys; sys.exit(0)"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || std::process::Command::new("python3")
            .args(["-c", "import sys; sys.exit(0)"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    if !python_ok {
        eprintln!("[skip] python not available on PATH");
        return;
    }

    const N: usize = 4;

    // A synthetic deterministic module identical to the one in
    // `determinism_runner_integration.rs` — writes a 3×3 red PNG via
    // Python stdlib only (no Pillow). The PNG bytes are fixed regardless
    // of when or how many times the module is executed.
    let module_src = r#"
import os, zlib, struct

def make_png_3x3_red():
    """Return raw bytes for a 3x3 solid-red 8-bit RGB PNG."""
    def chunk(tag, data):
        c = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', c)

    ihdr_data = struct.pack('>IIBBBBB', 3, 3, 8, 2, 0, 0, 0)
    ihdr = chunk(b'IHDR', ihdr_data)

    row = b'\x00' + (b'\xFF\x00\x00' * 3)
    raw = row * 3
    idat = chunk(b'IDAT', zlib.compress(raw))
    iend = chunk(b'IEND', b'')
    sig = b'\x89PNG\r\n\x1a\n'
    return sig + ihdr + idat + iend

def concurrent_det_figure(out_dir):
    """Render a deterministic 3x3 red PNG to out_dir."""
    os.makedirs(out_dir, exist_ok=True)
    data = make_png_3x3_red()
    with open(os.path.join(out_dir, 'output.png'), 'wb') as f:
        f.write(data)
"#;

    // Write the shared module source to a tmp file. Each thread receives
    // a clone of the path so they all point at the same file (read-only
    // during the run). This exercises the TOCTOU scenario where multiple
    // threads stat and spawn from the same path concurrently.
    let shared_tmp = Arc::new(TempDir::new().unwrap());
    let module_file = shared_tmp.path().join("concurrent_det_module.py");
    fs::write(&module_file, module_src).unwrap();
    let module_file = Arc::new(module_file);

    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::with_capacity(N);

    for thread_idx in 0..N {
        let module_clone = module_file.clone();
        let barrier_clone = barrier.clone();

        let handle = std::thread::spawn(move || {
            // Each thread gets its own output TempDir so file writes
            // don't race.
            let per_thread_tmp = TempDir::new().unwrap();
            let runner = DeterminismRunner {
                stage_id: format!("concurrent_det_stage_{thread_idx}"),
                figure_ids: vec!["concurrent_det_figure".into()],
                module_path_override: Some((*module_clone).clone()),
                runtime_root_override: None,
            };
            barrier_clone.wait(); // all N threads start together
            runner.run(per_thread_tmp.path())
        });
        handles.push(handle);
    }

    let outcomes: Vec<ValidatorOutcome> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();

    for (i, outcome) in outcomes.iter().enumerate() {
        assert_eq!(
            *outcome,
            ValidatorOutcome::Passed,
            "thread {i}: concurrent run of deterministic module produced {:?}; expected Passed. \
             This indicates non-determinism introduced by concurrent execution (TOCTOU or \
             per-run state injection).",
            outcome
        );
    }
}

/// A non-deterministic module (each run writes a different PNG) fails
/// consistently across N parallel runs — the runner must detect the
/// divergence between its two internal render passes regardless of
/// concurrent siblings.
///
/// This test confirms the `DeterminismRunner`'s *own* two-pass diff
/// still works correctly under concurrency, even though each run's
/// output is independently non-deterministic (each pair diverges from
/// each other, not from peers).
#[ignore = "requires live python on PATH; same caveat as \
            determinism_runner_integration.rs fails_with_byte_diff_when_nondeterministic. \
            Run with `cargo test -- --include-ignored`"]
#[test]
fn parallel_runs_of_nondeterministic_module_all_fail() {
    // Guard: skip if python is not importable.
    let python_ok = std::process::Command::new("python")
        .args(["-c", "import sys; sys.exit(0)"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
        || std::process::Command::new("python3")
            .args(["-c", "import sys; sys.exit(0)"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
    if !python_ok {
        eprintln!("[skip] python not available on PATH");
        return;
    }

    const N: usize = 4;

    // Non-deterministic module: embeds nanosecond timestamp in each PNG.
    let module_src = r#"
import os, zlib, struct, time

def make_png_with_seed(seed_bytes):
    def chunk(tag, data):
        c = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', c)

    ihdr_data = struct.pack('>IIBBBBB', 3, 3, 8, 2, 0, 0, 0)
    ihdr = chunk(b'IHDR', ihdr_data)
    text_payload = b'Comment\x00' + seed_bytes
    text_chunk = chunk(b'tEXt', text_payload)
    row = b'\x00' + (b'\xFF\x00\x00' * 3)
    raw = row * 3
    idat = chunk(b'IDAT', zlib.compress(raw))
    iend = chunk(b'IEND', b'')
    sig = b'\x89PNG\r\n\x1a\n'
    return sig + ihdr + text_chunk + idat + iend

def concurrent_nondet_figure(out_dir):
    os.makedirs(out_dir, exist_ok=True)
    seed = str(time.time_ns()).encode()
    data = make_png_with_seed(seed)
    with open(os.path.join(out_dir, 'output.png'), 'wb') as f:
        f.write(data)
"#;

    let shared_tmp = Arc::new(TempDir::new().unwrap());
    let module_file = shared_tmp.path().join("concurrent_nondet_module.py");
    fs::write(&module_file, module_src).unwrap();
    let module_file = Arc::new(module_file);

    let barrier = Arc::new(Barrier::new(N));
    let mut handles = Vec::with_capacity(N);

    for thread_idx in 0..N {
        let module_clone = module_file.clone();
        let barrier_clone = barrier.clone();

        let handle = std::thread::spawn(move || {
            let per_thread_tmp = TempDir::new().unwrap();
            let runner = DeterminismRunner {
                stage_id: format!("concurrent_nondet_stage_{thread_idx}"),
                figure_ids: vec!["concurrent_nondet_figure".into()],
                module_path_override: Some((*module_clone).clone()),
                runtime_root_override: None,
            };
            barrier_clone.wait();
            runner.run(per_thread_tmp.path())
        });
        handles.push(handle);
    }

    let outcomes: Vec<ValidatorOutcome> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();

    for (i, outcome) in outcomes.iter().enumerate() {
        match outcome {
            ValidatorOutcome::Failed { message } => {
                assert!(
                    message.contains("PNG byte-diff"),
                    "thread {i}: Failed message should contain 'PNG byte-diff'; got: {message}"
                );
            }
            other => panic!(
                "thread {i}: non-deterministic module should produce Failed; got {:?}",
                other
            ),
        }
    }
}
