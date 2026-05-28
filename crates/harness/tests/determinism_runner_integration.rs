//! Phase C8 — integration tests for `DeterminismRunner`.
//!
//! Test taxonomy:
//!
//! **Pure unit tests (always run):**
//! - `errors_when_module_missing` — non-existent stage_id → `Errored`.
//! - `errors_when_no_figure_ids` — empty `figure_ids` → `Errored`.
//!
//! **Live spawn tests (marked `#[ignore]`):**
//! - `passes_when_module_is_deterministic` — a synthetic Python module that
//!   always writes an identical 1x1 PNG → `Passed`. Requires `python` on PATH.
//! - `fails_with_byte_diff_when_nondeterministic` — synthetic module using
//!   `time.time_ns()` as a seed so each render produces different bytes →
//!   `Failed` with byte-diff statistics. Requires `python` + `Pillow` on PATH.
//!
//! The `#[ignore]` tests are un-ignored when the CI
//! environment is guaranteed to have a compatible Python + Pillow install.
//!
//! Run pure tests:
//! cargo test -p scripps-workflow-harness determinism_runner
//!
//! Run live spawn tests (requires python on PATH):
//! cargo test -p scripps-workflow-harness determinism_runner -- --ignored

use scripps_workflow_harness::renderer_validators::DeterminismRunner;
use scripps_workflow_harness::validators::{ValidatorOutcome, ValidatorRunner};
use std::fs;
use tempfile::TempDir;

// ── Pure unit tests ─────────────────────────────────────────────────────────

/// Passing a stage_id whose module file does not exist must return
/// `ValidatorOutcome::Errored` with a message that mentions "not found".
#[test]
fn errors_when_module_missing() {
    let tmp = TempDir::new().unwrap();
    let runner = DeterminismRunner {
        stage_id: "absolutely_nonexistent_stage".into(),
        figure_ids: vec!["some_fig".into()],
        module_path_override: None,
        // Use a fresh tmpdir as the runtime root so we're certain the
        // conventional path `lib/plotting/stages/_generated/...` won't
        // resolve to any real file.
        runtime_root_override: Some(tmp.path().to_path_buf()),
    };
    let outcome = runner.run(tmp.path());
    assert!(
        matches!(&outcome, ValidatorOutcome::Errored { reason } if reason.contains("not found")),
        "expected Errored {{ reason: '...not found...' }}, got {:?}",
        outcome
    );
}

/// When `figure_ids` is empty the runner must return `Errored` before
/// attempting to spawn Python — it cannot render without a function name.
#[test]
fn errors_when_no_figure_ids() {
    let tmp = TempDir::new().unwrap();
    // Write a placeholder Python file so the module-exists check passes.
    let module_file = tmp.path().join("placeholder.py");
    fs::write(&module_file, b"# placeholder").unwrap();

    let runner = DeterminismRunner {
        stage_id: "some_stage".into(),
        figure_ids: vec![], // empty — runner must error before spawning
        module_path_override: Some(module_file),
        runtime_root_override: None,
    };
    let outcome = runner.run(tmp.path());
    assert!(
        matches!(&outcome, ValidatorOutcome::Errored { reason } if reason.contains("no figure_ids")),
        "expected Errored {{ reason: '...no figure_ids...' }}, got {:?}",
        outcome
    );
}

// ── Live spawn tests ─────────────────────────────────────────────────────────

/// A deterministic Python module writes an identical PNG on every invocation.
///
/// The synthetic module creates a 3×3 all-red PNG using only the stdlib
/// `zlib` + raw PNG byte construction (no Pillow dependency) so the test
/// works on hosts with only a bare Python install.
///
/// Guards on `python` being available on PATH; skips when absent so CI
/// hosts without a Python install don't fail.
#[ignore = "requires live python on PATH + bwrap (SWFC_LOCAL_SANDBOX=bubblewrap) \
           for the DeterminismRunner spawn; the in-body python guard is partial \
           (no bwrap-presence skip), so default cargo test can still fail when \
           bwrap is partially installed. Run with `cargo test -- --include-ignored` \
           locally or in the eval-smoke advisory CI workflow"]
#[test]
fn passes_when_module_is_deterministic() {
    if !std::path::Path::new("/usr/bin/bwrap").exists() {
        // The DeterminismRunner uses bwrap when SWFC_LOCAL_SANDBOX=bubblewrap;
        // the spawn itself only needs python, but we guard bwrap too since
        // the live-spawn test family is gated on the sandbox env.
        // Skip gracefully when either is missing.
    }
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
    let tmp = TempDir::new().unwrap();

    // Synthetic deterministic module:
    // - Writes a 3x3 solid-red PNG using only Python stdlib (zlib).
    // - The PNG is byte-identical on every call because it uses no
    // timestamp or random seed.
    let module_src = r#"
import os, zlib, struct

def make_png_3x3_red():
    """Return raw bytes for a 3x3 solid-red 8-bit RGB PNG."""
    def chunk(tag, data):
        c = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', c)

    # IHDR: 3x3, 8-bit, RGB (color type 2), no interlace
    ihdr_data = struct.pack('>IIBBBBB', 3, 3, 8, 2, 0, 0, 0)
    ihdr = chunk(b'IHDR', ihdr_data)

    # Raw image data: 3 rows of [filter_byte=0, R, G, B, R, G, B, R, G, B]
    row = b'\x00' + (b'\xFF\x00\x00' * 3)  # filter byte 0 + 3 red pixels
    raw = row * 3                             # 3 identical rows
    idat = chunk(b'IDAT', zlib.compress(raw))

    iend = chunk(b'IEND', b'')
    sig = b'\x89PNG\r\n\x1a\n'
    return sig + ihdr + idat + iend

def deterministic_figure(out_dir):
    """Render a deterministic 3x3 red PNG to out_dir."""
    os.makedirs(out_dir, exist_ok=True)
    data = make_png_3x3_red()
    with open(os.path.join(out_dir, 'output.png'), 'wb') as f:
        f.write(data)
"#;

    let module_file = tmp.path().join("det_module.py");
    fs::write(&module_file, module_src).unwrap();

    let runner = DeterminismRunner {
        stage_id: "det_stage".into(),
        figure_ids: vec!["deterministic_figure".into()],
        module_path_override: Some(module_file),
        runtime_root_override: None,
    };

    let outcome = runner.run(tmp.path());
    assert_eq!(
        outcome,
        ValidatorOutcome::Passed,
        "deterministic module should produce Passed, got {:?}",
        outcome
    );
}

/// A non-deterministic Python module writes a different PNG each call
/// (seeded by nanosecond timestamp). The runner must detect the divergence
/// and return `Failed` with byte-diff statistics.
///
/// The byte randomisation relies on `time.time_ns()` which is always
/// available in Python 3.7+.
///
/// Guards on `python` being available on PATH; skips when absent so CI
/// hosts without a Python install don't fail.
#[ignore = "requires live python on PATH + bwrap (SWFC_LOCAL_SANDBOX=bubblewrap) \
           for the DeterminismRunner spawn; same caveat as \
           passes_when_module_is_deterministic. Run with \
           `cargo test -- --include-ignored` locally or in the eval-smoke \
           advisory CI workflow"]
#[test]
fn fails_with_byte_diff_when_nondeterministic() {
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
    let tmp = TempDir::new().unwrap();

    // Synthetic non-deterministic module:
    // - Embeds the current nanosecond timestamp as a comment in a PNG
    // text chunk, producing a different file on every call.
    // - Uses only Python stdlib (zlib, time, struct) — no Pillow needed.
    let module_src = r#"
import os, zlib, struct, time

def make_png_with_seed(seed_bytes):
    """Return raw bytes for a 3x3 red PNG with a tEXt chunk embedding seed_bytes."""
    def chunk(tag, data):
        c = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack('>I', len(data)) + tag + data + struct.pack('>I', c)

    ihdr_data = struct.pack('>IIBBBBB', 3, 3, 8, 2, 0, 0, 0)
    ihdr = chunk(b'IHDR', ihdr_data)

    # tEXt chunk carries a unique timestamp so each PNG is different.
    text_payload = b'Comment\x00' + seed_bytes
    text_chunk = chunk(b'tEXt', text_payload)

    row = b'\x00' + (b'\xFF\x00\x00' * 3)
    raw = row * 3
    idat = chunk(b'IDAT', zlib.compress(raw))
    iend = chunk(b'IEND', b'')
    sig = b'\x89PNG\r\n\x1a\n'
    return sig + ihdr + text_chunk + idat + iend

def nondeterministic_figure(out_dir):
    """Render a PNG whose tEXt chunk carries the current nanosecond timestamp."""
    os.makedirs(out_dir, exist_ok=True)
    seed = str(time.time_ns()).encode()
    data = make_png_with_seed(seed)
    with open(os.path.join(out_dir, 'output.png'), 'wb') as f:
        f.write(data)
"#;

    let module_file = tmp.path().join("nondet_module.py");
    fs::write(&module_file, module_src).unwrap();

    let runner = DeterminismRunner {
        stage_id: "nondet_stage".into(),
        figure_ids: vec!["nondeterministic_figure".into()],
        module_path_override: Some(module_file),
        runtime_root_override: None,
    };

    let outcome = runner.run(tmp.path());
    match &outcome {
        ValidatorOutcome::Failed { message } => {
            assert!(
                message.contains("PNG byte-diff"),
                "Failed message should mention 'PNG byte-diff', got: {}",
                message
            );
            // The message must carry both sizes and the first-divergence offset.
            assert!(
                message.contains("bytes"),
                "Failed message should report byte counts, got: {}",
                message
            );
        }
        other => panic!(
            "non-deterministic module should produce Failed, got {:?}",
            other
        ),
    }
}
