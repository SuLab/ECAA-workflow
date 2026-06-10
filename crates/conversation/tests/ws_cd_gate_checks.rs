//! WS-C/WS-D build-gate checks (D7h/L15 + D7i/M16).
//!
//! These are doc/config-as-contract gates over committed build files:
//!   * the `lint` Makefile target must verify workspace-hack is in sync
//!     (`cargo hakari verify`), so a stale workspace-hack cannot silently
//!     rot (L15);
//!   * whenever `crates/workspace-hack/Cargo.toml` pins tokio `"full"`,
//!     an explicit `# M16:` justification comment must accompany it
//!     (outside the auto-managed HAKARI SECTION) so a future accidental
//!     over-broad `full` from a member that does not need it is caught
//!     (M16).
//!
//! Kept in a standalone top-level test file so it compiles independently
//! of any other WS-D doc-reconcile suite.

use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crates/")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

#[test]
fn lint_target_runs_hakari_verify() {
    let mk = fs::read_to_string(repo_root().join("Makefile")).expect("read Makefile");
    assert!(
        mk.contains("hakari verify") || mk.contains("hakari generate --diff"),
        "Makefile `lint` target must verify workspace-hack is in sync \
         (cargo hakari verify) (L15)."
    );
}

#[test]
fn workspace_hack_tokio_full_is_justified() {
    let wh = fs::read_to_string(repo_root().join("crates/workspace-hack/Cargo.toml"))
        .expect("read workspace-hack Cargo.toml");
    if wh.contains("\"full\"") {
        assert!(
            wh.contains("# M16:"),
            "workspace-hack pins tokio `full`; add a `# M16:` comment (outside the HAKARI \
             SECTION) justifying it, or narrow the member feature + regenerate hakari (M16)."
        );
    }
}
